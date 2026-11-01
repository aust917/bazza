//! Post-call pipeline: quality transcription -> speaker diarization -> LLM cleanup.
//! Each optional stage fails soft: the meeting still becomes Ready.

use crate::events::{self, ProcessingPayload};
use crate::recorder::{notice, read_wav_as_16k_mono};
use crate::state::AppState;
use hark_capture::dsp::rms_db;
use hark_capture::video::{mux_recording, timeline_path};
use hark_diarize::{assign_clusters, best_match, find_me_cluster, DiarizeOutput, KnownSpeaker, Turn};
use hark_store::{Meeting, MeetingSpeaker, MeetingStatus, NewSegment};
use std::collections::BTreeMap;
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager};

/// Cosine similarity needed to suggest a known speaker (TitaNet same-speaker ~0.6-0.85).
pub const SUGGEST_THRESHOLD: f32 = 0.62;
const LEVEL_HOP_MS: i64 = 250;

/// One-off choices for a (re-)run of the pipeline.
#[derive(Default, Clone)]
pub struct PostOpts {
    /// Quality ASR model id to use instead of the configured one.
    pub asr_model: Option<String>,
    /// Only an explicit Re-transcribe action can replace a selected cloud result.
    pub force_local_asr: bool,
}

pub fn post_process(app: &AppHandle, meeting: Meeting) {
    post_process_with(app, meeting, PostOpts::default())
}

pub fn post_process_with(app: &AppHandle, mut meeting: Meeting, opts: PostOpts) {
    let state = app.state::<AppState>();
    let _busy = state.busy_guard();
    let dir = state.store.recordings_dir(&meeting.id);
    let settings = state.settings.read().clone();
    let meeting_id = meeting.id.clone();
    meeting.error = None;
    let _ = state.store.set_meeting_error(&meeting_id, None);
    let emit = |stage: &'static str, progress: f32, error: Option<String>| {
        let _ = app.emit(events::PROCESSING, ProcessingPayload { meeting_id: meeting_id.clone(), stage, progress, error });
    };

    // 1. Mux video (if any).
    emit("mux", 0.02, None);
    let raw = dir.join("screen.raw.mp4");
    let out = dir.join("screen.mp4");
    let expected_video = meeting.has_video || raw.exists() || timeline_path(&raw).exists();
    let video_result = if raw.exists() {
        state.ffmpeg.as_ref().ok_or_else(|| "FFmpeg is unavailable; original video and timing metadata were kept".to_string())
            .and_then(|ffmpeg| mux_recording(ffmpeg, &raw, &dir.join("mix.wav"), &out))
    } else if expected_video && !out.exists() {
        Err("screen capture produced no video file; audio and capture diagnostics were kept".into())
    } else {
        Ok(())
    };
    // An attempted capture is not a playable result. This also repairs recovery
    // after FFmpeg failed before it created a raw file, or is no longer installed.
    meeting.has_video = out.exists();
    match video_result {
        Ok(()) => {
            if raw.exists() {
                match std::fs::remove_file(&raw) {
                    Ok(()) => { let _ = std::fs::remove_file(timeline_path(&raw)); }
                    // Retrying a surviving raw file must still apply its pauses.
                    Err(e) => log::warn!("could not remove muxed raw video (pause metadata retained): {e}"),
                }
            }
        }
        Err(e) => {
            log::error!("mux failed: {e}");
            // Keep recoverable inputs; publishing raw here could expose
            // paused frames and would restore the mismatched timeline.
            meeting.error = Some(format!("video: {e}"));
            emit("mux", 0.02, meeting.error.clone());
            notice(app, "warning", format!("Video processing failed ({e}). Audio and available capture files were kept for recovery."));
        }
    }
    let mut video_error = meeting.error.clone();

    // 2. Quality transcription.
    emit("transcribe", 0.1, None);
    let tier = settings.tier(&state.hardware);
    let (live_id, mut quality_id, _) = settings.model_ids(&state.catalog, tier);
    if let Some(m) = opts.asr_model.clone() {
        quality_id = m;
    }
    let mix = dir.join("mix.wav");
    let pcm = match read_wav_as_16k_mono(&mix) {
        Ok(p) => p,
        Err(e) => {
            fail(&state, &mut meeting, &emit, format!("read audio: {e}"));
            return;
        }
    };
    // Speaker detection does not need the transcript: run it now, in parallel
    // with whisper (sidecar processes on the CPU; whisper on the GPU where there is one).
    let diar_job = settings.diarize_enabled.then(|| {
        let (app2, dir2) = (app.clone(), dir.clone());
        std::thread::spawn(move || diarize_raw(&app2, &dir2))
    });
    // On CPU-only machines the tier's model may be too slow for a long recording;
    // pick the best downloaded model that finishes in reasonable time (measured per machine).
    let audio_ms = (pcm.len() as i64) * 1000 / 16_000;
    let saved_stt = match crate::stt_stage::saved(&state, &meeting.id) {
        Ok(saved) => saved,
        Err(error) => {
            if let Some(job) = diar_job { let _ = job.join(); }
            fail(&state, &mut meeting, &emit, error);
            return;
        }
    };
    let use_saved = hark_asr::provider::use_saved_transcript(saved_stt.as_ref(), opts.force_local_asr);
    if !use_saved && opts.asr_model.is_none() {
        quality_id = crate::asr_pick::pick(&state, &quality_id, audio_ms);
    }
    let mut transcription_model = quality_id.clone();
    let mut local_transcribed = false;
    let mut captions = if use_saved {
        let saved = saved_stt.as_ref().expect("selected saved transcript");
        if saved.status != hark_asr::provider::TranscriptStatus::Complete {
            let error = format!("stt: {}. Confirmed Soniox text was kept; use Re-transcribe to explicitly replace it with local Whisper", saved.error.as_deref().unwrap_or("Soniox did not finalize"));
            video_error = Some(match video_error { Some(previous) => format!("{previous}; {error}"), None => error });
        }
        saved.captions.iter().filter(|caption| caption.is_final && caption.start_ms >= 0 && caption.end_ms >= caption.start_ms && caption.end_ms <= audio_ms + 100).cloned().collect()
    } else { match state.whisper_or_any(&[&quality_id, &live_id]) {
        Some(engine) => {
            transcription_model = engine.model_id().to_string();
            let t0 = std::time::Instant::now();
            match engine.transcribe_with(&pcm, 0, settings.language.as_deref(), true, 80) {
                Ok(c) => {
                    local_transcribed = true;
                    crate::asr_pick::record_run(&state.store, engine.model_id(), audio_ms, t0.elapsed().as_secs_f32());
                    c
                }
                Err(e) => {
                    if let Some(job) = diar_job { let _ = job.join(); }
                    fail(&state, &mut meeting, &emit, format!("transcription: {e}"));
                    return;
                }
            }
        }
        None => {
            if opts.force_local_asr {
                if let Some(job) = diar_job { let _ = job.join(); }
                fail(&state, &mut meeting, &emit, "No local transcription model available; the previous transcript was kept".into());
                return;
            }
            notice(app, "info", "Transcription skipped: no speech model downloaded. Download one in Settings, then Re-transcribe.".into());
            Vec::new()
        }
    }};
    captions.retain(|c| !c.text.trim().is_empty());

    // 3. Diarization -> speaker labels.
    emit("diarize", 0.45, None);
    let spans: Vec<(i64, i64)> = captions.iter().map(|c| (c.start_ms, c.end_ms)).collect();
    let diar_result = diar_job.map(|h| h.join().unwrap_or_else(|_| Err("diarization thread panicked".into())));
    let (labels, speaker_rows, embeddings) = if let (Some(result), false) = (diar_result, captions.is_empty()) {
        match result.and_then(|out| map_diarization(app, &meeting.id, &dir, &spans, out)) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("diarization skipped: {e}");
                if !e.contains("not downloaded") {
                    notice(app, "warning", format!("Speaker detection failed: {e}"));
                    let _ = state.store.set_meeting_error(&meeting.id, Some(&format!("speakers: {e}")));
                }
                (vec![None; captions.len()], Vec::new(), BTreeMap::new())
            }
        }
    } else {
        (vec![None; captions.len()], Vec::new(), BTreeMap::new())
    };

    let segs: Vec<NewSegment> = captions
        .iter()
        .zip(labels)
        .map(|(c, sp)| NewSegment { start_ms: c.start_ms, end_ms: c.end_ms, speaker: sp, text: c.text.clone() })
        .collect();
    if let Err(e) = state.store.replace_segments(&meeting.id, &segs) {
        fail(&state, &mut meeting, &emit, format!("save transcript: {e}"));
        return;
    }
    if !use_saved {
        if let Some(previous) = saved_stt.filter(|t| t.provider == hark_asr::provider::SttProvider::Soniox) {
            let _ = state.store.set_setting(&format!("stt_previous:{}", meeting.id), &previous);
        }
        let mut transcript = hark_asr::provider::Transcript::new(hark_asr::provider::SttProvider::Local, transcription_model, settings.language.clone());
        transcript.captions = captions.clone();
        transcript.status = if local_transcribed { hark_asr::provider::TranscriptStatus::Complete } else { hark_asr::provider::TranscriptStatus::Incomplete };
        if !local_transcribed { transcript.error = Some("No local speech model downloaded".into()); }
        if let Err(error) = crate::stt_stage::persist(&state, &meeting.id, &transcript) { notice(app, "error", error); }
    }
    let _ = state.store.set_meeting_speakers(&meeting.id, &speaker_rows);
    let _ = state.store.set_setting(&format!("speaker_embeddings:{}", meeting.id), &embeddings);

    // Ready now; cleanup improves it in place.
    meeting.status = MeetingStatus::Ready;
    meeting.error = video_error.clone();
    let _ = state.store.update_meeting(&meeting);
    emit("cleanup", 0.75, None);

    // 4. LLM cleanup, retrieval chunks, summary.
    if settings.cleanup_enabled && !segs.is_empty() {
        run_cleanup(app, &meeting.id);
    }
    if let Err(e) = state.store.rebuild_chunks(&meeting.id) {
        log::error!("rebuild chunks: {e}");
    }
    emit("embed", 0.85, None);
    if let Err(e) = crate::embed_stage::embed_meeting(app, &meeting.id) {
        log::warn!("embedding failed: {e}");
    }
    if settings.summary_enabled && !segs.is_empty() {
        run_summary(app, &meeting.id, None);
    }
    // Later optional stages share the legacy error field. Keep a capture failure
    // visible even when cleanup or summary also reports a problem.
    if let Some(video_error) = video_error {
        let other = state.store.get_meeting(&meeting_id).ok().flatten().and_then(|m| m.error);
        let combined = match other.filter(|error| error != &video_error) {
            Some(other) => format!("{video_error}; {other}"),
            None => video_error,
        };
        let _ = state.store.set_meeting_error(&meeting_id, Some(&combined));
    }
    emit("done", 1.0, None);
    if settings.webhook_enabled {
        match crate::webhook::fire(app, &meeting.id) {
            Ok(status) => log::info!("webhook delivered ({status})"),
            Err(e) => notice(app, "warning", format!("Webhook failed: {e}")),
        }
    }
}
pub use crate::summary_stage::run_summary;

/// Re-run only the cleanup pass on stored segments.
pub fn run_cleanup(app: &AppHandle, meeting_id: &str) {
    let state = app.state::<AppState>();
    let _busy = state.busy_guard();
    let Some(backend) = state.llm() else {
        notice(app, "info", "Transcript cleanup skipped: no language model available. Download one in Settings or point Hark at an endpoint.".into());
        let _ = state.store.set_meeting_error(meeting_id, Some(NO_LLM));
        return;
    };
    let stored = match state.store.segments(meeting_id) {
        Ok(s) => s,
        Err(e) => {
            log::error!("cleanup load: {e}");
            return;
        }
    };
    let raw: Vec<hark_llm::cleanup::RawSegment> = stored.iter().map(|s| (s.id, s.speaker.clone(), s.text.clone())).collect();
    let prog_app = app.clone();
    let mid = meeting_id.to_string();
    let progress = move |p: f32| {
        let _ = prog_app.emit(
            events::PROCESSING,
            ProcessingPayload { meeting_id: mid.clone(), stage: "cleanup", progress: 0.75 + 0.25 * p, error: None },
        );
    };
    match hark_llm::cleanup::run(backend.as_ref(), &raw, progress, || false) {
        Ok(updates) => {
            let _ = state.store.set_clean_text(meeting_id, &updates);
            let _ = app.emit(
                events::PROCESSING,
                ProcessingPayload { meeting_id: meeting_id.to_string(), stage: "done", progress: 1.0, error: None },
            );
        }
        Err(e) => {
            log::error!("cleanup failed: {e}");
            notice(app, "warning", format!("Transcript cleanup failed: {e}"));
            let _ = state.store.set_meeting_error(meeting_id, Some(&format!("cleanup: {e}")));
        }
    }
}

type Diarized = (Vec<Option<String>>, Vec<MeetingSpeaker>, BTreeMap<String, Vec<f32>>);

/// Run the sidecar over `mix.wav` and map its turns onto `spans` (one per
/// transcript segment): per-segment labels, the meeting_speakers rows (with
/// voice-memory suggestions) and the per-label embeddings.
fn diarize_spans(app: &AppHandle, meeting_id: &str, dir: &Path, spans: &[(i64, i64)]) -> Result<Diarized, String> {
    let out = diarize_raw(app, dir)?;
    map_diarization(app, meeting_id, dir, spans, out)
}

/// The expensive half: run the sidecar(s) over mix.wav and consolidate the
/// clusters. Independent of the transcript, so it can run alongside whisper.
fn diarize_raw(app: &AppHandle, dir: &Path) -> Result<DiarizeOutput, String> {
    let state = app.state::<AppState>();
    let mut out = run_diarize_sidecar(&state, &dir.join("mix.wav"))?;
    if out.turns.is_empty() {
        return Err("no speech turns found".into());
    }
    // Long, noisy calls come back over-segmented (hundreds of clusters); fold the
    // slivers into their nearest voice and cap the count at a plausible meeting.
    let raw_clusters = out.embeddings.len();
    hark_diarize::consolidate(&mut out.turns, &mut out.embeddings, hark_diarize::consolidate::MAX_SPEAKERS);
    if out.embeddings.len() < raw_clusters {
        log::info!("diarization: {} clusters consolidated to {}", raw_clusters, out.embeddings.len());
    }
    Ok(out)
}

/// The cheap half: attach clusters to transcript spans, name them, match voices.
fn map_diarization(app: &AppHandle, meeting_id: &str, dir: &Path, spans: &[(i64, i64)], out: DiarizeOutput) -> Result<Diarized, String> {
    let state = app.state::<AppState>();
    let settings = state.settings.read().clone();
    let turns = out.turns;
    let clusters = assign_clusters(spans, &turns);
    let me = me_cluster(dir, &turns);
    let names = cluster_names(&clusters, me, &settings.user_name);
    let labels: Vec<Option<String>> = clusters.iter().map(|c| c.and_then(|c| names.get(&c).cloned())).collect();
    let known: Vec<KnownSpeaker> = state
        .store
        .list_compatible_speakers(hark_store::CURRENT_VOICE_MODEL)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.embedding.is_empty())
        .map(|s| KnownSpeaker { id: s.id, name: s.name, embedding: s.embedding })
        .collect();
    let mut rows = Vec::new();
    let mut embeddings = BTreeMap::new();
    for (cluster, label) in &names {
        let Some(emb) = out.embeddings.get(cluster) else { continue };
        let (suggested_id, suggested_score) = if Some(*cluster) == me {
            (None, None)
        } else {
            match best_match(emb, &known, SUGGEST_THRESHOLD) {
                Some((k, score)) => (Some(k.id.clone()), Some(score)),
                None => (None, None),
            }
        };
        embeddings.insert(label.clone(), emb.clone());
        rows.push(MeetingSpeaker {
            meeting_id: meeting_id.to_string(),
            label: label.clone(),
            speaker_id: None,
            suggested_id,
            suggested_name: None,
            suggested_score,
        });
    }
    Ok((labels, rows, embeddings))
}

/// Re-run only speaker detection over the stored transcript.
pub fn rediarize(app: &AppHandle, meeting_id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let dir = state.store.recordings_dir(meeting_id);
    let emit = |stage: &'static str, progress: f32, error: Option<String>| {
        let _ = app.emit(events::PROCESSING, ProcessingPayload { meeting_id: meeting_id.to_string(), stage, progress, error });
    };
    emit("diarize", 0.45, None);
    let segs = state.store.segments(meeting_id).map_err(|e| e.to_string())?;
    let spans: Vec<(i64, i64)> = segs.iter().map(|s| (s.start_ms, s.end_ms)).collect();
    match diarize_spans(app, meeting_id, &dir, &spans) {
        Ok((labels, rows, embeddings)) => {
            let updates: Vec<(i64, Option<String>)> = segs.iter().zip(labels).map(|(s, l)| (s.id, l)).collect();
            state.store.set_segment_speakers(meeting_id, &updates).map_err(|e| e.to_string())?;
            let _ = state.store.set_meeting_speakers(meeting_id, &rows);
            let _ = state.store.set_setting(&format!("speaker_embeddings:{meeting_id}"), &embeddings);
            let _ = state.store.set_meeting_error(meeting_id, None);
            if let Err(e) = state.store.rebuild_chunks(meeting_id) {
                log::error!("rebuild chunks: {e}");
            }
            let _ = crate::embed_stage::embed_meeting(app, meeting_id);
            emit("done", 1.0, None);
            Ok(())
        }
        Err(e) => {
            let _ = state.store.set_meeting_error(meeting_id, Some(&format!("speakers: {e}")));
            emit("done", 1.0, Some(e.clone()));
            Err(e)
        }
    }
}

/// Rebuild retrieval chunks and their embeddings.
pub fn reembed(app: &AppHandle, meeting_id: &str) -> Result<usize, String> {
    let state = app.state::<AppState>();
    let _busy = state.busy_guard();
    let emit = |stage: &'static str, progress: f32, error: Option<String>| {
        let _ = app.emit(events::PROCESSING, ProcessingPayload { meeting_id: meeting_id.to_string(), stage, progress, error });
    };
    emit("embed", 0.5, None);
    state.store.rebuild_chunks(meeting_id).map_err(|e| e.to_string())?;
    let r = crate::embed_stage::embed_meeting(app, meeting_id);
    emit("done", 1.0, r.as_ref().err().cloned());
    r
}

/// Error text stored when the AI passes were skipped for lack of a language model.
pub const NO_LLM: &str = "no-llm: clean-up and summary will run once the language model is downloaded";

/// Run clean-up and summary for every meeting that was skipped for lack of a
/// language model (called when one finishes downloading).
pub fn run_deferred_ai(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let settings = state.settings.read().clone();
        let todo: Vec<String> = state
            .store
            .list_meetings()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.error.as_deref().is_some_and(|e| e.starts_with("no-llm:")))
            .map(|m| m.id)
            .collect();
        if todo.is_empty() {
            return;
        }
        notice(&app, "info", format!("Language model ready: finishing {} meeting(s).", todo.len()));
        for id in todo {
            let _ = state.store.set_meeting_error(&id, None);
            if settings.cleanup_enabled {
                run_cleanup(&app, &id);
            }
            let _ = state.store.rebuild_chunks(&id);
            let _ = crate::embed_stage::embed_meeting(&app, &id);
            if settings.summary_enabled {
                run_summary(&app, &id, None);
            }
        }
    });
}

/// Embed every meeting that has chunks but no vectors yet.
pub fn run_deferred_embeddings(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        for m in state.store.list_meetings().unwrap_or_default() {
            if m.status == MeetingStatus::Ready && !state.store.has_chunk_vectors(&m.id).unwrap_or(true) {
                let _ = crate::embed_stage::embed_meeting(&app, &m.id);
            }
        }
    });
}

fn fail(state: &AppState, meeting: &mut Meeting, emit: &dyn Fn(&'static str, f32, Option<String>), err: String) {
    log::error!("post-process {}: {err}", meeting.id);
    meeting.status = MeetingStatus::Failed;
    meeting.error = Some(err.clone());
    let _ = state.store.update_meeting(meeting);
    emit("failed", 1.0, Some(err));
}

/// Level series (250 ms hops) from the separate mic/system tracks, then the cluster that is "me".
fn me_cluster(dir: &Path, turns: &[Turn]) -> Option<i32> {
    let mic = level_series(&dir.join("mic.wav"));
    let sys = level_series(&dir.join("sys.wav"));
    if mic.is_empty() {
        return None;
    }
    find_me_cluster(turns, &mic, &sys, 6.0)
}

fn level_series(wav: &Path) -> Vec<(i64, f32)> {
    let Ok(pcm) = read_wav_as_16k_mono(wav) else { return Vec::new() };
    let hop = (16_000 * LEVEL_HOP_MS / 1000) as usize;
    pcm.chunks(hop).enumerate().map(|(i, c)| (i as i64 * LEVEL_HOP_MS, rms_db(c))).collect()
}

/// Cluster -> display label. "Me" cluster gets the user's name; others "Speaker 1..n" by first appearance.
fn cluster_names(clusters: &[Option<i32>], me: Option<i32>, user_name: &str) -> BTreeMap<i32, String> {
    let mut names = BTreeMap::new();
    let mut n = 0;
    for c in clusters.iter().flatten() {
        if names.contains_key(c) {
            continue;
        }
        if Some(*c) == me {
            names.insert(*c, if user_name.trim().is_empty() { "Me".to_string() } else { user_name.trim().to_string() });
        } else {
            n += 1;
            names.insert(*c, format!("Speaker {n}"));
        }
    }
    names
}

/// Run the `hark-diarize` sidecar on a WAV and parse its JSON.
/// Diarize `wav`, splitting long recordings into slices that run as parallel
/// sidecar processes (the speaker-embedding step barely scales across threads
/// but scales perfectly across processes). Slices are stitched and the same
/// voice across slices is merged by `consolidate` in the caller.
fn run_diarize_sidecar(state: &AppState, wav: &Path) -> Result<DiarizeOutput, String> {
    let bin = state.diarize_bin.as_ref().ok_or("hark-diarize sidecar not found")?.clone();
    let (seg, emb) = state.diarize_models().ok_or("speaker models not downloaded")?;
    let duration_ms = hound::WavReader::open(wav)
        .map(|r| r.len() as i64 * 1000 / (r.spec().sample_rate.max(1) as i64 * r.spec().channels.max(1) as i64))
        .map_err(|e| format!("read {}: {e}", wav.display()))?;
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let workers = hark_diarize::shards::workers_for(cpus);
    let slices = hark_diarize::shards::plan(duration_ms, workers);
    let threads = hark_diarize::shards::threads_per_worker(cpus, slices.len());
    log::info!("diarization: {} slice(s) x {threads} threads for {} ms", slices.len(), duration_ms);
    let started = std::time::Instant::now();
    let handles: Vec<_> = slices
        .iter()
        .map(|&(start, end)| {
            let (bin, seg, emb, wav) = (bin.clone(), seg.clone(), emb.clone(), wav.to_path_buf());
            std::thread::spawn(move || -> Result<DiarizeOutput, String> {
                let mut cmd = std::process::Command::new(&bin);
                cmd.arg("--seg").arg(&seg).arg("--emb").arg(&emb).arg("--wav").arg(&wav).arg("--threads").arg(threads.to_string());
                cmd.arg("--start-ms").arg(start.to_string()).arg("--end-ms").arg(end.to_string());
                let out = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| format!("spawn sidecar: {e}"))?;
                if !out.status.success() {
                    return Err(format!("sidecar exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
                }
                let parsed: DiarizeOutput = serde_json::from_slice(&out.stdout).map_err(|e| format!("sidecar output: {e}"))?;
                if let Some(e) = parsed.error {
                    return Err(e);
                }
                Ok(parsed)
            })
        })
        .collect();
    let mut results = Vec::with_capacity(handles.len());
    let mut dim = 0;
    for (h, &range) in handles.into_iter().zip(&slices) {
        let out = h.join().map_err(|_| "sidecar thread panicked".to_string())??;
        dim = dim.max(out.embedding_dim);
        results.push((range, out.turns, out.embeddings));
    }
    let (turns, embeddings) = hark_diarize::shards::stitch(results);
    log::info!("diarization: {} turns, {} clusters in {:.1}s", turns.len(), embeddings.len(), started.elapsed().as_secs_f32());
    Ok(DiarizeOutput { turns, embeddings, embedding_dim: dim, error: None })
}

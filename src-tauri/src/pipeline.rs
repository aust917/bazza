//! Post-call pipeline: quality transcription -> speaker diarization -> LLM cleanup.
//! Each optional stage fails soft: the meeting still becomes Ready.

use crate::events::{self, ProcessingPayload, TranscriptionPhase};
use crate::recorder::read_wav_as_16k_mono;
use crate::state::AppState;
use crate::video_diagnostic::{self, OptionalError, Stage};
use bazza_capture::video::{mux_recording, mux_recording_for_recovery, timeline_path, validate_recording_video};
use bazza_diarize::{assign_clusters, best_match, find_me_cluster, DiarizeOutput, KnownSpeaker, Turn};
use bazza_store::{Meeting, MeetingSpeaker, MeetingStatus, NewSegment};
use std::collections::BTreeMap;
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager};

/// Lower bound only; each profile additionally needs independent enrollment
/// support and its own observed consistency. Cosine is not an identity probability.
pub const SUGGEST_THRESHOLD: f32 = 0.62;
const LEVEL_HOP_MS: i64 = 250;

/// Old unversioned queries have no temporal proof. They may remain in settings,
/// but must never become new meeting observations under the current policy.
pub(crate) fn speaker_embeddings_key(meeting_id: &str) -> String {
    format!("speaker_embeddings:{}:{meeting_id}", bazza_diarize::voice_policy::POLICY_ID)
}

fn clear_speaker_queries(store: &bazza_store::Store, meeting_id: &str) -> Result<(), String> {
    store.set_setting(&speaker_embeddings_key(meeting_id), &BTreeMap::<String, Vec<f32>>::new())
        .map_err(|error| format!("invalidate speaker queries: {error}"))
}

pub fn post_process(app: &AppHandle, meeting: Meeting) {
    post_process_inner(app, meeting, false)
}

/// Interrupted captures lack the clean-stop guarantee of a normal recording.
pub fn post_process_recovery(app: &AppHandle, meeting: Meeting) {
    post_process_inner(app, meeting, true)
}

fn retained_live_rows(
    captions: &[bazza_asr::Caption],
    labels: &[Option<String>],
) -> (Vec<bazza_asr::Caption>, Vec<Option<String>>) {
    captions.iter().zip(labels)
        .filter(|(c, _)| c.is_final && c.start_ms >= 0 && c.end_ms >= c.start_ms && !c.text.trim().is_empty())
        .map(|(caption, label)| (caption.clone(), label.clone()))
        .unzip()
}

fn post_process_inner(app: &AppHandle, mut meeting: Meeting, verify_video: bool) {
    let state = app.state::<AppState>();
    let _busy = state.busy_guard();
    // A queued job may start long after import/Stop. Use current metadata and
    // never recreate a recording directory for a meeting that was removed.
    match state.store.get_meeting(&meeting.id) {
        Ok(Some(current)) => meeting = current,
        Ok(None) => return,
        Err(error) => { log::error!("read queued meeting: {error}"); return; }
    }
    let dir = state.store.recordings_dir(&meeting.id);
    let settings = state.settings.read().clone();
    let meeting_id = meeting.id.clone();
    meeting.error = video_diagnostic::begin_pipeline(&state.store, &meeting_id).unwrap_or_else(|error| {
        log::warn!("processing diagnostics: {error}");
        meeting.error.clone()
    });
    let saved_video_warning = video_diagnostic::has_video_warning(&state.store, &meeting_id).unwrap_or(true);
    let verify_video = verify_video || video_diagnostic::needs_video_verification(&state.store, &meeting_id).unwrap_or(true);
    let emit = |stage: &'static str, progress: f32, error: Option<String>| {
        let _ = app.emit(events::PROCESSING, ProcessingPayload { meeting_id: meeting_id.clone(), stage, progress, error });
    };

    // 1. Mux video (if any).
    emit("mux", 0.02, None);
    let raw = dir.join("screen.raw.mp4");
    let out = dir.join("screen.mp4");
    let expected_video = saved_video_warning || meeting.has_video || raw.exists() || timeline_path(&raw).exists();
    let video_result = if raw.exists() {
        state.ffmpeg.as_ref().ok_or_else(|| "FFmpeg is unavailable; original video and timing metadata were kept".to_string())
            .and_then(|ffmpeg| {
                let mux = if verify_video { mux_recording_for_recovery } else { mux_recording };
                mux(ffmpeg, &raw, &dir.join("mix.wav"), &out)
            })
    } else if expected_video && !out.exists() {
        Err("screen capture produced no video file; audio and capture diagnostics were kept".into())
    } else if verify_video && out.exists() {
        state.ffmpeg.as_ref().ok_or_else(|| "FFmpeg is unavailable; recovered video could not be verified".to_string())
            .and_then(|ffmpeg| validate_recording_video(ffmpeg, &out))
    } else {
        Ok(())
    };
    // An attempted capture is not a playable result. This also repairs recovery
    // after FFmpeg failed before it created a raw file, or is no longer installed.
    // An existing failed recovery artifact is retained but must not be exposed
    // as healthy video merely because its filename exists.
    meeting.has_video = out.exists() && (!verify_video || video_result.is_ok()
        || state.ffmpeg.as_ref().is_some_and(|ffmpeg| validate_recording_video(ffmpeg, &out).is_ok()));
    match video_result {
        Ok(()) => {
            if verify_video && out.exists() {
                match video_diagnostic::clear_verified_video(&state.store, &meeting_id) {
                    Ok(error) => meeting.error = error,
                    Err(error) => log::warn!("save verified video diagnostics: {error}"),
                }
            }
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
            meeting.error = video_diagnostic::record_video_failure(&state.store, &meeting_id, &e)
                .unwrap_or_else(|error| {
                    log::warn!("save video diagnostics: {error}");
                    let warning = format!("video: {e}");
                    let _ = state.store.set_meeting_error(&meeting_id, Some(&warning));
                    Some(warning)
                });
            emit("mux", 0.02, meeting.error.clone());
            crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.videoProcessingFailed", format!("Video processing failed ({e}). Audio and available capture files were kept for recovery.")).with_detail(e.to_string()));
        }
    }

    // Bot-only recordings never silently fall back to recognizing the mix,
    // including recovery and a recording with no confirmed live rows.
    if crate::bot_supplement::marker_exists(&dir) {
        emit("supplement", 0.1, None);
        let published = match crate::transcript_versions::supplement_legacy(app, &meeting_id) {
            Ok(published) => published,
            Err(error) => {
                log::warn!("bot supplement: {error:?}");
                let _ = video_diagnostic::set_optional(&state.store, &meeting_id, Stage::Pipeline,
                    Some(OptionalError::Failure("The complete transcript is unavailable. Original bot subtitles were kept; create a new combined version to retry.".into())));
                false
            }
        };
        if !published {
            let _ = video_diagnostic::set_optional(&state.store, &meeting_id, Stage::Pipeline,
                Some(OptionalError::Failure("The transcript is incomplete. Original bot subtitles and the partial combined version remain available.".into())));
        }
        meeting.status = MeetingStatus::Ready;
        meeting.error = video_diagnostic::current_error(&state.store, &meeting_id).unwrap_or(meeting.error);
        let _ = state.store.update_processing_state(&meeting);
        if published {
            if settings.cleanup_enabled { run_cleanup_stage(app, &meeting_id, false); }
            let _ = state.store.rebuild_chunks(&meeting_id);
            if let Err(error) = crate::embed_stage::embed_meeting(app, &meeting_id) { log::warn!("embedding: {error}"); }
            if settings.summary_enabled { run_summary(app, &meeting_id, None); }
        }
        emit("done", 1.0, None);
        return;
    }

    // 2. Retain completed live text, or run recognition when it is needed.
    let saved_stt = match crate::stt_stage::saved(&state, &meeting.id) {
        Ok(saved) => saved,
        Err(error) => {
            fail(&state, &mut meeting, &emit, error);
            return;
        }
    };
    let use_saved = bazza_asr::provider::use_saved_transcript(saved_stt.as_ref(), false);
    if let Some(saved) = saved_stt.as_ref().filter(|saved| use_saved
        && saved.provider == bazza_asr::provider::SttProvider::Soniox
        && saved.status != bazza_asr::provider::TranscriptStatus::Complete) {
        let error = format!("stt: {}. Confirmed Soniox text was kept. You can create a separate local Whisper version.", saved.error.as_deref().unwrap_or("Soniox did not finalize"));
        let _ = video_diagnostic::set_optional(&state.store, &meeting_id, Stage::Pipeline, Some(OptionalError::Failure(error)));
    }
    let initial_published = match state.store.has_published_initial_live(&meeting.id) {
        Ok(published) => published,
        Err(error) => {
            fail(&state, &mut meeting, &emit, format!("read published transcript: {error}"));
            return;
        }
    };
    let local_live = saved_stt.as_ref().filter(|_| use_saved)
        .and_then(|saved| saved.retained_local_live_labels().map(|labels| (saved, labels)));
    let published_rows = if initial_published {
        match state.store.segments(&meeting.id) {
            Ok(rows) => Some(rows),
            Err(error) => {
                fail(&state, &mut meeting, &emit, format!("read published segments: {error}"));
                return;
            }
        }
    } else {
        if let Err(error) = clear_speaker_queries(&state.store, &meeting_id) {
            fail(&state, &mut meeting, &emit, error);
            return;
        }
        None
    };
    // Participant tracks already identify the speaker. Ordinary live captions
    // carry only text/timing, so they can use the same diarizer as a new pass.
    let named_live = published_rows.as_ref().map_or_else(
        || local_live.is_some_and(|(_, labels)| labels.iter().any(Option::is_some)),
        |rows| rows.iter().any(|row| row.speaker.is_some()),
    );
    let tier = settings.tier(&state.hardware);
    let (live_id, mut quality_id, _) = settings.model_ids(&state.catalog, tier);
    let mix = dir.join("mix.wav");
    let pcm = if local_live.is_some() || initial_published {
        Vec::new()
    } else {
        events::emit_transcription(app, &meeting_id, TranscriptionPhase::Reading, None, None);
        match read_wav_as_16k_mono(&mix) {
            Ok(p) => p,
            Err(e) => {
                fail(&state, &mut meeting, &emit, format!("read audio: {e}"));
                return;
            }
        }
    };
    // Speaker detection does not need the transcript: run it now, in parallel
    // with whisper (sidecar processes on the CPU; whisper on the GPU where there is one).
    let diar_job = (!named_live && settings.diarize_enabled).then(|| {
        let (app2, dir2) = (app.clone(), dir.clone());
        std::thread::spawn(move || diarize_raw(&app2, &dir2))
    });
    // On CPU-only machines the tier's model may be too slow for a long recording;
    // pick the best downloaded model that finishes in reasonable time (measured per machine).
    let audio_ms = if initial_published { meeting.duration_ms } else { (pcm.len() as i64) * 1000 / 16_000 };
    if !use_saved && !initial_published {
        quality_id = crate::asr_pick::pick(&state, &quality_id, audio_ms);
    }
    let mut transcription_model = quality_id.clone();
    let mut local_transcribed = false;
    let mut live_labels = None;
    let mut captions = if let Some(rows) = &published_rows {
        live_labels = named_live.then(|| rows.iter().map(|row| row.speaker.clone()).collect());
        Vec::new()
    } else if let Some((saved, labels)) = local_live {
        let (captions, labels) = retained_live_rows(&saved.captions, labels);
        live_labels = named_live.then_some(labels);
        captions
    } else if let Some(saved) = saved_stt.as_ref().filter(|_| use_saved) {
        saved.captions.iter().filter(|caption| caption.is_final && caption.start_ms >= 0 && caption.end_ms >= caption.start_ms && caption.end_ms <= audio_ms + 100).cloned().collect()
    } else {
        events::emit_transcription(app, &meeting_id, TranscriptionPhase::Loading, None, None);
        match state.whisper_or_any(&[&quality_id, &live_id]) {
            Some(engine) => {
                transcription_model = engine.model_id().to_string();
                let decoding = |percent| events::emit_transcription(
                    app, &meeting_id, TranscriptionPhase::Decoding, Some(engine.model_id()), percent,
                );
                decoding(None);
                let t0 = std::time::Instant::now();
                match bazza_asr::mix_prefix::transcribe_with_progress(
                    &engine, &pcm, settings.language.as_deref(), |percent| decoding(Some(percent)),
                ) {
                    Ok(captions) => {
                        local_transcribed = true;
                        crate::asr_pick::record_run(&state.store, engine.model_id(), audio_ms, t0.elapsed().as_secs_f32());
                        captions
                    }
                    Err(e) => {
                        if let Some(job) = diar_job { let _ = job.join(); }
                        fail(&state, &mut meeting, &emit, format!("transcription: {e}"));
                        return;
                    }
                }
            }
            None => {
                crate::recorder::notice_message(app, "info", crate::ui_message::app("native.transcriptionNoModel", "Transcription skipped: no speech model downloaded. Download one in Settings, then Re-transcribe."));
                Vec::new()
            }
        }
    };
    captions.retain(|c| !c.text.trim().is_empty());

    // 3. Diarization -> speaker labels.
    emit("diarize", 0.45, None);
    let spans: Vec<(i64, i64)> = match &published_rows {
        Some(rows) => rows.iter().map(|row| (row.start_ms, row.end_ms)).collect(),
        None => captions.iter().map(|row| (row.start_ms, row.end_ms)).collect(),
    };
    let diar_result = diar_job.map(|h| h.join().unwrap_or_else(|_| Err("diarization thread panicked".into())));
    let mut diarized = false;
    let (labels, speaker_rows, embeddings) = if let Some(labels) = live_labels {
        let rows = labels.iter().flatten().collect::<std::collections::BTreeSet<_>>().into_iter().map(|label| MeetingSpeaker {
            meeting_id: meeting.id.clone(), label: label.clone(), speaker_id: None,
            suggested_id: None, suggested_name: None, suggested_score: None,
        }).collect();
        (labels, rows, BTreeMap::new())
    } else if let (Some(result), false) = (diar_result, spans.is_empty()) {
        match result.and_then(|out| map_diarization(app, &meeting.id, &dir, &spans, out)) {
            Ok(d) => { diarized = true; d }
            Err(e) => {
                log::warn!("diarization skipped: {e}");
                if !e.contains("not downloaded") {
                    crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.speakerDetectionFailed", format!("Speaker detection failed: {e}")).with_detail(e.to_string()));
                    let _ = video_diagnostic::set_optional(&state.store, &meeting.id, Stage::Speakers, Some(OptionalError::Failure(format!("speakers: {e}"))));
                }
                (vec![None; spans.len()], Vec::new(), BTreeMap::new())
            }
        }
    } else {
        (vec![None; spans.len()], Vec::new(), BTreeMap::new())
    };

    let query_key = speaker_embeddings_key(&meeting.id);
    let speaker_result = if let Some(rows) = &published_rows {
        if diarized {
            let updates: Vec<_> = rows.iter().zip(&labels).map(|(row, label)| (row.id, label.clone())).collect();
            state.store.apply_initial_live_speakers(&meeting.id, &updates, &speaker_rows, &query_key, &embeddings)
        } else if named_live {
            // Recovery may revisit a previously enriched meeting. Keep its
            // voice evidence and confirmed profile links in that case.
            state.store.meeting_speakers(&meeting.id).and_then(|current| {
                if current.is_empty() { state.store.set_meeting_speakers(&meeting.id, &speaker_rows) } else { Ok(()) }
            })
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
    if let Err(error) = speaker_result {
        fail(&state, &mut meeting, &emit, format!("save live speaker evidence: {error}"));
        return;
    }

    let segs: Vec<NewSegment> = captions
        .iter()
        .zip(labels)
        .map(|(c, sp)| NewSegment { start_ms: c.start_ms, end_ms: c.end_ms, speaker: sp, text: c.text.clone() })
        .collect();
    if !initial_published {
        if let Err(e) = state.store.replace_segments(&meeting.id, &segs) {
            fail(&state, &mut meeting, &emit, format!("save transcript: {e}"));
            return;
        }
    }
    if !use_saved && !initial_published {
        let mut transcript = bazza_asr::provider::Transcript::new(bazza_asr::provider::SttProvider::Local, transcription_model, settings.language.clone());
        transcript.captions = captions.clone();
        transcript.status = if local_transcribed { bazza_asr::provider::TranscriptStatus::Complete } else { bazza_asr::provider::TranscriptStatus::Incomplete };
        if !local_transcribed { transcript.error = Some("No local speech model downloaded".into()); }
        if let Err(error) = crate::stt_stage::persist(&state, &meeting.id, &transcript) { crate::recorder::notice_message(app, "error", crate::ui_message::app("native.transcriptSaveFailed", error.clone()).with_detail(error)); }
    }
    if !initial_published {
        if let Err(error) = state.store.set_meeting_speakers(&meeting.id, &speaker_rows)
            .and_then(|_| state.store.set_setting(&query_key, &embeddings)) {
            fail(&state, &mut meeting, &emit, format!("save speaker evidence: {error}"));
            return;
        }
    }

    // Ready now; cleanup improves it in place.
    meeting.status = MeetingStatus::Ready;
    meeting.error = video_diagnostic::current_error(&state.store, &meeting_id).unwrap_or_else(|error| {
        log::warn!("read processing diagnostics: {error}");
        meeting.error.clone()
    });
    let _ = state.store.update_processing_state(&meeting);
    // 4. LLM cleanup, retrieval chunks, summary.
    if settings.cleanup_enabled && (initial_published || !segs.is_empty()) {
        emit("cleanup", 0.75, None);
        run_cleanup_stage(app, &meeting.id, false);
    }
    if let Err(e) = state.store.rebuild_chunks(&meeting.id) {
        log::error!("rebuild chunks: {e}");
    }
    emit("embed", 0.85, None);
    if let Err(e) = crate::embed_stage::embed_meeting(app, &meeting.id) {
        log::warn!("embedding failed: {e}");
    }
    if settings.summary_enabled && (initial_published || !segs.is_empty()) {
        run_summary(app, &meeting.id, None);
    }
    emit("done", 1.0, None);
    if settings.webhook_enabled {
        match crate::webhook::fire(app, &meeting.id) {
            Ok(status) => log::info!("webhook delivered ({status})"),
            Err(e) => crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.webhookFailed", format!("Webhook failed: {e}")).with_detail(e.to_string())),
        }
    }
}
pub use crate::summary_stage::run_summary;

/// Re-run only the cleanup pass on stored segments.
pub fn run_cleanup(app: &AppHandle, meeting_id: &str) {
    run_cleanup_stage(app, meeting_id, true);
}

fn run_cleanup_stage(app: &AppHandle, meeting_id: &str, standalone: bool) {
    let state = app.state::<AppState>();
    let _busy = state.busy_guard();
    let Some(backend) = state.llm() else {
        crate::recorder::notice_message(app, "info", crate::ui_message::app("native.cleanupNoModel", "Transcript cleanup skipped: no language model available. Download one in Settings or point Bazza at an endpoint."));
        let _ = video_diagnostic::set_optional(&state.store, meeting_id, Stage::Cleanup, Some(OptionalError::MissingLanguageModel));
        return;
    };
    let stored = match state.store.segments(meeting_id) {
        Ok(s) => s,
        Err(e) => {
            log::error!("cleanup load: {e}");
            return;
        }
    };
    let raw: Vec<bazza_llm::cleanup::RawSegment> = stored.iter().map(|s| {
        let text = if s.clean_text_manual { s.clean_text.as_deref().unwrap_or(&s.text) } else { &s.text };
        (s.id, s.speaker.clone(), text.to_string())
    }).collect();
    let prog_app = app.clone();
    let mid = meeting_id.to_string();
    let progress = move |p: f32| {
        let _ = prog_app.emit(
            events::PROCESSING,
            ProcessingPayload { meeting_id: mid.clone(), stage: "cleanup", progress: 0.75 + 0.25 * p, error: None },
        );
    };
    let result = bazza_llm::cleanup::run(backend.as_ref(), &raw, progress, || false)
        .map_err(|error| error.to_string())
        .and_then(|updates| state.store.set_clean_text(meeting_id, &updates).map_err(|error| error.to_string()));
    match &result {
        Ok(()) => {
            let _ = video_diagnostic::set_optional(&state.store, meeting_id, Stage::Cleanup, None);
        }
        Err(e) => {
            log::error!("cleanup failed: {e}");
            crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.cleanupFailed", format!("Transcript cleanup failed: {e}")).with_detail(e.clone()));
            let _ = video_diagnostic::set_optional(&state.store, meeting_id, Stage::Cleanup, Some(OptionalError::Failure(format!("cleanup: {e}"))));
        }
    }
    if standalone {
        let _ = app.emit(
            events::PROCESSING,
            ProcessingPayload { meeting_id: meeting_id.to_string(), stage: "done", progress: 1.0, error: result.err() },
        );
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

/// Speaker labels for a user-requested alternate without changing the meeting's
/// speaker profiles, embeddings, or canonical segments.
pub(crate) fn label_version_rows(app: &AppHandle, meeting_id: &str, dir: &Path, rows: &mut [NewSegment]) {
    let spans: Vec<_> = rows.iter().map(|row| (row.start_ms, row.end_ms)).collect();
    match diarize_spans(app, meeting_id, dir, &spans) {
        Ok((labels, _, _)) => {
            for (row, label) in rows.iter_mut().zip(labels) {
                row.speaker = label;
            }
        }
        Err(error) => log::warn!("alternate transcript speaker detection: {error}"),
    }
}

/// The expensive half: run the sidecar(s) over mix.wav and consolidate the
/// clusters. Independent of the transcript, so it can run alongside whisper.
struct RawDiarization {
    output: DiarizeOutput,
    /// None retains a constituent without an embedding: it cannot verify a name.
    constituents: BTreeMap<i32, Vec<Option<Vec<f32>>>>,
    /// Nemotron requires versioned observations tied to the original IDs. Legacy
    /// enrollment/meeting output keeps its previous compatibility path.
    required_evidence: Option<RequiredVoiceEvidence>,
}

struct RequiredVoiceEvidence {
    audio_samples: u64,
    sources: BTreeMap<i32, Vec<bazza_diarize::voice_evidence::SourceInterval>>,
    embeddings: BTreeMap<i32, Vec<f32>>,
    groups: BTreeMap<i32, std::collections::BTreeSet<i32>>,
}

fn consolidate_output(mut output: DiarizeOutput, required_samples: Option<u64>) -> RawDiarization {
    let original_turns = output.turns.clone();
    let original_embeddings = output.embeddings.clone();
    bazza_diarize::consolidate(&mut output.turns, &mut output.embeddings, bazza_diarize::consolidate::MAX_SPEAKERS);
    let mut groups = BTreeMap::<i32, std::collections::BTreeSet<i32>>::new();
    // Consolidation changes IDs only, preserving turn count and order. Timestamp
    // lookups would lose coincident/overlapping turns and chains of merges.
    for (original, mapped) in original_turns.iter().zip(&output.turns) {
        groups.entry(mapped.cluster).or_default().insert(original.cluster);
    }
    let constituents = groups.iter().map(|(cluster, ids)| {
        (*cluster, ids.iter().map(|id| original_embeddings.get(id).cloned()).collect())
    }).collect();
    let required_evidence = required_samples.map(|audio_samples| {
        let mut sources = BTreeMap::<i32, Vec<bazza_diarize::voice_evidence::SourceInterval>>::new();
        let at = |ms: i64| (ms.max(0) as u64).saturating_mul(16).min(audio_samples);
        for (index, turn) in original_turns.iter().enumerate() {
            let (start_sample, end_sample) = (at(turn.start_ms), at(turn.end_ms));
            if end_sample > start_sample {
                sources.entry(turn.cluster).or_default().push(bazza_diarize::voice_evidence::SourceInterval {
                    turn_index: index as u32,
                    span: bazza_diarize::voice_evidence::SampleInterval { start_sample, end_sample },
                });
            }
        }
        RequiredVoiceEvidence { audio_samples, sources, embeddings: original_embeddings, groups }
    });
    RawDiarization { output, constituents, required_evidence }
}

fn temporal_voice_checks(
    raw: &RawDiarization, known: &[KnownSpeaker], model_sha256: &str,
) -> Option<BTreeMap<i32, bazza_diarize::voice_policy::VoiceAssessment>> {
    use bazza_diarize::voice_evidence::validate_envelope;
    use bazza_diarize::voice_policy::{assess_cluster, VoiceAssessment, POLICY_ID};
    let required = raw.required_evidence.as_ref()?;
    let envelope = raw.output.voice_evidence.as_ref().filter(|evidence| {
        validate_envelope(evidence, "titanet-small", model_sha256, POLICY_ID).is_ok()
            && evidence.clusters.keys().all(|id| required.sources.contains_key(id))
    });
    Some(required.sources.iter().map(|(cluster, sources)| {
        let assessment = envelope.and_then(|evidence| evidence.clusters.get(cluster)).map(|evidence| {
            assess_cluster(required.embeddings.get(cluster).map(Vec::as_slice), evidence, sources,
                required.audio_samples, raw.output.embedding_dim, known, SUGGEST_THRESHOLD)
        }).unwrap_or_else(VoiceAssessment::missing);
        (*cluster, assessment)
    }).collect())
}

fn group_passes(raw: &RawDiarization, checks: Option<&BTreeMap<i32, bazza_diarize::voice_policy::VoiceAssessment>>, cluster: i32,
    predicate: fn(&bazza_diarize::voice_policy::VoiceAssessment)->bool) -> bool {
    match (&raw.required_evidence, checks) {
        (None, _) => true,
        (Some(required), Some(checks)) => required.groups.get(&cluster).is_some_and(|ids| {
            !ids.is_empty() && ids.iter().all(|id| checks.get(id).is_some_and(predicate))
        }),
        _ => false,
    }
}

fn reusable_group(raw: &RawDiarization, checks: Option<&BTreeMap<i32, bazza_diarize::voice_policy::VoiceAssessment>>, cluster: i32) -> bool {
    group_passes(raw,checks,cluster,bazza_diarize::voice_policy::VoiceAssessment::reusable)
}

fn naming_group(raw: &RawDiarization, checks: Option<&BTreeMap<i32, bazza_diarize::voice_policy::VoiceAssessment>>, cluster: i32) -> bool {
    group_passes(raw,checks,cluster,bazza_diarize::voice_policy::VoiceAssessment::naming_allowed)
}

/// A forced merged average cannot establish identity for an unknown constituent.
fn match_constituents<'a>(parts: &[Option<Vec<f32>>], known: &'a [KnownSpeaker]) -> Option<(&'a KnownSpeaker, f32)> {
    let mut accepted: Option<(&KnownSpeaker, f32)> = None;
    for part in parts {
        let (speaker, score) = best_match(part.as_deref()?, known, SUGGEST_THRESHOLD)?;
        if let Some((previous, minimum)) = accepted {
            if previous.id != speaker.id { return None; }
            accepted = Some((speaker, minimum.min(score)));
        } else {
            accepted = Some((speaker, score));
        }
    }
    accepted
}

fn diarize_raw(app: &AppHandle, dir: &Path) -> Result<RawDiarization, String> {
    let state = app.state::<AppState>();
    let (out, required_samples) = run_diarize_sidecar(&state, &dir.join("mix.wav"))?;
    if out.turns.is_empty() {
        return Err("no speech turns found".into());
    }
    // Bound over-segmented calls, retaining proof for every original cluster.
    // Results within the supported count keep even short/similar participants.
    let raw_clusters = out.embeddings.len();
    let raw = consolidate_output(out, required_samples);
    if raw.output.embeddings.len() < raw_clusters {
        log::info!("diarization: {} clusters consolidated to {}", raw_clusters, raw.output.embeddings.len());
    }
    Ok(raw)
}

/// The cheap half: attach clusters to transcript spans, name them, match voices.
fn map_diarization(app: &AppHandle, meeting_id: &str, dir: &Path, spans: &[(i64, i64)], raw: RawDiarization) -> Result<Diarized, String> {
    let state = app.state::<AppState>();
    let settings = state.settings.read().clone();
    let known: Vec<KnownSpeaker> = state
        .store
        .list_voice_match_profiles(bazza_store::CURRENT_VOICE_MODEL, Some(meeting_id))
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.embedding.is_empty())
        .map(|s| KnownSpeaker { id: s.id, name: s.name, embedding: s.embedding, samples: s.samples })
        .collect();
    let model_sha = state.catalog.get("titanet-small").map(|s| s.sha256.as_str()).unwrap_or("");
    let checks = temporal_voice_checks(&raw, &known, model_sha);
    if let Some(checks) = &checks {
        for (id, check) in checks { log::info!("voice evidence cluster {id}: {:?}", check.status); }
    }
    let out = &raw.output;
    let clusters = assign_clusters(spans, &out.turns);
    // Temporal checks also protect the owner-name shortcut and subsequent voice
    // learning. Missing or conflicting evidence retains anonymous labels.
    let me = me_cluster(dir, &out.turns).filter(|id| raw.constituents.get(id).is_some_and(|parts| parts.len() == 1)
        && naming_group(&raw, checks.as_ref(), *id));
    let names = cluster_names(&clusters, me, &settings.user_name);
    let labels: Vec<Option<String>> = clusters.iter().map(|c| c.and_then(|c| names.get(&c).cloned())).collect();
    let mut rows = Vec::new();
    let mut embeddings = BTreeMap::new();
    for (cluster, label) in &names {
        let parts = raw.constituents.get(cluster).map(Vec::as_slice).unwrap_or_default();
        let reusable = reusable_group(&raw, checks.as_ref(), *cluster);
        let verified = naming_group(&raw,checks.as_ref(),*cluster).then(|| match_constituents(parts, &known)).flatten();
        let (suggested_id, suggested_score) = if Some(*cluster) == me {
            (None, None)
        } else {
            match verified {
                Some((k, score)) => (Some(k.id.clone()), Some(score)),
                None => (None, None),
            }
        };
        // Keep a single unknown voice available for explicit user naming. A
        // forced mixed/unverified average must never become enrollment material.
        if reusable && (parts.len() == 1 || verified.is_some()) {
            if let Some(emb) = out.embeddings.get(cluster).filter(|emb| bazza_diarize::cosine(emb, emb) > 0.0) {
                embeddings.insert(label.clone(), emb.clone());
            }
        }
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
    clear_speaker_queries(&state.store, meeting_id)?;
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
            state.store.set_meeting_speakers(meeting_id, &rows).map_err(|e| e.to_string())?;
            state.store.set_setting(&speaker_embeddings_key(meeting_id), &embeddings).map_err(|e| e.to_string())?;
            let _ = video_diagnostic::set_optional(&state.store, meeting_id, Stage::Speakers, None);
            if let Err(e) = state.store.rebuild_chunks(meeting_id) {
                log::error!("rebuild chunks: {e}");
            }
            let _ = crate::embed_stage::embed_meeting(app, meeting_id);
            emit("done", 1.0, None);
            Ok(())
        }
        Err(e) => {
            let _ = video_diagnostic::set_optional(&state.store, meeting_id, Stage::Speakers, Some(OptionalError::Failure(format!("speakers: {e}"))));
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
            .filter(|m| video_diagnostic::needs_deferred_ai(&state.store, &m.id, m.error.as_deref()).unwrap_or_else(|error| {
                log::warn!("read deferred AI diagnostics: {error}");
                false
            }))
            .map(|m| m.id)
            .collect();
        if todo.is_empty() {
            return;
        }
        crate::recorder::notice_message(&app, "info", crate::ui_message::app("native.modelReady", format!("Language model ready: finishing {} meeting(s).", todo.len())).with_args(serde_json::json!({"count":todo.len()})));
        for id in todo {
            let settings = settings.clone();
            if let Err(error) = crate::processing_jobs::enqueue(&app, id.clone(), move |app| {
                let state = app.state::<AppState>();
                let _busy = state.busy_guard();
                let _ = video_diagnostic::clear_deferred_ai(&state.store, &id);
                if settings.cleanup_enabled { run_cleanup(app, &id); }
                let _ = state.store.rebuild_chunks(&id);
                let _ = crate::embed_stage::embed_meeting(app, &id);
                if settings.summary_enabled { run_summary(app, &id, None); }
            }) {
                log::info!("deferred AI already queued: {error}");
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
                let id = m.id;
                let _ = crate::processing_jobs::enqueue(&app, id.clone(), move |app| {
                    let state = app.state::<AppState>();
                    let _busy = state.busy_guard();
                    let _ = crate::embed_stage::embed_meeting(app, &id);
                });
            }
        }
    });
}

fn fail(state: &AppState, meeting: &mut Meeting, emit: &dyn Fn(&'static str, f32, Option<String>), err: String) {
    log::error!("post-process {}: {err}", meeting.id);
    meeting.status = MeetingStatus::Failed;
    meeting.error = video_diagnostic::set_optional(&state.store, &meeting.id, Stage::Pipeline, Some(OptionalError::Failure(err.clone())))
        .unwrap_or_else(|error| {
            log::warn!("save failed processing diagnostics: {error}");
            Some(err.clone())
        });
    let _ = state.store.update_processing_state(meeting);
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
    let hop = (16_000 * LEVEL_HOP_MS / 1000) as usize;
    bazza_capture::wav::rms_resampled(wav, 16_000, hop).unwrap_or_default()
        .into_iter().enumerate().map(|(index, db)| (index as i64 * LEVEL_HOP_MS, db)).collect()
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

/// Run one continuous Nemotron job, or the explicit legacy parallel-shard path.
/// The returned input length requires native temporal evidence even when a
/// stale sidecar happens to omit the new JSON field.
fn run_diarize_sidecar(state: &AppState, wav: &Path) -> Result<(DiarizeOutput, Option<u64>), String> {
    run_diarize_sidecar_with_pin(state, wav, None)
}

pub(crate) fn service_speaker_clusters(
    state: &crate::state::RuntimeState,
    media: &bazza_service_client::postprocess_media::OwnedPostProcessMedia,
    spans: &[(i64, i64)],
) -> Result<Vec<Option<i32>>, bazza_domain::speaker_result::SpeakerSkipReason> {
    use bazza_domain::speaker_result::SpeakerSkipReason;
    use bazza_service_client::api::postprocess_media::PostProcessMediaKind;
    if state.diarize_bin.is_none()
        || (state.nemotron_model().is_none() && state.diarize_models().is_none())
    {
        return Err(SpeakerSkipReason::ModelUnavailable);
    }
    let audio = media.grant().artifacts.iter()
        .find(|entry| entry.kind == PostProcessMediaKind::AudioMix)
        .ok_or(SpeakerSkipReason::SidecarFailed)?;
    let pin = media.try_clone_file().map_err(|_| SpeakerSkipReason::SidecarFailed)?;
    let (output, samples) = run_diarize_sidecar_with_pin(
        state, Path::new(&audio.private_path), Some(std::sync::Arc::new(pin)),
    ).map_err(|error| {
        log::warn!("service speaker sidecar: {error}");
        SpeakerSkipReason::SidecarFailed
    })?;
    if output.turns.is_empty() {
        return Err(SpeakerSkipReason::NoTurns);
    }
    let raw = consolidate_output(output, samples);
    Ok(assign_clusters(spans, &raw.output.turns))
}

fn run_diarize_sidecar_with_pin(
    state: &crate::state::RuntimeState,
    wav: &Path,
    pin: Option<std::sync::Arc<std::fs::File>>,
) -> Result<(DiarizeOutput, Option<u64>), String> {
    let bin = state.diarize_bin.as_ref().ok_or("bazza-diarize sidecar not found")?.clone();
    if let Some(model) = state.nemotron_model() {
        let reader = hound::WavReader::open(wav).map_err(|e| format!("read WAV: {e}"))?;
        let spec = reader.spec();
        if spec.channels == 0 || spec.sample_rate == 0 { return Err("invalid WAV geometry".into()); }
        let native_frames = (reader.len() as u64).div_ceil(spec.channels as u64);
        let audio_samples = if spec.sample_rate == 16_000 { native_frames } else { (native_frames as f64 / (spec.sample_rate as f64 / 16_000.0)) as u64 };
        drop(reader);
        let runtime = state.nemotron_runtime().ok_or("Nemotron native runtime not found; prepare the application bundle")?;
        // Nemotron can still separate anonymous speakers without a usable voice
        // encoder. Pass its canonical destination even when absent/truncated;
        // the sidecar validates it after producing turns and omits voiceprints.
        let emb_spec = state.catalog.get("titanet-small").ok_or("TitaNet model missing from catalog")?;
        let emb = bazza_models::model_path(&state.models_dir(), emb_spec);
        log::info!("diarization: Nemotron 3, one continuous stream");
        let started = std::time::Instant::now();
        let mut command = std::process::Command::new(&bin);
        command.arg("--backend").arg("nemotron")
            .arg("--nemotron").arg(model).arg("--nemotron-runtime").arg(runtime)
            .arg("--emb").arg(emb).arg("--wav").arg(wav);
        let out = speaker_child_output(command, pin.as_deref())?;
        let parsed = parse_diarize_output(out)?;
        log::info!("diarization: Nemotron {} turns, {} clusters in {:.1}s", parsed.turns.len(), parsed.embeddings.len(), started.elapsed().as_secs_f32());
        return Ok((parsed, Some(audio_samples)));
    }
    // Existing installations can continue recording before explicitly downloading
    // the new model. Only legacy pyannote jobs use independent parallel shards.
    let (seg, emb) = state.diarize_models().ok_or("speaker models not downloaded")?;
    let duration_ms = hound::WavReader::open(wav)
        .map(|r| r.len() as i64 * 1000 / (r.spec().sample_rate.max(1) as i64 * r.spec().channels.max(1) as i64))
        .map_err(|e| format!("read {}: {e}", wav.display()))?;
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let workers = bazza_diarize::shards::workers_for(cpus);
    let slices = bazza_diarize::shards::plan(duration_ms, workers);
    let threads = bazza_diarize::shards::threads_per_worker(cpus, slices.len());
    log::info!("diarization: {} slice(s) x {threads} threads for {} ms", slices.len(), duration_ms);
    let started = std::time::Instant::now();
    let handles: Vec<_> = slices
        .iter()
        .map(|&(start, end)| {
            let (bin, seg, emb, wav) = (bin.clone(), seg.clone(), emb.clone(), wav.to_path_buf());
            let pin = pin.clone();
            std::thread::spawn(move || -> Result<DiarizeOutput, String> {
                let mut cmd = std::process::Command::new(&bin);
                cmd.arg("--seg").arg(&seg).arg("--emb").arg(&emb).arg("--wav").arg(&wav).arg("--threads").arg(threads.to_string());
                cmd.arg("--start-ms").arg(start.to_string()).arg("--end-ms").arg(end.to_string());
                let out = speaker_child_output(cmd, pin.as_deref())?;
                parse_diarize_output(out)
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
    let (turns, embeddings) = bazza_diarize::shards::stitch(results);
    log::info!("diarization: {} turns, {} clusters in {:.1}s", turns.len(), embeddings.len(), started.elapsed().as_secs_f32());
    Ok((DiarizeOutput { turns, embeddings, embedding_dim: dim, error: None, voice_evidence: None }, None))
}

fn speaker_child_output(
    mut command: std::process::Command,
    pin: Option<&std::fs::File>,
) -> Result<std::process::Output, String> {
    let Some(pin) = pin else {
        return command.stdin(std::process::Stdio::null()).output()
            .map_err(|error| format!("spawn sidecar: {error}"));
    };
    #[cfg(not(windows))]
    {
        let _ = pin;
        Err("Service speaker subprocess ownership is unavailable on this platform.".into())
    }
    #[cfg(windows)]
    {
        use std::{io::Read, time::{Duration, Instant}};
        fn drain(reader: impl Read, limit: u64) -> std::io::Result<Vec<u8>> {
            let mut output = Vec::new();
            reader.take(limit + 1).read_to_end(&mut output)?;
            if output.len() as u64 > limit {
                return Err(std::io::Error::other("speaker sidecar output budget exceeded"));
            }
            Ok(output)
        }
        let args = command.get_args()
            .map(|arg| arg.to_str().ok_or("invalid speaker sidecar argument").map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let exe = Path::new(command.get_program());
        let cwd = exe.parent().ok_or("speaker sidecar directory is unavailable")?;
        let mut child = bazza_capture::OwnedMediaChild::spawn_piped_stdout(exe, &args, cwd, &[pin])
            .map_err(|error| format!("spawn pinned speaker sidecar: {error}"))?;
        drop(child.take_stdin());
        let stdout = child.take_stdout().ok_or("missing speaker output")?;
        let stderr = child.take_stderr().ok_or("missing speaker diagnostics")?;
        let stdout = std::thread::spawn(move || drain(stdout, 64 * 1024 * 1024));
        let stderr = std::thread::spawn(move || drain(stderr, 1024 * 1024));
        let deadline = Instant::now() + Duration::from_secs(30 * 60);
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err("speaker sidecar exceeded its output or time budget".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let stdout = stdout.join().map_err(|_| "speaker output reader failed")?
            .map_err(|error| error.to_string())?;
        let stderr = stderr.join().map_err(|_| "speaker diagnostics reader failed")?
            .map_err(|error| error.to_string())?;
        Ok(std::process::Output { status, stdout, stderr })
    }
}

fn parse_diarize_output(out: std::process::Output) -> Result<DiarizeOutput, String> {
    if !out.status.success() {
        return Err(format!("sidecar exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let parsed: DiarizeOutput = serde_json::from_slice(&out.stdout).map_err(|e| format!("sidecar output: {e}"))?;
    if let Some(ref error) = parsed.error { return Err(error.clone()); }
    Ok(parsed)
}

#[cfg(test)]
mod voice_matching_tests {
    use super::*;

    #[test]
    fn retained_live_rows_keep_overlapping_speakers_and_drop_drafts() {
        let caption = |start_ms, end_ms, text: &str, is_final| bazza_asr::Caption {
            start_ms, end_ms, text: text.into(), is_final,
        };
        let (rows, labels) = retained_live_rows(
            &[
                caption(100, 500, "Первая строка", true),
                caption(250, 450, "Черновик", false),
                caption(300, 600, "Вторая строка", true),
                caption(600, 600, "", true),
            ],
            &[Some("Марина".into()), None, Some("Александр".into()), None],
        );
        assert_eq!(rows, [
            caption(100, 500, "Первая строка", true),
            caption(300, 600, "Вторая строка", true),
        ]);
        assert_eq!(labels, [Some(String::from("Марина")), Some(String::from("Александр"))]);
    }

    #[test]
    fn reprocessing_invalidates_only_the_current_policy_queries() {
        let store=bazza_store::Store::open_in_memory().unwrap();
        let observed=BTreeMap::from([("Speaker 1".to_string(),vec![1.0_f32,0.0])]);
        store.set_setting("speaker_embeddings:meeting",&observed).unwrap();
        store.set_setting(&speaker_embeddings_key("meeting"),&observed).unwrap();
        clear_speaker_queries(&store,"meeting").unwrap();
        assert_eq!(store.get_setting::<BTreeMap<String,Vec<f32>>>("speaker_embeddings:meeting").unwrap(),Some(observed));
        assert_eq!(store.get_setting::<BTreeMap<String,Vec<f32>>>(&speaker_embeddings_key("meeting")).unwrap(),Some(BTreeMap::new()));
    }

    fn evidence_output(turns: Vec<Turn>) -> (DiarizeOutput, u64) {
        use bazza_diarize::voice_evidence::*;
        use bazza_diarize::voice_policy::{planner_config, POLICY_ID, PRIMARY_POOL_LIMIT};
        let samples = turns.iter().map(|t| t.end_ms.max(0) as u64 * 16).max().unwrap();
        let mut clusters = BTreeMap::<i32, Vec<SourceInterval>>::new();
        for (index,t) in turns.iter().enumerate() {
            clusters.entry(t.cluster).or_default().push(SourceInterval {turn_index:index as u32,span:SampleInterval {start_sample:t.start_ms as u64*16,end_sample:t.end_ms as u64*16}});
        }
        let evidence = clusters.iter().map(|(id,sources)| {
            let plan=plan_coherence(sources,samples,planner_config()).unwrap();
            let duration=sources.iter().map(|s|s.span.end_sample-s.span.start_sample).sum::<u64>().min(PRIMARY_POOL_LIMIT);
            (*id,ClusterEvidence { recovery:RecoveryEvidence {primary:PrimaryResult::Valid,query_source:QuerySource::Primary,pool_samples:duration,windows:vec![]},
                coherence_windows:plan.selected.into_iter().map(|window|WindowEvidence {window,result:WindowResult::Valid {embedding:FiniteEmbedding::new(vec![1.0,0.0]).unwrap()}}).collect() })
        }).collect();
        let output=DiarizeOutput { turns,embeddings:clusters.keys().map(|id|(*id,vec![1.0,0.0])).collect(),embedding_dim:2,
            voice_evidence:Some(VoiceEvidence {version:EVIDENCE_VERSION,model_id:"titanet-small".into(),model_sha256:"a".repeat(64),policy_id:POLICY_ID.into(),clusters:evidence}),..Default::default() };
        (output,samples)
    }

    #[test]
    fn native_internal_conflict_blocks_name_owner_shortcut_and_reusable_vector() {
        use bazza_diarize::voice_evidence::{FiniteEmbedding,WindowResult};
        let (mut output,samples)=evidence_output(vec![Turn{start_ms:0,end_ms:10000,cluster:0}]);
        output.voice_evidence.as_mut().unwrap().clusters.get_mut(&0).unwrap().coherence_windows[1].result=
            WindowResult::Valid {embedding:FiniteEmbedding::new(vec![0.0,1.0]).unwrap()};
        let raw=consolidate_output(output,Some(samples));
        let profiles=vec![known("A",false),known("B",true)];
        assert!(match_constituents(&raw.constituents[&0],&profiles).is_some(),"aggregate alone would accept A");
        let checks=temporal_voice_checks(&raw,&profiles,&"a".repeat(64)).unwrap();
        assert!(!reusable_group(&raw,Some(&checks),0),"same gate protects suggestions, Me and enrollment");
    }

    #[test]
    fn old_json_cannot_masquerade_as_checked_nemotron_evidence() {
        let (mut output,samples)=evidence_output(vec![Turn{start_ms:0,end_ms:10000,cluster:0}]);
        output.voice_evidence=None;
        let raw=consolidate_output(output.clone(),Some(samples));
        let checks=temporal_voice_checks(&raw,&[known("A",false)],&"a".repeat(64)).unwrap();
        assert!(!reusable_group(&raw,Some(&checks),0));
        let legacy=consolidate_output(output,None);
        assert!(reusable_group(&legacy,None,0),"legacy path is explicit, not inferred from missing JSON");
    }

    #[test]
    fn forced_merge_keeps_the_original_temporal_veto() {
        use bazza_diarize::voice_evidence::{FiniteEmbedding,WindowResult};
        let turns=(0..9).map(|id|Turn{start_ms:id*40000,end_ms:id*40000+if id==8 {7000}else{30000},cluster:id as i32}).collect();
        let (mut output,samples)=evidence_output(turns);
        output.voice_evidence.as_mut().unwrap().clusters.get_mut(&8).unwrap().coherence_windows[1].result=
            WindowResult::Valid {embedding:FiniteEmbedding::new(vec![0.0,1.0]).unwrap()};
        let raw=consolidate_output(output,Some(samples));
        let checks=temporal_voice_checks(&raw,&[known("A",false),known("B",true)],&"a".repeat(64)).unwrap();
        let merged=*raw.required_evidence.as_ref().unwrap().groups.iter().find(|(_,ids)|ids.contains(&8)).unwrap().0;
        assert!(raw.required_evidence.as_ref().unwrap().groups[&merged].len()>1);
        assert!(!reusable_group(&raw,Some(&checks),merged));
    }

    fn known(id: &str, vertical: bool) -> KnownSpeaker {
        let samples = if vertical {
            vec![vec![0.15, 1.0], vec![-0.15, 1.0]]
        } else {
            vec![vec![1.0, 0.15], vec![1.0, -0.15]]
        };
        KnownSpeaker { id: id.into(), name: id.into(), embedding: bazza_diarize::centroid(&samples), samples }
    }

    #[test]
    fn known_dominated_average_cannot_name_its_unknown_constituent() {
        let profiles = vec![known("known", false)];
        let unknown = vec![0.8, 0.6];
        let average = bazza_diarize::centroid(&[vec![20.0, 0.0], unknown.clone()]);
        assert!(best_match(&average, &profiles, SUGGEST_THRESHOLD).is_some());
        assert!(match_constituents(&[Some(vec![1.0, 0.0]), Some(unknown)], &profiles).is_none());
        assert!(match_constituents(&[Some(vec![1.0, 0.0]), None], &profiles).is_none());
    }

    #[test]
    fn a_group_requires_one_identity_for_every_original_voice() {
        let profiles = vec![known("horizontal", false), known("vertical", true)];
        assert!(match_constituents(&[Some(vec![1.0, 0.0]), Some(vec![0.0, 1.0])], &profiles).is_none());
        let (speaker, score) = match_constituents(&[Some(vec![1.0, 0.0]), Some(vec![1.0, 0.05])], &profiles).unwrap();
        assert_eq!(speaker.id, "horizontal");
        assert!(score <= best_match(&[1.0, 0.0], &profiles, SUGGEST_THRESHOLD).unwrap().1);
        assert!(match_constituents(&[], &profiles).is_none());
    }

    #[test]
    fn forced_merge_retains_a_missing_original_embedding_as_unverified() {
        let mut output = DiarizeOutput::default();
        for id in 0..9 {
            output.turns.push(Turn { start_ms: id as i64 * 40_000, end_ms: id as i64 * 40_000 + if id == 8 { 1000 } else { 30_000 }, cluster: id });
            if id != 8 { output.embeddings.insert(id, vec![1.0, 0.0]); }
        }
        let raw = consolidate_output(output, None);
        let merged = raw.constituents.values().find(|parts| parts.len() > 1).unwrap();
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(Option::is_none));
        assert!(match_constituents(merged, &[known("known", false)]).is_none());
        assert_eq!(raw.constituents.len(), 8);
        assert_eq!(raw.output.turns.len(), 9);
    }
}

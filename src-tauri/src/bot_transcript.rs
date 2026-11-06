//! The local, on-demand transcript of the optional participant audio tracks.
//! This artifact is separate from the meeting transcript used by the player.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, BufReader, Read, Write},
    path::Path,
    time::{Duration, Instant},
};

use bazza_asr::{whisper::{AsrError, TranscribeOpts, WhisperBackend, WhisperJob}, Chunker, WhisperEngine};
use bazza_capture::{dsp::Resampler, ASR_HZ};
use bazza_domain::transcript_versions::{
    NewTranscriptVersionSegment, TranscriptPlaybackTiming, TranscriptSegmentProvenance,
    TranscriptSegmentSource, TranscriptParticipantSource, MAX_VERSION_SPEAKER_BYTES,
};
use bazza_domain::MeetingStatus;
use bazza_media_gate::{BotTrackSource, MediaGateError, MediaReadGuard};
use bazza_service_client::RepositoryClient;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::{
    attendee_settings::library_root,
    commands::{blocking, CmdResult},
    recorder_service::ServiceCapture,
    state::{AppState, RuntimeState},
    ui_message::UiMessage,
};

mod alignment;
mod jobs;
pub use jobs::{Jobs, Status as JobStatus};
use jobs::{Job, Stage};

const MANIFEST_LIMIT: u64 = 512 * 1024;
const RESULT_LIMIT: u64 = 8 * 1024 * 1024;
const RESULT_FILE: &str = "participant-transcript.json";
const MAX_TRACK_MS: u64 = 8 * 60 * 60 * 1000;
const MAX_SEGMENTS: usize = 20_000;
const WINDOW_SECONDS: u64 = 30;
const OVERLAP_SECONDS: u64 = 4;
const HALF_OVERLAP_MS: i64 = OVERLAP_SECONDS as i64 * 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Meet,
    Discord,
}

impl Source {
    fn media(self) -> BotTrackSource {
        match self {
            Self::Meet => BotTrackSource::Meet,
            Self::Discord => BotTrackSource::Discord,
        }
    }

    fn timing(self) -> Timing {
        match self {
            Self::Meet => Timing::Approximate,
            Self::Discord => Timing::Relative,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Timing {
    // Both are bot-relative. Meet derives placement from packet timestamps
    // anchored to arrival; Discord counts ticks in its own voice connection.
    Approximate,
    Relative,
}

#[derive(Serialize, Deserialize)]
pub struct BotTranscript {
    timing: Timing,
    partial: bool,
    segments: Vec<BotSegment>,
}

#[derive(Serialize, Deserialize)]
struct BotSegment {
    speaker_id: String,
    speaker_name: Option<String>,
    start_ms: i64,
    end_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playback_start_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playback_end_ms: Option<i64>,
    text: String,
}

#[derive(Serialize)]
pub struct BotTranscriptSource {
    source: Source,
    track_count: usize,
    transcript: Option<BotTranscript>,
}

#[derive(Serialize, Deserialize)]
struct StoredTranscript {
    schema_version: u32,
    #[serde(flatten)]
    transcript: BotTranscript,
}

struct Track {
    id: String,
    name: Option<String>,
    file: String,
}

struct Input {
    tracks: Vec<Track>,
    partial: bool,
    sample_rate: u32,
    duration_ms: u64,
    anchors: alignment::Anchors,
}

fn unavailable() -> UiMessage {
    UiMessage::app(
        "native.botTranscriptUnavailable",
        "The participant audio tracks are unavailable.",
    )
}

fn busy() -> UiMessage {
    UiMessage::app(
        "native.botTranscriptBusy",
        "Wait for recording or processing to finish, then try again.",
    )
}

fn failed() -> UiMessage {
    UiMessage::app(
        "native.botTranscriptFailed",
        "Could not transcribe the participant tracks. Try again.",
    )
}

fn cancelled() -> UiMessage {
    UiMessage::app("native.botTranscriptCancelled", "Participant transcription cancelled.")
}

fn gpu_unavailable() -> UiMessage {
    UiMessage::app("native.botTranscriptGpuUnavailable", "GPU transcription is unavailable. Use a GPU-enabled Bazza build or select a CPU model profile in Settings.")
}

fn valid_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|parsed| !parsed.is_nil() && parsed.to_string() == id)
}

fn may_read(app: &AppHandle, meeting_id: &str) -> CmdResult<bool> {
    if !valid_id(meeting_id) {
        return Ok(false);
    }
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        let lookup = tauri::async_runtime::block_on(repository.meeting(meeting_id.to_owned()))
            .map_err(|_| unavailable())?;
        return Ok(lookup.data.is_some_and(|row| {
            row.ended_at.is_some() && matches!(row.status.as_str(), "ready" | "failed")
        }));
    }
    Ok(app
        .state::<AppState>()
        .store
        .get_meeting(meeting_id)
        .map_err(|_| unavailable())?
        .is_some_and(|row| {
            row.ended_at.is_some()
                && matches!(row.status, MeetingStatus::Ready | MeetingStatus::Failed)
        }))
}

fn optional_file(guard: &MediaReadGuard, source: Source, name: &str) -> CmdResult<Option<File>> {
    match guard.open_bot_track_file(source.media(), name) {
        Ok(file) => Ok(Some(file)),
        Err(MediaGateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(unavailable()),
    }
}

fn read_json<T: DeserializeOwned>(file: File, limit: u64) -> CmdResult<T> {
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > limit {
        return Err(unavailable());
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

fn speaker_name(name: Option<String>) -> Option<String> {
    name.and_then(|name| {
        let name = name.trim();
        (!name.is_empty()).then(|| name.chars().take(128).collect())
    })
}

fn read_input(guard: &MediaReadGuard, source: Source) -> CmdResult<Option<Input>> {
    let Some(file) = optional_file(guard, source, "tracks.json")? else {
        return Ok(None);
    };
    let (mut input, expected_tracks) = match source {
        Source::Meet => {
            let manifest: bazza_attendee::Manifest = read_json(file, MANIFEST_LIMIT)?;
            if manifest.schema_version != 1
                || manifest.sample_rate != ASR_HZ
                || manifest.tracks.len() > 64
            {
                return Err(unavailable());
            }
            let expected = manifest.tracks.len();
            let anchors = alignment::Anchors::Meet(
                manifest.timing.as_ref().and_then(|timing| timing.origin_unix_ms),
            );
            let tracks = manifest
                .tracks
                .into_iter()
                .filter_map(|row| {
                    let id = row
                        .file
                        .strip_prefix("participant-")?
                        .strip_suffix(".wav")?;
                    let slot = id.parse::<u32>().ok()?;
                    (slot > 0 && id == format!("{slot:03}") && row.received_samples > 0).then_some(
                        Track {
                            id: row.participant_id,
                            name: speaker_name(row.display_name),
                            file: row.file,
                        },
                    )
                })
                .collect();
            (
                Input {
                    tracks,
                    partial: manifest.timing.as_ref().is_none_or(|timing| timing.queue_dropped_packets > 0 || timing.provider_clock_reanchors > 0),
                    sample_rate: manifest.sample_rate,
                    duration_ms: manifest.duration_ms,
                    anchors,
                },
                expected,
            )
        }
        Source::Discord => {
            use bazza_discord::tracks::{MAX_TRACKS, SAMPLE_RATE, TICK_MS};
            let manifest: bazza_discord::Manifest = read_json(file, MANIFEST_LIMIT)?;
            if manifest.schema_version != 1
                || manifest.sample_rate != SAMPLE_RATE
                || !manifest.duration_ms.is_multiple_of(TICK_MS)
                || manifest.tracks.len() > MAX_TRACKS
            {
                return Err(unavailable());
            }
            let expected = manifest.tracks.len();
            let timing_partial = manifest.timeline.as_ref().is_none_or(|timing| timing.truncated || !timing.writer_drops.is_empty());
            let anchors = alignment::Anchors::Discord(manifest.timeline);
            let partial = timing_partial || manifest.dropped_ticks > 0
                || manifest.unmapped_frames > 0
                || manifest.invalid_frames > 0
                || !matches!(
                    manifest.finish_reason.as_str(),
                    "stopped_by_operator" | "duration_elapsed"
                );
            let tracks = manifest
                .tracks
                .into_iter()
                .filter_map(|row| {
                    let id = row.user_id.parse::<u64>().ok()?;
                    if id == 0
                        || row.user_id != id.to_string()
                        || row.file != format!("{id}.wav")
                        || row.received_frames == 0
                    {
                        return None;
                    }
                    let name =
                        (row.display_name != format!("Discord {id}")).then_some(row.display_name);
                    Some(Track {
                        id: row.user_id,
                        name: speaker_name(name),
                        file: row.file,
                    })
                })
                .collect();
            (
                Input {
                    tracks,
                    partial,
                    sample_rate: manifest.sample_rate,
                    duration_ms: manifest.duration_ms,
                    anchors,
                },
                expected,
            )
        }
    };
    if !input.tracks.is_empty() && !(1..=MAX_TRACK_MS).contains(&input.duration_ms) {
        return Err(unavailable());
    }
    let mut ids = HashSet::new();
    let mut files = HashSet::new();
    let sample_rate = input.sample_rate;
    let duration_ms = input.duration_ms;
    input.tracks.retain(|track| {
        !track.id.is_empty()
            && track.id.len() <= 512
            && !ids.contains(&track.id)
            && !files.contains(&track.file)
            && open_wav(guard, source, track, sample_rate, duration_ms).is_ok()
            && ids.insert(track.id.clone())
            && files.insert(track.file.clone())
    });
    input.partial |= input.tracks.len() != expected_tracks;
    Ok(Some(input))
}

fn open_wav(
    guard: &MediaReadGuard,
    source: Source,
    track: &Track,
    sample_rate: u32,
    duration_ms: u64,
) -> CmdResult<hound::WavReader<BufReader<File>>> {
    let file = optional_file(guard, source, &track.file)?.ok_or_else(unavailable)?;
    let samples = duration_ms * u64::from(sample_rate) / 1000;
    if file.metadata().map_err(|_| unavailable())?.len() > samples * 2 + 65_536 {
        return Err(unavailable());
    }
    let reader = hound::WavReader::new(BufReader::new(file)).map_err(|_| unavailable())?;
    let expected = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    if reader.spec() != expected || reader.duration() == 0 || u64::from(reader.duration()) > samples
    {
        return Err(unavailable());
    }
    Ok(reader)
}

fn load_result(guard: &MediaReadGuard, source: Source) -> CmdResult<Option<BotTranscript>> {
    let Some(file) = optional_file(guard, source, RESULT_FILE)? else {
        return Ok(None);
    };
    let saved: StoredTranscript = read_json(file, RESULT_LIMIT)?;
    if saved.schema_version != 1 || saved.transcript.timing != source.timing() {
        return Err(unavailable());
    }
    Ok(Some(saved.transcript))
}

fn save_result(
    guard: &MediaReadGuard,
    source: Source,
    transcript: BotTranscript,
    job: Option<&Job>,
) -> CmdResult<BotTranscript> {
    let directory = guard
        .bot_tracks_directory(source.media())
        .map_err(|_| unavailable())?;
    let stored = StoredTranscript {
        schema_version: 1,
        transcript,
    };
    let bytes = serde_json::to_vec(&stored).map_err(|_| failed())?;
    if bytes.len() as u64 > RESULT_LIMIT {
        return Err(failed());
    }
    let mut file = tempfile::NamedTempFile::new_in(&directory).map_err(|_| failed())?;
    file.write_all(&bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| failed())?;
    if guard
        .bot_tracks_directory(source.media())
        .map_err(|_| unavailable())?
        != directory
    {
        return Err(unavailable());
    }
    if let Some(job) = job { job.begin_publish()?; }
    match file.persist_noclobber(directory.join(RESULT_FILE)) {
        Ok(_) => Ok(stored.transcript),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            load_result(guard, source)?.ok_or_else(failed)
        }
        Err(_) => Err(failed()),
    }
}

fn transcribe_window(
    engine: &mut WhisperJob<'_>,
    job: &Job,
    language: Option<&str>,
    track: &Track,
    (start, samples): (f64, Vec<f32>),
    last: bool,
    duration_ms: i64,
    segments: &mut Vec<BotSegment>,
) -> CmdResult<()> {
    let start_ms = (start * 1000.0).round() as i64;
    let end_ms = (start_ms + samples.len() as i64 * 1000 / i64::from(ASR_HZ)).min(duration_ms);
    let options = TranscribeOpts {
        offset_ms: start_ms,
        lang: language,
        is_final: true,
        max_len: 80,
        drop_no_speech_above: Some(0.6),
        ..TranscribeOpts::default()
    };
    let result = engine
        .transcribe_opts(&samples, &options, Some(&job.cancel))
        .map_err(|error| match error { AsrError::Cancelled => cancelled(), _ => failed() })?;
    // Let the middle of the shared overlap decide ownership while keeping
    // complete captions and independent speech by different participants.
    let owner_start = if start_ms == 0 {
        0
    } else {
        start_ms + HALF_OVERLAP_MS
    };
    let owner_end = if last {
        end_ms
    } else {
        end_ms - HALF_OVERLAP_MS
    };
    for row in result.captions {
        let start = row.start_ms.clamp(start_ms, end_ms);
        let end = row.end_ms.clamp(start_ms, end_ms);
        let middle = start + (end - start) / 2;
        if start < end && (owner_start..owner_end).contains(&middle) {
            if segments.len() >= MAX_SEGMENTS || row.text.len() > 2048 {
                return Err(failed());
            }
            segments.push(BotSegment {
                speaker_id: track.id.clone(),
                speaker_name: track.name.clone(),
                start_ms: start,
                end_ms: end,
                playback_start_ms: None,
                playback_end_ms: None,
                text: row.text,
            });
        }
    }
    Ok(())
}

fn transcribe_input(
    engine: &WhisperEngine,
    job: &Job,
    require_gpu: bool,
    language: Option<&str>,
    guard: &MediaReadGuard,
    source: Source,
    input: Input,
    progress: impl Fn(Stage, u64, u64),
) -> CmdResult<BotTranscript> {
    job.checkpoint()?;
    let mut partial = input.partial;
    let mut segments = Vec::new();
    let mut complete_tracks = Vec::new();
    let mut whisper = engine.job();
    if require_gpu {
        let backend = whisper.prepare().map_err(|error| {
            log::warn!("participant ASR could not initialize the requested GPU decoder: {error}");
            gpu_unavailable()
        })?;
        job.engine(engine.model_id(), backend.as_str(), whisper.backend_reason());
        if !matches!(backend, WhisperBackend::Cuda | WhisperBackend::Metal) {
            log::warn!("participant ASR GPU unavailable: backend={} reason={:?}", backend.as_str(), whisper.backend_reason());
            return Err(gpu_unavailable());
        }
    }
    let mut read_time = Duration::ZERO;
    let mut prepare_time = Duration::ZERO;
    let total = input.duration_ms * input.tracks.len() as u64;
    let report = |completed| {
        job.stage(Stage::Transcribing, Some((completed, total)));
        progress(Stage::Transcribing, completed, total);
    };
    report(0);
    for (index, track) in input.tracks.iter().enumerate() {
        job.checkpoint()?;
        let base = index as u64 * input.duration_ms;
        let track_end = base + input.duration_ms;
        let Ok(reader) = open_wav(guard, source, track, input.sample_rate, input.duration_ms)
        else {
            partial = true;
            report(track_end);
            continue;
        };
        let duration_ms = i64::from(reader.duration()) * 1000 / i64::from(input.sample_rate);
        partial |= duration_ms + 1 < input.duration_ms as i64;
        let mut chunker = Chunker::new(
            ASR_HZ,
            Duration::from_secs(WINDOW_SECONDS),
            Duration::from_secs(OVERLAP_SECONDS),
        );
        let mut resampler = Resampler::new(input.sample_rate, ASR_HZ);
        let mut samples = reader.into_samples::<i16>();
        let mut block = Vec::with_capacity(8192);
        let first_segment = segments.len();
        let mut recognize = |window: (f64, Vec<f32>), last| -> CmdResult<()> {
            let end = (window.0 * 1000.0).round() as u64 + window.1.len() as u64 * 1000 / u64::from(ASR_HZ);
            transcribe_window(&mut whisper, job, language, track, window, last, duration_ms, &mut segments)?;
            job.engine(engine.model_id(), whisper.backend().as_str(), whisper.backend_reason());
            report((base + end).min(track_end));
            Ok(())
        };
        let mut damaged = false;
        loop {
            job.checkpoint()?;
            block.clear();
            let reading = Instant::now();
            for sample in samples.by_ref().take(8192) {
                match sample {
                    Ok(sample) => block.push(f32::from(sample) / 32768.0),
                    Err(_) => {
                        damaged = true;
                        break;
                    }
                }
            }
            read_time += reading.elapsed();
            if damaged { break; }
            let preparing = Instant::now();
            if block.is_empty() {
                // Flush the filter tail as in the regular WAV decoder. The
                // transcript's end stays bounded by the source WAV duration.
                let window = chunker.push(&resampler.process_offline(&[0.0; 64]));
                let tail = chunker.flush();
                prepare_time += preparing.elapsed();
                if let Some(window) = window { recognize(window, false)?; }
                if let Some(window) = tail { recognize(window, true)?; }
                break;
            }
            let window = chunker.push(&resampler.process_offline(&block));
            prepare_time += preparing.elapsed();
            if let Some(window) = window { recognize(window, false)?; }
        }
        if damaged {
            segments.truncate(first_segment);
            partial = true;
        } else {
            complete_tracks.push((track, first_segment..segments.len()));
        }
        report(track_end);
    }
    if complete_tracks.is_empty() {
        return Err(unavailable());
    }
    let aligning = Instant::now();
    let mut mapped = 0;
    let mut attempted = 0;
    if let Some(mut playback) = alignment::Playback::open(guard) {
        let total = segments.len() as u64;
        job.stage(Stage::Aligning, Some((0, total)));
        progress(Stage::Aligning, 0, total);
        for (track, range) in complete_tracks {
            job.checkpoint()?;
            let mut wav = open_wav(guard, source, track, input.sample_rate, input.duration_ms).ok();
            for segment in &mut segments[range] {
                job.checkpoint()?;
                if let Some(wav) = &mut wav {
                    playback.align(wav, &input.anchors, segment);
                    mapped += usize::from(segment.playback_start_ms.is_some());
                }
                attempted += 1;
                job.stage(Stage::Aligning, Some((attempted, total)));
                progress(Stage::Aligning, attempted, total);
            }
        }
    }
    let profile = whisper.profile();
    log::info!(
        "participant ASR: backend={} backend_reason={:?} read_ms={} prepare_ms={} state_ms={} inference_ms={} align_ms={} windows={} skipped={} inferred={} state_creations={} mapped={}/{}",
        whisper.backend().as_str(), whisper.backend_reason(),
        read_time.as_millis(), prepare_time.as_millis(), profile.state_init.as_millis(),
        profile.inference.as_millis(), aligning.elapsed().as_millis(), profile.windows_seen,
        profile.windows_skipped, profile.windows_inferred, profile.state_creations, mapped, attempted,
    );
    segments.sort_by(|a, b| {
        (a.start_ms, a.end_ms, &a.speaker_id).cmp(&(b.start_ms, b.end_ms, &b.speaker_id))
    });
    Ok(BotTranscript {
        timing: source.timing(),
        partial,
        segments,
    })
}

fn run_job(
    app: &AppHandle,
    runtime: &RuntimeState,
    root: &Path,
    meeting_id: &str,
    source: Source,
    job: &Job,
) -> CmdResult<BotTranscript> {
    job.checkpoint()?;
    log::info!("participant ASR: meeting={meeting_id} source={source:?} queue_ms={}", job.elapsed_ms());
    job.stage(Stage::Reading, None);
    if !may_read(app, meeting_id)? {
        return Err(unavailable());
    }
    let guard = MediaReadGuard::acquire(root, meeting_id).map_err(|_| unavailable())?;
    if !may_read(app, meeting_id)? {
        return Err(unavailable());
    }
    if let Some(result) = load_result(&guard, source)? {
        job.begin_publish()?;
        return Ok(result);
    }
    let input = read_input(&guard, source)?
        .filter(|input| !input.tracks.is_empty())
        .ok_or_else(unavailable)?;
    job.checkpoint()?;
    job.stage(Stage::Loading, None);
    let loading = Instant::now();
    let _busy = runtime.busy_guard();
    let settings = runtime.settings.read().clone();
    let tier = settings.tier(&runtime.hardware);
    let require_gpu = settings.tier_override == Some(bazza_models::Tier::Gpu);
    let (live_id, quality_id, _) = settings.model_ids(&runtime.catalog, tier);
    let engine = if settings.quality_asr_model.is_some() {
        if runtime.model_file(&quality_id).is_none() {
            return Err(UiMessage::app("native.botTranscriptNoModel", "Download the selected Whisper model in Settings, then try again."));
        }
        runtime.whisper(&quality_id)
    } else {
        runtime.whisper_or_any(&[&quality_id, &live_id])
    }
        .ok_or_else(|| {
            if runtime.catalog.all().iter().any(|model| {
                model.kind == bazza_models::ModelKind::Asr
                    && bazza_models::is_present(&runtime.models_dir(), model)
            }) {
                failed()
            } else {
                UiMessage::app(
                    "native.botTranscriptNoModel",
                    "Download a local Whisper model in Settings, then try again.",
                )
            }
        })?;
    job.engine(engine.model_id(), engine.backend().as_str(), None);
    log::info!("participant ASR: meeting={meeting_id} source={source:?} model={} load_ms={}", engine.model_id(), loading.elapsed().as_millis());
    let result = transcribe_input(&engine, job, require_gpu, settings.language.as_deref(), &guard, source, input, |_, _, _| {})?;
    // Service deletion can tombstone a meeting while the read pin defers the
    // actual purge. Do not publish a newly computed result for a tombstone.
    if !may_read(app, meeting_id)? {
        return Err(unavailable());
    }
    job.checkpoint()?;
    job.stage(Stage::Saving, None);
    let saving = Instant::now();
    let result = save_result(&guard, source, result, Some(job));
    log::info!("participant ASR: meeting={meeting_id} save_ms={} success={}", saving.elapsed().as_millis(), result.is_ok());
    result
}

/// The current meeting integration wins when a library contains both track sets.
pub(crate) fn participant_source(
    guard: &MediaReadGuard,
    preferred_app: Option<&str>,
) -> CmdResult<Option<Source>> {
    let sources = if preferred_app == Some("discord") {
        [Source::Discord, Source::Meet]
    } else {
        [Source::Meet, Source::Discord]
    };
    let mut rejected = None;
    for source in sources {
        match read_input(guard, source) {
            Ok(Some(input)) if !input.tracks.is_empty() => return Ok(Some(source)),
            Ok(_) => {}
            Err(error) => rejected = Some(error),
        }
    }
    rejected.map_or(Ok(None), Err)
}

fn bounded_name(name: &str, limit: usize) -> &str {
    let mut end = name.len().min(limit);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].trim_end()
}

fn version_speaker_labels(tracks: &[Track]) -> HashMap<String, String> {
    let names: Vec<_> = tracks
        .iter()
        .map(|track| {
            track.name.as_deref()
                .filter(|name| !name.trim().is_empty() && !name.contains('\0'))
                .map(|name| bounded_name(name, MAX_VERSION_SPEAKER_BYTES))
        })
        .collect();
    // A generated label must leave all platform names available to their owners.
    let reserved: HashSet<_> = names.iter().flatten().copied().collect();
    let mut used = HashSet::new();
    let mut unnamed = 1..;
    tracks.iter().zip(names)
        .map(|(track, name)| {
            let label = match name {
                Some(name) if !used.contains(name) => name.to_owned(),
                Some(name) => (2..)
                    .map(|number| {
                        let suffix = format!(" ({number})");
                        let prefix = bounded_name(name, MAX_VERSION_SPEAKER_BYTES - suffix.len());
                        format!("{prefix}{suffix}")
                    })
                    .find(|label| !reserved.contains(label.as_str()) && !used.contains(label))
                    .unwrap(),
                None => unnamed.by_ref()
                    .map(|number| format!("Speaker {number}"))
                    .find(|label| !reserved.contains(label.as_str()))
                    .unwrap(),
            };
            used.insert(label.clone());
            (track.id.clone(), label)
        })
        .collect()
}

/// Compute a fresh result with a fixed model and language. The version owner
/// publishes it only after the job has completed and the meeting still exists.
pub(crate) fn compute_for_version(
    app: &AppHandle,
    runtime: &RuntimeState,
    guard: &MediaReadGuard,
    meeting_id: &str,
    source: Source,
    model: &str,
    language: Option<&str>,
    version_job: &crate::transcript_versions::VersionJob,
) -> CmdResult<Vec<NewTranscriptVersionSegment>> {
    Ok(compute_for_supplement(app, runtime, guard, meeting_id, source, model, language, version_job)?.rows)
}

pub(crate) struct SupplementTracks {
    pub rows: Vec<NewTranscriptVersionSegment>,
    pub partial: bool,
    pub references: HashMap<String, File>,
}

pub(crate) fn compute_for_supplement(
    app: &AppHandle,
    runtime: &RuntimeState,
    guard: &MediaReadGuard,
    meeting_id: &str,
    source: Source,
    model: &str,
    language: Option<&str>,
    version_job: &crate::transcript_versions::VersionJob,
) -> CmdResult<SupplementTracks> {
    use crate::transcript_versions::Phase;
    // The version job owns the UI status and reports success after its save.
    let decoder_job = Job::new(source, version_job.cancel.clone());
    decoder_job.checkpoint()?;
    version_job.update(app, meeting_id, Phase::Reading, None);
    let Some(input) = read_input(guard, source)?.filter(|input| !input.tracks.is_empty()) else {
        return Ok(SupplementTracks { rows: Vec::new(), partial: true, references: HashMap::new() });
    };
    let speaker_labels = version_speaker_labels(&input.tracks);
    let references = input.tracks.iter().map(|track| {
        guard.open_bot_track_file(source.media(), &track.file)
            .map(|file| (track.id.clone(), file)).map_err(|_| unavailable())
    }).collect::<CmdResult<HashMap<_, _>>>()?;
    decoder_job.checkpoint()?;
    version_job.update(app, meeting_id, Phase::Loading, None);
    let _busy = runtime.busy_guard();
    let engine = runtime.whisper(model).ok_or_else(failed)?;
    let require_gpu = runtime.settings.read().tier_override == Some(bazza_models::Tier::Gpu);
    version_job.update(app, meeting_id, Phase::Recognizing, None);
    let result = transcribe_input(&engine, &decoder_job, require_gpu, language, guard, source, input, |stage, completed, total| {
        let percent = (total > 0).then(|| (completed.min(total) * 100 / total) as u8);
        let phase = if stage == Stage::Aligning { Phase::Aligning } else { Phase::Recognizing };
        version_job.update(app, meeting_id, phase, percent);
    })?;
    let partial = result.partial;
    let rows = result.segments.into_iter().map(|row| {
        let playback = row.playback_start_ms
            .zip(row.playback_end_ms)
            .map(|(start_ms, end_ms)| TranscriptPlaybackTiming { start_ms, end_ms })
            .filter(|timing| timing.valid());
        let speaker = speaker_labels.get(&row.speaker_id).cloned();
        NewTranscriptVersionSegment {
            segment: bazza_store::NewSegment {
                start_ms: row.start_ms,
                end_ms: row.end_ms,
                speaker,
                text: row.text,
            },
            playback,
            provenance: Some(TranscriptSegmentProvenance {
                source: TranscriptSegmentSource::Bot,
                provider: Some(match source { Source::Meet => TranscriptParticipantSource::Meet, Source::Discord => TranscriptParticipantSource::Discord }),
                participant_id: Some(row.speaker_id),
                source_start_ms: row.start_ms,
                source_end_ms: row.end_ms,
                uncertain: playback.is_none(),
            }),
        }
    }).collect();
    Ok(SupplementTracks { rows, partial, references })
}

#[tauri::command]
pub async fn bot_transcript_sources(
    app: AppHandle,
    meeting_id: String,
) -> CmdResult<Vec<BotTranscriptSource>> {
    let root = library_root(&app)?;
    blocking(move || {
        if !may_read(&app, &meeting_id)? {
            return Ok(Vec::new());
        }
        let guard = MediaReadGuard::acquire(&root, &meeting_id).map_err(|_| unavailable())?;
        if !may_read(&app, &meeting_id)? {
            return Ok(Vec::new());
        }
        let mut sources = Vec::new();
        let mut rejected = None;
        for source in [Source::Meet, Source::Discord] {
            let result = (|| {
                let Some(input) = read_input(&guard, source)? else {
                    return Ok(None);
                };
                let transcript = load_result(&guard, source)?;
                let track_count = input.tracks.len();
                Ok(
                    (track_count > 0 || transcript.is_some()).then_some(BotTranscriptSource {
                        source,
                        track_count,
                        transcript,
                    }),
                )
            })();
            match result {
                Ok(Some(source)) => sources.push(source),
                Ok(None) => {}
                Err(error) => rejected = Some(error),
            }
        }
        if sources.is_empty() {
            if let Some(error) = rejected {
                return Err(error);
            }
        }
        Ok(sources)
    })
    .await
}

#[tauri::command]
pub fn bot_transcript_jobs(meeting_id: String, jobs: tauri::State<'_, Jobs>) -> Vec<JobStatus> {
    if valid_id(&meeting_id) { jobs.list(&meeting_id) } else { Vec::new() }
}

#[tauri::command]
pub fn cancel_bot_transcript(app: AppHandle, meeting_id: String, source: Source, job_id: Uuid) -> bool {
    let Some(was_queued) = app.state::<Jobs>().cancel(&meeting_id, source, job_id) else { return false; };
    if was_queued {
        if let Some(state) = app.try_state::<AppState>() {
            state.processing_jobs.cancel_pending(job_id);
        }
    }
    true
}

#[tauri::command]
pub async fn transcribe_bot_tracks(
    app: AppHandle,
    meeting_id: String,
    source: Source,
) -> CmdResult<BotTranscript> {
    if !valid_id(&meeting_id) {
        return Err(unavailable());
    }
    let root = library_root(&app)?;
    if app.try_state::<AppState>().is_some() {
        // A queued job owns its meeting ID. Never reserve a recording that
        // Stop still needs to enqueue for ordinary post-processing.
        let valid = {
            let app = app.clone();
            let id = meeting_id.clone();
            blocking(move || may_read(&app, &id)).await?
        };
        if !valid {
            return Err(unavailable());
        }
        let lease = app.state::<Jobs>().start(&meeting_id, source)?;
        let job_id = lease.id();
        let handle = lease.handle();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::processing_jobs::enqueue_tagged(&app, meeting_id.clone(), job_id, move |app| {
            let state = app.state::<AppState>();
            let result = state
                .data_copy
                .try_read()
                .ok_or_else(busy)
                .and_then(|_copy| run_job(app, &state, &root, &meeting_id, source, lease.job()));
            lease.finish(&result);
            let _ = tx.send(result);
        })
        .map_err(|_| busy())?;
        // Cancellation can arrive between registering the job and queuing it.
        if handle.checkpoint().is_err() {
            app.state::<AppState>().processing_jobs.cancel_pending(job_id);
        }
        return rx.await.map_err(|_| handle.checkpoint().err().unwrap_or_else(failed))?;
    }
    let lease = app.state::<Jobs>().start(&meeting_id, source)?;
    blocking(move || {
        let result = (|| {
            lease.job().checkpoint()?;
            let runtime = app.state::<RuntimeState>();
            let _job = runtime.local_asr_work.try_lock().ok_or_else(busy)?;
            if !app.state::<ServiceCapture>().try_admit_postprocess() {
                return Err(busy());
            }
            run_job(&app, &runtime, &root, &meeting_id, source, lease.job())
        })();
        lease.finish(&result);
        result
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn version_labels_keep_distinct_participant_ids_and_reserve_platform_names() {
        let long_name = "🎙".repeat(80);
        let tracks: Vec<_> = [
            None, Some("Alex"), Some("Alex"), Some("Alex (2)"), Some("Speaker 1"),
            Some(long_name.as_str()), Some(long_name.as_str()),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| Track {
            id: index.to_string(),
            name: name.map(str::to_owned),
            file: String::new(),
        })
        .collect();
        let labels = version_speaker_labels(&tracks);
        assert_eq!(labels["0"], "Speaker 2");
        assert_eq!(labels["1"], "Alex");
        assert_eq!(labels["2"], "Alex (3)");
        assert_eq!(labels["3"], "Alex (2)");
        assert_eq!(labels["4"], "Speaker 1");
        assert_ne!(labels["5"], labels["6"]);
        assert!(labels.values().all(|label| label.len() <= MAX_VERSION_SPEAKER_BYTES));
    }

    fn wave(path: &Path, rate: u32) {
        let mut wav = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..rate / 50 {
            wav.write_sample(0_i16).unwrap();
        }
        wav.finalize().unwrap();
    }

    #[test]
    fn meet_accepts_distinct_speakers_with_the_same_name_and_rejects_an_escaped_track() {
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        let directory = root.path().join("recordings").join(&id).join("attendee");
        fs::create_dir_all(&directory).unwrap();
        wave(&directory.join("participant-001.wav"), ASR_HZ);
        wave(&directory.join("participant-002.wav"), ASR_HZ);
        wave(
            &directory.parent().unwrap().join("participant-003.wav"),
            ASR_HZ,
        );
        let manifest = serde_json::json!({
            "schema_version": 1, "sample_rate": ASR_HZ, "duration_ms": 20,
            "tracks": [
                { "participant_id": "one", "display_name": "Alex", "file": "participant-001.wav", "received_samples": 320 },
                { "participant_id": "two", "display_name": "Alex", "file": "participant-002.wav", "received_samples": 320 },
                { "participant_id": "escaped", "display_name": "Alex", "file": "../participant-003.wav", "received_samples": 320 }
            ],
        });
        fs::write(directory.join("tracks.json"), manifest.to_string()).unwrap();
        let guard = MediaReadGuard::acquire(root.path(), &id).unwrap();
        let input = read_input(&guard, Source::Meet).unwrap().unwrap();
        assert!(input.partial);
        assert_eq!(input.tracks.len(), 2);
        assert_eq!(input.tracks[0].name, input.tracks[1].name);
        assert_ne!(input.tracks[0].id, input.tracks[1].id);
        assert!(guard
            .open_bot_track_file(BotTrackSource::Meet, "../participant-003.wav")
            .is_err());
    }

    #[test]
    fn discord_marks_known_packet_loss_and_does_not_replace_an_existing_result() {
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        let directory = root.path().join("recordings").join(&id).join("discord");
        fs::create_dir_all(&directory).unwrap();
        wave(
            &directory.join("123.wav"),
            bazza_discord::tracks::SAMPLE_RATE,
        );
        let manifest = serde_json::json!({
            "schema_version": 1, "sample_rate": bazza_discord::tracks::SAMPLE_RATE, "duration_ms": 20,
            "finish_reason": "stopped_by_operator", "dropped_ticks": 1, "unmapped_frames": 0, "invalid_frames": 0,
            "tracks": [{ "user_id": "123", "display_name": "Discord 123", "file": "123.wav", "received_frames": 1 }],
        });
        fs::write(directory.join("tracks.json"), manifest.to_string()).unwrap();
        fs::write(
            directory.parent().unwrap().join("mix.wav"),
            b"main recording",
        )
        .unwrap();
        let guard = MediaReadGuard::acquire(root.path(), &id).unwrap();
        let input = read_input(&guard, Source::Discord).unwrap().unwrap();
        assert!(input.partial);
        assert_eq!(input.tracks[0].name, None);
        let transcript = BotTranscript {
            timing: Timing::Relative,
            partial: true,
            segments: vec![],
        };
        assert!(
            save_result(&guard, Source::Discord, transcript, None)
                .unwrap()
                .partial
        );
        let other = BotTranscript {
            timing: Timing::Relative,
            partial: false,
            segments: vec![],
        };
        assert!(save_result(&guard, Source::Discord, other, None).unwrap().partial);
        assert_eq!(
            fs::read(directory.parent().unwrap().join("mix.wav")).unwrap(),
            b"main recording"
        );
    }
}

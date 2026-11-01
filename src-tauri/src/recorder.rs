//! Orchestrates one recording: capture + optional video + live captions, then
//! the post-call pipeline (mux, quality transcription).

use crate::events::{self, LevelsPayload, NoticePayload, RecordingStatePayload};
use crate::state::AppState;
use crate::windows;
use hark_capture::{CaptureEvent, RecordConfig, Recorder, VideoRecorder, VideoTarget};
use hark_store::{Meeting, MeetingStatus};
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use std::thread::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};

// Start and stop run on background threads. Keep a newer start from replacing
// the live feed while an older stop still joins its transcriber/forwarder.
static RECORDING_LIFECYCLE: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StartOptions {
    pub title: Option<String>,
    pub app: Option<String>,
    pub video: Option<bool>,
    pub target: Option<VideoTarget>,
    /// Capture system audio only from this process tree (from the window picker).
    #[serde(default)]
    pub audio_pid: Option<u32>,
}

pub struct ActiveRecording {
    pub meeting: Meeting,
    recorder: Option<Recorder>,
    video: Option<VideoRecorder>,
    /// Feeds live ASR; shared with the forwarder so `stop` can close it from here
    /// (the only sender) instead of waiting for the capture channel to drain.
    pcm_tx: Arc<Mutex<Option<crossbeam_channel::Sender<Vec<f32>>>>>,
    live: Option<crate::stt_stage::SttWorker>,
    captions: Option<JoinHandle<()>>,
    forwarder: Option<JoinHandle<()>>,
    forwarder_stop: Arc<AtomicBool>,
    pub highlights: Vec<u64>,
    /// Stops the running-notes loop.
    notes_stop: Arc<AtomicBool>,
}

pub fn status_payload(state: &AppState) -> RecordingStatePayload {
    match state.recording.lock().as_ref() {
        Some(a) => RecordingStatePayload {
            state: if a.recorder.as_ref().map(|r| r.is_paused()).unwrap_or(false) { "paused" } else { "recording" },
            meeting_id: Some(a.meeting.id.clone()),
            elapsed_ms: a.recorder.as_ref().map(|r| r.elapsed().as_millis() as u64).unwrap_or(0),
        },
        None => RecordingStatePayload { state: "idle", meeting_id: None, elapsed_ms: 0 },
    }
}

fn emit_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    let _ = app.emit(events::RECORDING_STATE, status_payload(&state));
}

pub fn start(app: &AppHandle, opts: StartOptions) -> Result<Meeting, String> {
    let _lifecycle = RECORDING_LIFECYCLE.lock();
    let state = app.state::<AppState>();
    if state.recording.lock().is_some() {
        return Err("already recording".into());
    }
    let settings = state.settings.read().clone();
    let event = crate::calendar::current_event(&state);
    let title = opts
        .title
        .clone()
        .or_else(|| event.as_ref().map(|e| e.title.clone()))
        .unwrap_or_else(|| default_title(opts.app.as_deref()));
    let mut meeting = Meeting::new_recording(&title, opts.app.as_deref());
    if let Some(e) = &event {
        meeting.calendar_uid = Some(e.uid.clone());
        meeting.participants = e.attendees.iter().filter(|a| !a.eq_ignore_ascii_case(settings.user_name.trim())).cloned().collect();
        log::info!("recorder: matched calendar event '{}'", e.title);
    }
    let want_video = opts.video.unwrap_or(settings.video_enabled);
    let dir = state.store.recordings_dir(&meeting.id);
    crate::stt_stage::prepare_selection(&state, &meeting.id, &settings)?;

    log::info!("recorder: opening audio devices");
    let (cap_tx, cap_rx) = crossbeam_channel::unbounded::<CaptureEvent>();
    let recorder = Recorder::start(
        RecordConfig {
            dir: dir.clone(),
            mic_device: settings.mic_device.clone(),
            loopback_device: settings.loopback_device.clone(),
            capture_system: settings.capture_system,
            audio_pid: opts.audio_pid,
        },
        cap_tx,
    )
    .map_err(|e| e.to_string())?;

    // Optional screen video.
    let video = if want_video {
        match state.ffmpeg.as_ref() {
            Some(ffmpeg) => {
                let target = opts.target.clone().unwrap_or_else(|| settings.video_target.clone());
                match VideoRecorder::start(ffmpeg, &target, &dir.join("screen.raw.mp4"), settings.video_fps, settings.video_max_height) {
                    Ok(v) => {
                        meeting.has_video = true;
                        Some(v)
                    }
                    Err(e) => {
                        notice(app, "warning", format!("Screen recording failed to start: {e}. Recording audio only."));
                        None
                    }
                }
            }
            None => {
                notice(app, "warning", "ffmpeg not found; recording audio only.".into());
                None
            }
        }
    } else {
        None
    };

    state.store.create_meeting(&meeting).map_err(|e| e.to_string())?;
    log::info!("recorder: audio open, loading live model");
    crate::live_feed::LiveFeed::begin(&state, &meeting.id);
    let notes_stop = Arc::new(AtomicBool::new(false));
    if settings.summary_enabled {
        crate::live_feed::spawn_notes_loop(app.clone(), meeting.id.clone(), notes_stop.clone());
    }

    // The selected STT provider never owns or blocks the local media writer.
    let crate::stt_stage::LiveSession { input: pcm_tx, worker: live, captions, overflow } = crate::stt_stage::start_live(app, &meeting.id, &settings);

    // Forward capture events to the UI and the live transcriber. Exits on the
    // stop flag: the capture channel may stay open while a dead stream is
    // being disposed off-thread, so we never wait for it to close.
    let pcm_tx = Arc::new(Mutex::new(pcm_tx));
    let forwarder_stop = Arc::new(AtomicBool::new(false));
    // A writer may fail while devices/models are still being opened. Publish
    // ActiveRecording before forwarding that queued failure to auto-stop.
    let (forwarder_ready, ready) = crossbeam_channel::bounded::<()>(1);
    let forwarder = {
        let app = app.clone();
        let pcm_tx = pcm_tx.clone();
        let stop = forwarder_stop.clone();
        let meeting_id = meeting.id.clone();
        std::thread::spawn(move || {
            if ready.recv().is_err() { return; }
            let mut next_video_check = std::time::Instant::now();
            loop {
                if stop.load(Ordering::Relaxed) {
                    // Recorder::stop has finalized the writer. Drain its queued
                    // last PCM frames without waiting for dead device senders.
                    while let Ok(event) = cap_rx.try_recv() {
                        if let CaptureEvent::Pcm16k(pcm) = event {
                            if let Some(tx) = pcm_tx.lock().as_ref() {
                                if tx.try_send(pcm).is_err() {
                                    if let Some(overflow) = &overflow { overflow.store(true, Ordering::Relaxed); }
                                }
                            }
                        }
                    }
                    break;
                }
                if std::time::Instant::now() >= next_video_check {
                    next_video_check = std::time::Instant::now() + std::time::Duration::from_secs(1);
                    let warning = {
                        let state = app.state::<AppState>();
                        let mut recording = state.recording.lock();
                        recording.as_mut().and_then(|active| active.video.as_mut()).and_then(|video| video.poll_warning())
                    };
                    if let Some(warning) = warning {
                        notice(&app, "warning", warning);
                    }
                }
                let ev = match cap_rx.recv_timeout(std::time::Duration::from_millis(300)) {
                    Ok(ev) => ev,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                };
                match ev {
                    CaptureEvent::Levels { mic_db, sys_db } => {
                        let _ = app.emit(events::LEVELS, LevelsPayload { mic_db, sys_db });
                    }
                    CaptureEvent::Pcm16k(pcm) => {
                        if let Some(tx) = pcm_tx.lock().as_ref() {
                            if tx.try_send(pcm).is_err() {
                                if let Some(overflow) = &overflow { overflow.store(true, Ordering::Relaxed); }
                            }
                        }
                    }
                    CaptureEvent::Warning(m) if m.contains("underrun") || m.contains("overrun") => log::debug!("capture: {m}"),
                    CaptureEvent::Warning(m) => notice(&app, "warning", m),
                    CaptureEvent::Error(m) => log::warn!("capture: {m}"),
                    CaptureEvent::WriterError(message) => {
                        notice(&app, "error", format!("{message}. Recording is stopping; captured files will be kept."));
                        // stop joins this forwarder, so it must run elsewhere.
                        let app = app.clone();
                        let meeting_id = meeting_id.clone();
                        std::thread::spawn(move || {
                            if let Err(error) = stop_matching(&app, Some(&meeting_id)) {
                                log::warn!("finish recording after audio writer failure: {error}");
                            }
                            crate::tray::refresh(&app);
                        });
                    }
                    CaptureEvent::Stalled { mic, sys } => reopen_streams(&app, mic, sys),
                }
            }
        })
    };

    *state.recording.lock() = Some(ActiveRecording {
        meeting: meeting.clone(),
        recorder: Some(recorder),
        video,
        pcm_tx,
        live,
        captions,
        forwarder: Some(forwarder),
        forwarder_stop,
        highlights: Vec::new(),
        notes_stop,
    });
    state.detection_paused.store(true, Ordering::SeqCst);
    spawn_participant_sampler(app.clone(), meeting.id.clone(), opts.app.clone());
    log::info!("recorder: showing record bar");
    windows::hide_popup(app);
    windows::show_recordbar(app);
    emit_state(app);
    log::info!("recorder: started {}", meeting.id);
    let _ = forwarder_ready.send(());
    Ok(meeting)
}

/// While this meeting records, read attendee names from the meeting window's
/// accessibility tree every 45 s and merge them into the meeting.
/// Manual start: ask which window/app to record before anything is captured.
/// The popup is the same one detection uses, with app "manual" and no countdown.
pub fn open_picker(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.recording.lock().is_some() {
        return;
    }
    let _ = app.emit(
        events::DETECTION,
        events::DetectionPayload {
            app: "manual".into(),
            label: "Ready to record".into(),
            title: "Pick the window or app to record.".into(),
            confidence: 1.0,
            event: crate::calendar::current_event(&state).map(|e| e.title),
        },
    );
    windows::show_popup_near(app, 0);
}

/// A stream died (device switched, unplugged, or invalidated by Windows):
/// rebuild it on the current devices so the recording carries on.
fn reopen_streams(app: &AppHandle, mic: bool, sys: bool) {
    let state = app.state::<AppState>();
    let mut guard = state.recording.lock();
    let Some(active) = guard.as_mut() else { return };
    let Some(rec) = active.recorder.as_mut() else { return };
    log::warn!("capture stalled (mic={mic}, sys={sys}); reopening audio devices");
    match rec.reopen() {
        Ok(()) => notice(app, "warning", "Audio device changed - Hark switched to the current one and is still recording.".into()),
        Err(e) => {
            log::error!("reopen audio: {e}");
            notice(app, "error", format!("Audio device lost ({e}). Hark keeps trying; stop the recording to keep what was captured."));
        }
    }
}

/// Meetings left in `recording` by a crash or a hung stop: finish them from
/// the files on disk so nothing that was captured is lost.
pub fn recover_orphans(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(meetings) = state.store.list_meetings() else { return };
    for mut m in meetings.into_iter().filter(|m| matches!(m.status, MeetingStatus::Recording | MeetingStatus::Processing)) {
        let dir = state.store.recordings_dir(&m.id);
        let mix = dir.join("mix.wav");
        if m.status == MeetingStatus::Processing {
            // Interrupted mid-pipeline (crash or kill): just run it again.
            if !mix.exists() {
                m.status = MeetingStatus::Failed;
                m.error = Some("Processing was interrupted and the audio file is missing.".into());
                let _ = state.store.update_meeting(&m);
                continue;
            }
            log::info!("resuming interrupted processing of {}", m.id);
            notice(app, "info", format!("Finishing \"{}\" from the last session.", m.title));
            let app2 = app.clone();
            std::thread::spawn(move || crate::pipeline::post_process(&app2, m));
            continue;
        }
        let duration = hound::WavReader::open(&mix)
            .ok()
            .map(|r| r.len() as i64 * 1000 / (r.spec().sample_rate.max(1) as i64 * r.spec().channels.max(1) as i64))
            .unwrap_or(0);
        if duration < 1000 {
            log::warn!("orphaned recording {} has no usable audio; marking failed", m.id);
            m.status = MeetingStatus::Failed;
            m.error = Some("Recording was interrupted before any audio was saved.".into());
            let _ = state.store.update_meeting(&m);
            continue;
        }
        let ended = std::fs::metadata(&mix).and_then(|md| md.modified()).map(chrono::DateTime::<chrono::Utc>::from).unwrap_or_else(|_| chrono::Utc::now());
        log::info!("recovering orphaned recording {} ({} ms)", m.id, duration);
        m.duration_ms = duration;
        m.ended_at = Some(ended);
        m.status = MeetingStatus::Processing;
        m.has_video = m.has_video || dir.join("screen.raw.mp4").exists() || dir.join("screen.mp4").exists();
        let _ = state.store.update_meeting(&m);
        notice(app, "info", format!("Finishing \"{}\" from the last session.", m.title));
        let app2 = app.clone();
        std::thread::spawn(move || crate::pipeline::post_process(&app2, m));
    }
}

fn spawn_participant_sampler(app: AppHandle, meeting_id: String, app_id: Option<String>) {
    let Some(app_id) = app_id.filter(|a| a != "unknown") else { return };
    std::thread::spawn(move || {
        let patterns = hark_detect::PatternSet::builtin();
        let mut first = true;
        loop {
            if !first {
                std::thread::sleep(std::time::Duration::from_secs(45));
            }
            first = false;
            let state = app.state::<AppState>();
            let still = state.recording.lock().as_ref().map(|a| a.meeting.id == meeting_id).unwrap_or(false);
            if !still {
                break;
            }
            let windows = hark_detect::list_visible_windows();
            let Some(det) = patterns.match_windows(&windows).filter(|d| d.app == app_id && d.hwnd != 0) else { continue };
            let raw = hark_detect::scrape_window(det.hwnd);
            let names = hark_detect::extract_names(&app_id, &raw);
            if names.is_empty() {
                continue;
            }
            let mut merged = state.recording.lock().as_ref().map(|a| a.meeting.participants.clone()).unwrap_or_default();
            let before = merged.len();
            for n in names {
                if !merged.iter().any(|m| m.eq_ignore_ascii_case(&n)) {
                    merged.push(n);
                }
            }
            if merged.len() == before {
                continue;
            }
            if let Some(a) = state.recording.lock().as_mut() {
                a.meeting.participants = merged.clone();
            }
            let _ = state.store.set_participants(&meeting_id, &merged);
            let _ = app.emit(events::PARTICIPANTS, events::ParticipantsPayload { meeting_id: meeting_id.clone(), participants: merged });
        }
    });
}

pub fn pause(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let result = (|| {
        if let Some(a) = state.recording.lock().as_mut() {
            if let Some(r) = &a.recorder {
                if let Some(v) = a.video.as_mut() {
                    v.pause_with(|| r.pause()).map_err(|e| format!("Could not save video pause: {e}"))?;
                } else {
                    r.pause();
                }
            }
        }
        Ok(())
    })();
    // A final metadata-write failure can occur after the actual audio transition.
    // Publish the real state even when the command reports that recoverable error.
    emit_state(app);
    result
}

pub fn resume(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let result = (|| {
        if let Some(a) = state.recording.lock().as_mut() {
            if let Some(r) = &a.recorder {
                if let Some(v) = a.video.as_mut() {
                    v.resume_with(|| r.resume()).map_err(|e| format!("Could not save video resume: {e}"))?;
                } else {
                    r.resume();
                }
            }
        }
        Ok(())
    })();
    emit_state(app);
    result
}

pub fn mark_highlight(app: &AppHandle) -> Option<u64> {
    let state = app.state::<AppState>();
    let mut g = state.recording.lock();
    let a = g.as_mut()?;
    let ms = a.recorder.as_ref()?.elapsed().as_millis() as u64;
    a.highlights.push(ms);
    Some(ms)
}

pub fn stop(app: &AppHandle) -> Result<Meeting, String> {
    stop_matching(app, None)
}

/// A late failure from an old forwarder must not take a newer recording.
fn take_matching_recording(recording: &mut Option<ActiveRecording>, expected_id: Option<&str>) -> Result<ActiveRecording, String> {
    let active = recording.as_ref().ok_or("not recording")?;
    if expected_id.is_some_and(|id| id != active.meeting.id) {
        return Err("recording has changed".into());
    }
    Ok(recording.take().expect("recording checked while locked"))
}

fn stop_matching(app: &AppHandle, expected_id: Option<&str>) -> Result<Meeting, String> {
    let _lifecycle = RECORDING_LIFECYCLE.lock();
    let state = app.state::<AppState>();
    let mut active = take_matching_recording(&mut state.recording.lock(), expected_id)?;
    state.detection_paused.store(false, Ordering::SeqCst);
    active.notes_stop.store(true, Ordering::Relaxed);
    windows::hide_recordbar(app);

    // A failed writer is already stopped, but video/live resources still need
    // their normal shutdown. Do not return early before completing it.
    let audio_result = active.recorder.take().ok_or_else(|| "recorder missing".to_string())
        .and_then(|recorder| recorder.stop().map_err(|error| error.to_string()));
    if let Some(v) = active.video.take() {
        if let Err(error) = v.stop() {
            log::warn!("screen capture could not finish cleanly: {error}");
            notice(app, "warning", format!("Screen recording did not finish cleanly ({error}). Audio and recoverable video were kept."));
        }
    }
    // Stop forwarding, close the live feed (its only sender) and wait for the last window.
    active.forwarder_stop.store(true, Ordering::Relaxed);
    if let Some(f) = active.forwarder.take() {
        let _ = f.join();
    }
    drop(active.pcm_tx.lock().take());
    if let Some(l) = active.live.take() {
        l.join();
    }
    if let Some(captions) = active.captions.take() {
        let _ = captions.join();
    }

    let mut meeting = active.meeting.clone();
    let dir = state.store.recordings_dir(&meeting.id);
    conclude_meeting(&mut meeting, &audio_result, &dir);
    let saved = state.store.update_meeting(&meeting).map_err(|error| error.to_string());
    if !active.highlights.is_empty() {
        let _ = state.store.set_setting(&format!("highlights:{}", meeting.id), &active.highlights);
    }
    crate::live_feed::LiveFeed::end(&state);
    emit_state(app);
    saved?;
    audio_result?;

    let app2 = app.clone();
    let m2 = meeting.clone();
    std::thread::spawn(move || crate::pipeline::post_process(&app2, m2));
    Ok(meeting)
}

fn conclude_meeting(meeting: &mut Meeting, audio_result: &Result<hark_capture::RecordOutput, String>, dir: &Path) {
    meeting.ended_at = Some(chrono::Utc::now());
    match audio_result {
        Ok(output) => {
            meeting.duration_ms = output.duration.as_millis() as i64;
            meeting.status = MeetingStatus::Processing;
        }
        Err(error) => {
            // The writer's Drop finalizes whatever header it can. Keep every
            // input for recovery, but do not publish it as a successful capture.
            meeting.duration_ms = hound::WavReader::open(dir.join("mix.wav")).ok()
                .map(|reader| reader.len() as i64 * 1000 / (reader.spec().sample_rate.max(1) as i64 * reader.spec().channels.max(1) as i64))
                .unwrap_or(0);
            meeting.status = MeetingStatus::Failed;
            meeting.error = Some(format!("audio: {error}. Available recording files were kept."));
            meeting.has_video = dir.join("screen.mp4").exists();
        }
    }
}

/// Re-run the quality transcription for an existing meeting (e.g. after downloading a model).
pub fn retranscribe(app: &AppHandle, meeting_id: &str, asr_model: Option<String>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut m = state.store.get_meeting(meeting_id).map_err(|e| e.to_string())?.ok_or("no such meeting")?;
    if m.status == MeetingStatus::Recording {
        return Err("still recording".into());
    }
    m.status = MeetingStatus::Processing;
    state.store.update_meeting(&m).map_err(|e| e.to_string())?;
    let app2 = app.clone();
    std::thread::spawn(move || crate::pipeline::post_process_with(&app2, m, crate::pipeline::PostOpts { asr_model, force_local_asr: true }));
    Ok(())
}

pub fn read_wav_as_16k_mono(path: &Path) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = ((1u64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader.into_samples::<i32>().map(|s| s.map(|v| v as f32 / max)).collect::<Result<_, _>>().map_err(|e| e.to_string())?
        }
        hound::SampleFormat::Float => reader.into_samples::<f32>().collect::<Result<_, _>>().map_err(|e| e.to_string())?,
    };
    let mono = hark_capture::dsp::to_mono(&samples, ch);
    let mut rs = hark_capture::dsp::Resampler::new(spec.sample_rate, hark_capture::ASR_HZ);
    let mut out = rs.process(&mono);
    // Push a tail of silence so the final samples make it through the filter delay.
    out.extend(rs.process(&vec![0.0; 64]));
    Ok(out)
}

fn default_title(app: Option<&str>) -> String {
    let when = chrono::Local::now().format("%b %-d, %-I:%M %p");
    match app {
        Some(a) => format!("{} call - {when}", capitalize(a)),
        None => format!("Recording - {when}"),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub fn notice(app: &AppHandle, level: &'static str, message: String) {
    log::info!("[{level}] {message}");
    let _ = app.emit(events::NOTICE, NoticePayload { level, message });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    struct FixtureDir(PathBuf);
    impl FixtureDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("hark-recorder-stop-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for FixtureDir {
        fn drop(&mut self) {
            // Only the named synthetic files; no recursive deletion.
            for name in ["mix.wav", "screen.raw.mp4"] { let _ = std::fs::remove_file(self.0.join(name)); }
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    fn inactive_resources(meeting: Meeting) -> ActiveRecording {
        ActiveRecording {
            meeting, recorder: None, video: None,
            pcm_tx: Arc::new(Mutex::new(None)), live: None, captions: None, forwarder: None,
            forwarder_stop: Arc::new(AtomicBool::new(false)), highlights: Vec::new(),
            notes_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn old_writer_failure_cannot_take_a_new_recording() {
        let old = Meeting::new_recording("old", None);
        let new = Meeting::new_recording("new", None);
        let mut active = Some(inactive_resources(new.clone()));
        assert!(take_matching_recording(&mut active, Some(&old.id)).is_err());
        assert_eq!(active.as_ref().unwrap().meeting.id, new.id);
        assert_eq!(take_matching_recording(&mut active, Some(&new.id)).unwrap().meeting.id, new.id);
        assert!(active.is_none());
    }

    #[test]
    fn failed_writer_keeps_partial_inputs_and_marks_failed() {
        let dir = FixtureDir::new();
        let mut wav = hark_capture::WavWriter::create(&dir.0.join("mix.wav"), 48_000, 1).unwrap();
        wav.write(&vec![0.25; 4_800]).unwrap();
        wav.finish().unwrap();
        std::fs::write(dir.0.join("screen.raw.mp4"), b"recoverable raw capture").unwrap();
        let mut meeting = Meeting::new_recording("writer failure", None);
        meeting.has_video = true;
        conclude_meeting(&mut meeting, &Err("controlled writer failure".into()), &dir.0);
        assert_eq!(meeting.status, MeetingStatus::Failed);
        assert!(meeting.ended_at.is_some());
        assert_eq!(meeting.duration_ms, 100);
        assert!(!meeting.has_video);
        assert!(meeting.error.as_deref().unwrap().contains("controlled writer failure"));
        assert_eq!(hound::WavReader::open(dir.0.join("mix.wav")).unwrap().len(), 4_800);
        assert_eq!(std::fs::read(dir.0.join("screen.raw.mp4")).unwrap(), b"recoverable raw capture");
    }

    #[test]
    fn successful_capture_keeps_the_processing_path() {
        let dir = FixtureDir::new();
        let output = hark_capture::RecordOutput {
            mic_wav: dir.0.join("mic.wav"), sys_wav: None, mix_wav: dir.0.join("mix.wav"),
            duration: Duration::from_millis(250),
        };
        let mut meeting = Meeting::new_recording("normal stop", None);
        meeting.has_video = true;
        conclude_meeting(&mut meeting, &Ok(output), &dir.0);
        assert_eq!(meeting.status, MeetingStatus::Processing);
        assert_eq!(meeting.duration_ms, 250);
        assert!(meeting.has_video);
        assert!(meeting.error.is_none());
    }
}

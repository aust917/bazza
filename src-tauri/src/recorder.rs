//! Orchestrates one recording: capture + optional video + live captions, then
//! the post-call pipeline (mux, quality transcription).

use crate::events::{self, LevelsPayload, NoticePayload, RecordingStatePayload};
use crate::state::AppState;
use crate::windows;
use bazza_capture::{CaptureEvent, MonitorInfo, RecordConfig, Recorder, VideoRecorder, VideoTarget};
use bazza_store::{Meeting, MeetingStatus};
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

/// Keep library mutations and processing out of capture startup and Stop,
/// including the parts where AppState does not hold an active recording.
pub(crate) fn try_recording_lifecycle() -> Option<parking_lot::MutexGuard<'static, ()>> {
    RECORDING_LIFECYCLE.try_lock()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StartOptions {
    pub title: Option<String>,
    pub app: Option<String>,
    pub video: Option<bool>,
    pub target: Option<VideoTarget>,
    /// Opaque screen identity returned by list_video_sources.
    pub screen_id: Option<String>,
    #[serde(default)]
    pub(crate) discord: Option<crate::discord_recording::DiscordRecordingSelection>,
    #[serde(default)]
    pub(crate) attendee: Option<crate::attendee_recording::AttendeeRecordingSelection>,
    #[serde(default)]
    pub(crate) bot_connection_id: Option<uuid::Uuid>,
}

impl StartOptions {
    pub(crate) fn bot_provider(&self, prepared_provider: Option<&'static str>) -> Option<&'static str> {
        prepared_provider
            .or_else(|| self.discord.as_ref().map(|_| "discord"))
            .or_else(|| self.attendee.as_ref().map(|_| "meet"))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScreenSelectionError {
    Window,
    Required,
    Unavailable,
}

impl ScreenSelectionError {
    pub(crate) fn message(&self) -> (&'static str, &'static str) {
        match self {
            Self::Window => ("native.screenOnlyVideo", "Choose a screen to record video."),
            Self::Required => ("native.screenSelectionRequired", "Choose which screen to record."),
            Self::Unavailable => ("native.screenSelectionUnavailable", "The selected screen is unavailable. Choose a screen again."),
        }
    }
}

pub(crate) fn resolve_recording_screen(
    video: bool,
    target: Option<&VideoTarget>,
    screen_id: Option<&str>,
    ffmpeg: Option<&Path>,
) -> Result<Option<MonitorInfo>, ScreenSelectionError> {
    if !video {
        return Ok(None);
    }
    if matches!(target, Some(VideoTarget::Window { .. })) {
        return Err(ScreenSelectionError::Window);
    }
    select_recording_screen(bazza_capture::list_monitors(ffmpeg), target, screen_id).map(Some)
}

fn select_recording_screen(
    screens: Vec<MonitorInfo>,
    target: Option<&VideoTarget>,
    screen_id: Option<&str>,
) -> Result<MonitorInfo, ScreenSelectionError> {
    if let Some(screen_id) = screen_id {
        let mut matches = screens.into_iter().filter(|screen| screen.screen_id == screen_id);
        return matches.next().filter(|_| matches.next().is_none()).ok_or(ScreenSelectionError::Unavailable);
    }
    match screens.as_slice() {
        [] => Err(ScreenSelectionError::Unavailable),
        [only] if target.is_none_or(|target| matches!(target, VideoTarget::Monitor { index } if *index == only.index)) => Ok(only.clone()),
        [_] => Err(ScreenSelectionError::Unavailable),
        _ => Err(ScreenSelectionError::Required),
    }
}

pub(crate) fn audio_writer_config(
    dir: &Path,
    settings: &crate::settings::Settings,
) -> RecordConfig {
    RecordConfig {
        dir: dir.into(),
        mic_device: settings.mic_device.clone(),
        loopback_device: settings.loopback_device.clone(),
        capture_system: settings.capture_system,
    }
}

pub(crate) fn start_screen_writer(
    ffmpeg: &Path,
    dir: &Path,
    settings: &crate::settings::Settings,
    screen: &MonitorInfo,
) -> std::io::Result<VideoRecorder> {
    VideoRecorder::start(ffmpeg, screen, &dir.join("screen.raw.mp4"), settings.video_fps, settings.video_max_height)
}

pub struct ActiveRecording {
    pub meeting: Meeting,
    pub(crate) discord: Option<crate::discord_recording::DiscordRecording>,
    pub(crate) attendee: Option<crate::attendee_recording::AttendeeRecording>,
    recorder: Option<Recorder>,
    video: Option<VideoRecorder>,
    /// Feeds live ASR; shared with the forwarder so `stop` can close it from here
    /// (the only sender) instead of waiting for the capture channel to drain.
    pcm_tx: Arc<Mutex<Option<crossbeam_channel::Sender<Vec<f32>>>>>,
    live: Option<crate::stt_stage::SttWorker>,
    participant_live: Option<crate::participant_live::ParticipantSession>,
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

pub(crate) fn start(
    app: &AppHandle,
    opts: StartOptions,
    prepared: Option<crate::bot_connection::PreparedBot>,
    screen: Option<MonitorInfo>,
) -> Result<Meeting, String> {
    let _lifecycle = RECORDING_LIFECYCLE.lock();
    let state = app.state::<AppState>();
    let _copy = state.data_copy.try_read().ok_or("A library copy is in progress. Try recording again after it finishes.")?;
    if state.recording.lock().is_some() {
        return Err("already recording".into());
    }
    if let Some(prepared) = &prepared { prepared.validate()?; }
    // The user's selected source fixes the policy for the entire recording;
    // a failed/quiet bot must never silently enable mixed live ASR.
    let bot_provider = opts.bot_provider(prepared.as_ref().map(|bot| bot.provider()));
    let bot_only = bot_provider.is_some();
    let settings = crate::stt_stage::recording_settings(&state.settings.read(), bot_only);
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
    let dir = state.store.recordings_dir(&meeting.id);
    if let Some(provider) = bot_provider {
        crate::bot_connection::persist_live_policy(&dir, provider, settings.capture_system)?;
    }
    crate::stt_stage::prepare_selection(&state, &meeting.id, &settings)?;

    log::info!("recorder: opening audio devices");
    let (cap_tx, cap_rx) = crossbeam_channel::unbounded::<CaptureEvent>();
    let config = audio_writer_config(&dir, &settings);
    let recorder = Recorder::start(config, cap_tx).map_err(|e| e.to_string())?;

    // Optional screen video.
    let video = if let Some(screen) = &screen {
        match state.ffmpeg.as_ref() {
            Some(ffmpeg) => {
                match start_screen_writer(ffmpeg, &dir, &settings, screen) {
                    Ok(v) => {
                        meeting.has_video = true;
                        Some(v)
                    }
                    Err(e) => {
                        crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.screenStartFailed", format!("Screen recording failed to start: {e}. Recording audio only.")).with_detail(e.to_string()));
                        None
                    }
                }
            }
            None => {
                crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.recordingAudioOnly", "ffmpeg not found; recording audio only."));
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
    let mut participant_live = bot_only
        .then(|| crate::participant_live::ParticipantSession::new(app, &meeting.id));
    let named_input = participant_live.as_ref().map(|session| session.sender());
    let origin = recorder.time_origin();
    let (discord, attendee) = if let Some(prepared) = prepared {
        match prepared.adopt(&meeting.id, &dir, origin, named_input) {
            Ok(recordings) => recordings,
            Err(error) => {
                // A disconnect can race device startup after validation. Fail
                // this start, finalize its media and retain it for recovery.
                notes_stop.store(true, Ordering::Relaxed);
                if let Some(video) = video { let _ = video.stop(); }
                let output = recorder.stop().map_err(|error| error.to_string());
                conclude_meeting(&mut meeting, &output, &dir);
                meeting.status = MeetingStatus::Failed;
                meeting.error = Some(error.clone());
                let saved = state.store.update_processing_state(&meeting);
                crate::live_feed::LiveFeed::end(&state);
                saved.map_err(|error| error.to_string())?;
                return Err(error);
            }
        }
    } else {
        crate::bot_connection::cancel_pending(app);
        let discord = opts.discord.map(|selection| {
            crate::discord_recording::DiscordRecording::start(app, &meeting.id, &dir, selection, origin, named_input.clone())
        });
        let attendee = opts.attendee.map(|selection| {
            crate::attendee_recording::AttendeeRecording::start(app, &meeting.id, &dir, selection, origin, named_input)
        });
        (discord, attendee)
    };
    if let Some(live) = &mut participant_live { live.start(&settings); }
    let crate::stt_stage::LiveSession { input: pcm_tx, worker: live, captions, overflow } = crate::stt_stage::start_live(app, &meeting.id, &settings, !bot_only);

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
            let forward_pcm = |event| {
                if let CaptureEvent::Pcm16k(pcm) = event {
                    let guard = pcm_tx.lock();
                    let Some(tx) = guard.as_ref() else { return; };
                    if tx.try_send(pcm).is_err() {
                        if let Some(overflow) = &overflow { overflow.store(true, Ordering::Relaxed); }
                    }
                }
            };
            let mut next_video_check = std::time::Instant::now();
            loop {
                if stop.load(Ordering::Relaxed) {
                    // Recorder::stop has finalized the writer. Drain its queued
                    // last PCM frames without waiting for dead device senders.
                    while let Ok(event) = cap_rx.try_recv() {
                        forward_pcm(event);
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
                        crate::recorder::notice_message(&app, "warning", crate::ui_message::app("native.screenCaptureInterrupted", warning.clone()).with_detail(warning));
                    }
                }
                let ev = match cap_rx.recv_timeout(std::time::Duration::from_millis(300)) {
                    Ok(ev) => ev,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                };
                match ev {
                    CaptureEvent::Levels { mic_db, sys_db } => {
                        let _ = app.emit(events::LEVELS, LevelsPayload { mic_db, sys_db });
                    }
                    event @ (CaptureEvent::Pcm16k(_) | CaptureEvent::SystemPcm16k(_)) => forward_pcm(event),
                    notice @ (CaptureEvent::Warning(_) | CaptureEvent::Error(_) | CaptureEvent::AudioUnavailable { .. }
                        | CaptureEvent::AudioReconnected { .. }) => report_audio_notice(Some(&app), notice),
                    CaptureEvent::WriterError(message) => {
                        crate::recorder::notice_message(&app, "error", crate::ui_message::app("native.recordingStopped", format!("{message}. Recording is stopping; captured files will be kept.")).with_detail(message));
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
                    CaptureEvent::Stalled { mic, sys } => reopen_streams(&app, &meeting_id, mic, sys),
                }
            }
        })
    };

    *state.recording.lock() = Some(ActiveRecording {
        meeting: meeting.clone(),
        discord,
        attendee,
        recorder: Some(recorder),
        video,
        pcm_tx,
        live,
        participant_live,
        captions,
        forwarder: Some(forwarder),
        forwarder_stop,
        highlights: Vec::new(),
        notes_stop,
    });
    crate::bot_connection::cancel_pending(app);
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

/// Manual start: open source selection in the main window before capturing.
pub fn open_picker(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.recording.lock().is_some() {
        return;
    }
    windows::open_recording_setup(app);
}

fn reopen_streams(app: &AppHandle, meeting_id: &str, mic: bool, sys: bool) {
    let state = app.state::<AppState>();
    let mut guard = state.recording.lock();
    let Some(active) = guard.as_mut().filter(|active| active.meeting.id == meeting_id) else { return };
    let Some(rec) = active.recorder.as_mut() else { return };
    rec.reopen_stalled(mic, sys);
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
                let _ = state.store.update_processing_state(&m);
                continue;
            }
            log::info!("resuming interrupted processing of {}", m.id);
            crate::recorder::notice_message(app, "info", crate::ui_message::app("native.recoverMeeting", format!("Finishing \"{}\" from the last session.", m.title)).with_args(serde_json::json!({"title":m.title})));
            let id = m.id.clone();
            if let Err(error) = crate::processing_jobs::enqueue(app, id, move |app| crate::pipeline::post_process_recovery(app, m)) {
                log::warn!("recovery already queued: {error}");
            }
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
            let _ = state.store.update_processing_state(&m);
            continue;
        }
        let ended = std::fs::metadata(&mix).and_then(|md| md.modified()).map(chrono::DateTime::<chrono::Utc>::from).unwrap_or_else(|_| chrono::Utc::now());
        log::info!("recovering orphaned recording {} ({} ms)", m.id, duration);
        m.duration_ms = duration;
        m.ended_at = Some(ended);
        m.status = MeetingStatus::Processing;
        m.has_video = m.has_video || dir.join("screen.raw.mp4").exists() || dir.join("screen.mp4").exists();
        let _ = state.store.update_processing_state(&m);
        crate::recorder::notice_message(app, "info", crate::ui_message::app("native.recoverMeeting", format!("Finishing \"{}\" from the last session.", m.title)).with_args(serde_json::json!({"title":m.title})));
        let id = m.id.clone();
        if let Err(error) = crate::processing_jobs::enqueue(app, id, move |app| crate::pipeline::post_process_recovery(app, m)) {
            log::warn!("recovery already queued: {error}");
        }
    }
}

fn spawn_participant_sampler(app: AppHandle, meeting_id: String, app_id: Option<String>) {
    let Some(app_id) = app_id.filter(|a| a != "unknown") else { return };
    std::thread::spawn(move || {
        let patterns = bazza_detect::PatternSet::builtin();
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
            let windows = bazza_detect::list_visible_windows();
            let Some(det) = patterns.match_windows(&windows).filter(|d| d.app == app_id && d.hwnd != 0) else { continue };
            let raw = bazza_detect::scrape_window(det.hwnd);
            let names = bazza_detect::extract_names(&app_id, &raw);
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

/// Share the writer transition with service-owned capture. The video writer
/// persists its pending marker before invoking the audio transition.
pub(crate) fn set_capture_paused(
    recorder: &Recorder,
    video: Option<&mut VideoRecorder>,
    paused: bool,
) -> std::io::Result<()> {
    let transition = || {
        if paused {
            recorder.pause();
        } else {
            recorder.resume();
        }
    };
    match video {
        Some(video) if paused => video.pause_with(transition),
        Some(video) => video.resume_with(transition),
        None => {
            transition();
            Ok(())
        }
    }
}

pub fn pause(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let result = (|| {
        if let Some(a) = state.recording.lock().as_mut() {
            if let Some(r) = &a.recorder {
                let result = set_capture_paused(r, a.video.as_mut(), true);
                if let Some(live) = &a.participant_live {
                    live.set_paused(r.is_paused());
                }
                if let Some(discord) = &a.discord {
                    discord.set_paused(r.is_paused());
                }
                if let Some(attendee) = &a.attendee {
                    attendee.set_paused(r.is_paused());
                }
                result.map_err(|e| format!("Could not save video pause: {e}"))?;
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
                let result = set_capture_paused(r, a.video.as_mut(), false);
                if let Some(live) = &a.participant_live {
                    live.set_paused(r.is_paused());
                }
                if let Some(discord) = &a.discord {
                    discord.set_paused(r.is_paused());
                }
                if let Some(attendee) = &a.attendee {
                    attendee.set_paused(r.is_paused());
                }
                result.map_err(|e| format!("Could not save video resume: {e}"))?;
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
    if let Some(discord) = &active.discord {
        discord.request_stop();
    }
    if let Some(attendee) = &active.attendee {
        attendee.request_stop();
    }
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
            crate::recorder::notice_message(app, "warning", crate::ui_message::app("native.screenFinishFailed", format!("Screen recording did not finish cleanly ({error}). Audio and recoverable video were kept.")).with_detail(error.to_string()));
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
    if let Some(discord) = active.discord.take() {
        discord.finish();
    }
    if let Some(attendee) = active.attendee.take() {
        attendee.finish();
    }
    if let Some(live) = active.participant_live.take() {
        live.finish();
    }

    let mut meeting = active.meeting.clone();
    let dir = state.store.recordings_dir(&meeting.id);
    conclude_meeting(&mut meeting, &audio_result, &dir);
    if let Err(error) = crate::stt_stage::publish_completed_live(&state, &meeting.id, meeting.duration_ms) {
        // Media finalization and recovery still run if the transcript write failed.
        notice_message(app, "error", crate::ui_message::app(
            "native.transcriptSaveFailed", "Could not save the transcript.",
        ).with_detail(error));
    }
    let saved = state.store.update_processing_state(&meeting).map_err(|error| error.to_string());
    if !active.highlights.is_empty() {
        let _ = state.store.set_setting(&format!("highlights:{}", meeting.id), &active.highlights);
    }
    crate::live_feed::LiveFeed::end(&state);
    emit_state(app);
    saved?;
    audio_result?;

    let m2 = meeting.clone();
    crate::processing_jobs::enqueue(app, m2.id.clone(), move |app| crate::pipeline::post_process(app, m2))?;
    Ok(meeting)
}

fn conclude_meeting(meeting: &mut Meeting, audio_result: &Result<bazza_capture::RecordOutput, String>, dir: &Path) {
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

pub fn read_wav_as_16k_mono(path: &Path) -> Result<Vec<f32>, String> {
    bazza_capture::wav::read_mono_resampled(path, bazza_capture::ASR_HZ).map_err(|error| error.to_string())
}

pub(crate) fn default_title(app: Option<&str>) -> String {
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
    notice_message(app, level, message.into());
}

pub fn notice_message(app: &AppHandle, level: &'static str, message: crate::ui_message::UiMessage) {
    log::info!("[{level}] {message}");
    let _ = app.emit(events::NOTICE, NoticePayload { level, message });
}

pub(crate) fn report_audio_notice(app: Option<&AppHandle>, event: CaptureEvent) {
    use crate::ui_message::app as message;
    let (level, notice) = match event {
        CaptureEvent::Warning(detail) if !bazza_capture::recorder::is_fatal_stream_error(&detail) => {
            log::debug!("capture: {detail}");
            return;
        }
        CaptureEvent::Warning(detail) => ("warning", message("native.captureWarning", detail.clone()).with_detail(detail)),
        CaptureEvent::Error(detail) => { log::warn!("capture: {detail}"); return; }
        CaptureEvent::AudioUnavailable { system: true, detail } => ("warning", message(
            "native.systemAudioUnavailable", "System audio is unavailable. Bazza is still recording and will retry the output device.",
        ).with_detail(detail)),
        CaptureEvent::AudioUnavailable { system: false, detail } => ("error", message(
            "native.microphoneUnavailable", "The microphone is unavailable. Bazza will keep trying to reconnect it.",
        ).with_detail(detail)),
        CaptureEvent::AudioReconnected { system: true } => ("warning", message(
            "native.systemAudioReconnected", "The system audio device reconnected. Some system audio may be missing from the recording.",
        )),
        CaptureEvent::AudioReconnected { system: false } => ("warning", message(
            "native.microphoneReconnected", "The microphone reconnected. Some microphone audio may be missing from the recording.",
        )),
        _ => return,
    };
    if let Some(app) = app { notice_message(app, level, notice); }
    else { log::warn!("[{level}] {notice}"); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(index: u32, screen_id: &str) -> MonitorInfo {
        MonitorInfo {
            index, screen_id: screen_id.into(), name: screen_id.into(),
            x: 0, y: 0, width: 1920, height: 1080, primary: index == 0, device: index,
        }
    }

    #[test]
    fn video_requires_an_identified_screen_when_several_are_present() {
        let main = screen(0, "main");
        let secondary = screen(1, "secondary");
        let indexed = VideoTarget::Monitor { index: 0 };
        assert_eq!(select_recording_screen(vec![main.clone()], None, None), Ok(main.clone()));
        assert_eq!(select_recording_screen(vec![main.clone(), secondary.clone()], Some(&indexed), None), Err(ScreenSelectionError::Required));
        assert_eq!(select_recording_screen(vec![main.clone(), secondary.clone()], Some(&indexed), Some("secondary")), Ok(secondary));
        assert_eq!(select_recording_screen(vec![main], None, Some("disconnected")), Err(ScreenSelectionError::Unavailable));
        assert_eq!(select_recording_screen(Vec::new(), None, None), Err(ScreenSelectionError::Unavailable));
    }

    #[test]
    fn window_video_is_rejected_before_enumeration_but_audio_only_is_allowed() {
        let window = VideoTarget::Window { title: "Legacy window".into(), identity: None };
        assert_eq!(resolve_recording_screen(true, Some(&window), Some("screen"), None), Err(ScreenSelectionError::Window));
        assert_eq!(resolve_recording_screen(false, Some(&window), Some("screen"), None), Ok(None));
        assert!(serde_json::from_value::<StartOptions>(serde_json::json!({"window_id": "17"})).is_err());
    }

    #[test]
    fn audio_config_keeps_saved_devices_and_rejects_legacy_process_requests() {
        assert!(serde_json::from_value::<StartOptions>(serde_json::json!({"audio_pid": 42})).is_err());
        for capture_system in [true, false] {
            let settings: crate::settings::Settings = serde_json::from_value(serde_json::json!({
                "audio_scope": "app",
                "capture_system": capture_system,
                "mic_device": "saved microphone",
                "loopback_device": "saved output",
            })).unwrap();
            let config = audio_writer_config(Path::new("recording"), &settings);
            assert_eq!(config.capture_system, capture_system);
            assert_eq!(config.mic_device.as_deref(), Some("saved microphone"));
            assert_eq!(config.loopback_device.as_deref(), Some("saved output"));
        }
    }

    #[test]
    fn bot_policy_follows_selection_instead_of_app_labels_or_connection_health() {
        let ordinary = StartOptions { app: Some("meet".into()), ..StartOptions::default() };
        assert_eq!(ordinary.bot_provider(None), None);
        assert_eq!(ordinary.bot_provider(Some("meet")), Some("meet"));
        assert_eq!(ordinary.bot_provider(Some("discord")), Some("discord"));
        for (selection, expected) in [
            (serde_json::json!({"attendee": {"meetingUrl": "https://meet.google.com/abc-defg-hij"}}), "meet"),
            (serde_json::json!({"discord": {"guild_id": "1234", "channel_id": "5678"}}), "discord"),
        ] {
            // No connection object or received PCM is needed to fix the policy.
            let options: StartOptions = serde_json::from_value(selection).unwrap();
            assert_eq!(options.bot_provider(None), Some(expected));
        }
    }

    #[test]
    fn processing_admission_waits_for_start_and_stop_lifecycle_windows() {
        let lifecycle = RECORDING_LIFECYCLE.lock();
        assert!(try_recording_lifecycle().is_none());
        drop(lifecycle);
        assert!(try_recording_lifecycle().is_some());
    }
    use std::path::PathBuf;
    use std::time::Duration;

    struct FixtureDir(PathBuf);
    impl FixtureDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("bazza-recorder-stop-{}", uuid::Uuid::new_v4()));
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
            discord: None,
            attendee: None,
            pcm_tx: Arc::new(Mutex::new(None)), live: None, participant_live: None, captions: None, forwarder: None,
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
        let mut wav = bazza_capture::WavWriter::create(&dir.0.join("mix.wav"), 48_000, 1).unwrap();
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
        let output = bazza_capture::RecordOutput {
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

use super::CmdResult;
use crate::bot_connection::{BotConnections, RecordingBotStatus, RecordingBotTarget};
use crate::events::RecordingStatePayload;
use crate::recorder::{self, StartOptions};
use crate::recorder_service::ServiceCapture;
use crate::state::{AppState, RuntimeState};
use crate::tray;
use crate::windows;
use bazza_store::Meeting;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewWindow};
use uuid::Uuid;

fn capture_unavailable() -> crate::ui_message::UiMessage {
    crate::ui_message::app(
        "native.captureUnavailable",
        "Recording controls are unavailable.",
    )
}

#[tauri::command]
pub(crate) fn connect_recording_bot(
    window: WebviewWindow,
    request_id: Uuid,
    mut target: RecordingBotTarget,
) -> CmdResult<RecordingBotStatus> {
    let app = window.app_handle();
    if window.label() != "main" || recording_status(app.clone()).state != "idle" {
        return Err(capture_unavailable());
    }
    target.validate()?;
    let connections = app.try_state::<BotConnections>().ok_or_else(capture_unavailable)?;
    connections.connect(app, request_id, target)
}

#[tauri::command]
pub(crate) fn disconnect_recording_bot(window: WebviewWindow, request_id: Uuid) -> CmdResult<()> {
    if window.label() != "main" {
        return Err(capture_unavailable());
    }
    let app = window.app_handle();
    let connections = app.try_state::<BotConnections>().ok_or_else(capture_unavailable)?;
    connections.cancel(app, request_id);
    Ok(())
}

// Start and stop open/close audio devices and join capture threads; they run
// off the main thread so a slow or dead device can never freeze the window.
#[tauri::command]
pub async fn start_recording(window: WebviewWindow, opts: Option<StartOptions>) -> CmdResult<Meeting> {
    let main_window = window.label() == "main";
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut opts = opts.unwrap_or_default();
        if let Some(attendee) = &mut opts.attendee {
            attendee.meeting_url = bazza_attendee::normalize_meeting_url(&attendee.meeting_url)
                .map_err(|_| crate::ui_message::app(
                    "native.attendeeInvalidMeetingUrl",
                    "Enter a Google Meet link with a meeting code.",
                ))?;
            opts.app = Some("meet".into());
        }
        let (settings, ffmpeg) = if let Some(runtime) = app.try_state::<RuntimeState>() {
            let settings = runtime.settings.read().clone();
            (settings, runtime.ffmpeg.clone())
        } else {
            let state = app.try_state::<AppState>().ok_or_else(capture_unavailable)?;
            let settings = state.settings.read().clone();
            (settings, state.ffmpeg.clone())
        };
        let screen = recorder::resolve_recording_screen(
            opts.video.unwrap_or(settings.video_enabled),
            opts.target.as_ref(),
            opts.screen_id.as_deref(),
            ffmpeg.as_deref(),
        ).map_err(|error| {
            let (code, message) = error.message();
            crate::ui_message::app(code, message)
        })?;
        let prepared = if let Some(request_id) = opts.bot_connection_id.take() {
            if !main_window || opts.attendee.is_some() || opts.discord.is_some() {
                return Err(capture_unavailable());
            }
            let connections = app.try_state::<BotConnections>().ok_or_else(capture_unavailable)?;
            Some(connections.claim(&app, request_id, opts.app.as_deref()).ok_or_else(|| {
                crate::ui_message::app("setup.botConnectionLost", "The bot connection is unavailable.")
            })?)
        } else {
            None
        };
        if let Some(capture) = app.try_state::<ServiceCapture>() {
            let result = capture.start_desktop(opts, &settings, ffmpeg.as_deref(), &app, prepared, screen);
            capture.publish_desktop_state(&app, true);
            return result.map_err(Into::into);
        }
        app.try_state::<AppState>()
            .ok_or_else(capture_unavailable)?;
        let m = recorder::start(&app, opts, prepared, screen)?;
        tray::refresh(&app);
        Ok(m)
    })
    .await
    .map_err(super::err)?
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> CmdResult<Meeting> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(capture) = app.try_state::<ServiceCapture>() {
            let result = capture.stop_desktop(&app);
            capture.publish_desktop_state(&app, true);
            return result.map_err(Into::into);
        }
        app.try_state::<AppState>()
            .ok_or_else(capture_unavailable)?;
        let result = recorder::stop(&app).map_err(super::err);
        tray::refresh(&app);
        result
    })
    .await
    .map_err(super::err)?
}

#[tauri::command]
pub async fn pause_recording(app: AppHandle) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || change_pause(&app, true))
        .await
        .map_err(super::err)?
}

#[tauri::command]
pub async fn resume_recording(app: AppHandle) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || change_pause(&app, false))
        .await
        .map_err(super::err)?
}

fn change_pause(app: &AppHandle, paused: bool) -> CmdResult<()> {
    if let Some(capture) = app.try_state::<ServiceCapture>() {
        let result = capture.change_pause(paused);
        capture.publish_desktop_state(app, false);
        return result.map(|_| ()).map_err(Into::into);
    }
    app.try_state::<AppState>()
        .ok_or_else(capture_unavailable)?;
    if paused {
        recorder::pause(app).map_err(super::err)
    } else {
        recorder::resume(app).map_err(super::err)
    }
}

#[tauri::command]
pub fn mark_highlight(app: AppHandle) -> CmdResult<u64> {
    if app.try_state::<ServiceCapture>().is_some() {
        return Err(crate::ui_message::app(
            "native.serviceCaptureHighlightUnavailable",
            "Highlights are unavailable in the service desktop.",
        ));
    }
    app.try_state::<AppState>()
        .ok_or_else(capture_unavailable)?;
    recorder::mark_highlight(&app)
        .ok_or_else(|| crate::ui_message::app("native.notRecording", "not recording"))
}

#[tauri::command]
pub fn recording_status(app: AppHandle) -> RecordingStatePayload {
    if let Some(capture) = app.try_state::<ServiceCapture>() {
        return capture.recording_state();
    }
    app.try_state::<AppState>().map_or(
        RecordingStatePayload {
            state: "idle",
            meeting_id: None,
            elapsed_ms: 0,
        },
        |state| recorder::status_payload(&state),
    )
}

#[tauri::command]
pub fn discord_recording_status(app: AppHandle) -> Option<crate::discord_recording::DiscordRecordingStatus> {
    if let Some(capture) = app.try_state::<ServiceCapture>() {
        return capture.discord_recording_status();
    }
    let state = app.try_state::<AppState>()?;
    let recording = state.recording.lock();
    recording.as_ref()?.discord.as_ref().map(|discord| discord.status())
}

#[tauri::command]
pub fn attendee_recording_status(app: AppHandle) -> Option<crate::attendee_recording::AttendeeRecordingStatus> {
    if let Some(capture) = app.try_state::<ServiceCapture>() {
        return capture.attendee_recording_status();
    }
    let state = app.try_state::<AppState>()?;
    let recording = state.recording.lock();
    recording.as_ref()?.attendee.as_ref().map(|attendee| attendee.status())
}

/// Open the main window's source selection screen for a manual start.
#[tauri::command]
pub fn open_record_picker(app: AppHandle) {
    if let Some(capture) = app.try_state::<ServiceCapture>() {
        if capture.recording_state().state == "idle" {
            windows::open_recording_setup(&app);
        }
    } else if app.try_state::<AppState>().is_some() {
        recorder::open_picker(&app);
    }
}

/// Captions and running notes of the recording in progress (None when idle).
#[tauri::command]
pub fn live_snapshot(app: AppHandle) -> Option<crate::live_feed::LiveSnapshot> {
    if app.try_state::<ServiceCapture>().is_some() {
        return app.try_state::<crate::state::RuntimeState>()
            .and_then(|state| crate::live_feed::LiveFeed::snapshot(&state));
    }
    app.try_state::<AppState>()
        .and_then(|state| crate::live_feed::LiveFeed::snapshot(&state))
}

/// `mode`: "now" hides the popup; "never" also adds the app to the never-list;
/// "snooze" pauses detection for an hour.
#[tauri::command]
pub fn dismiss_detection(app: AppHandle, app_id: String, mode: String) -> CmdResult<()> {
    if app.try_state::<ServiceCapture>().is_some() {
        match mode.as_str() {
            "never" => {
                return Err(crate::ui_message::app(
                    "native.serviceCaptureDetectionUnavailable",
                    "Changing the detection list is unavailable in the service desktop.",
                ));
            }
            "snooze" => {
                let runtime = app
                    .try_state::<RuntimeState>()
                    .ok_or_else(capture_unavailable)?;
                *runtime.snooze_until.lock() = Some(Instant::now() + Duration::from_secs(3600));
            }
            _ => {}
        }
        windows::hide_popup(&app);
        return Ok(());
    }
    let state = app
        .try_state::<AppState>()
        .ok_or_else(capture_unavailable)?;
    match mode.as_str() {
        "never" => {
            let mut s = state.settings.write();
            if !s.never_apps.contains(&app_id) {
                s.never_apps.push(app_id);
            }
            s.save(&state.store).map_err(super::err)?;
        }
        "snooze" => {
            *state.snooze_until.lock() = Some(Instant::now() + Duration::from_secs(3600));
        }
        _ => {}
    }
    windows::hide_popup(&app);
    Ok(())
}

#[tauri::command]
pub fn open_main(app: AppHandle) {
    windows::focus_main(&app);
}

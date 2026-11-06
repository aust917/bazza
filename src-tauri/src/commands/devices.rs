use super::CmdResult;
use bazza_capture::AudioDevice;
use crate::state::{AppState, RuntimeState};
use serde::Serialize;
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
pub struct AudioDevices {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
}

#[tauri::command]
pub async fn list_audio_devices() -> CmdResult<AudioDevices> {
    tauri::async_runtime::spawn_blocking(|| AudioDevices {
        inputs: bazza_capture::input_devices(),
        outputs: bazza_capture::output_devices(),
    }).await.map_err(super::err)
}

/// Sample current mic / system levels for ~400 ms (onboarding and Settings meters).
/// Opening, sampling and closing native devices must never block the UI thread.
#[tauri::command]
pub async fn sample_levels(mic: Option<String>, loopback: Option<String>) -> CmdResult<(f32, f32)> {
    tauri::async_runtime::spawn_blocking(move || {
        bazza_capture::probe::sample_levels(mic.as_deref(), loopback.as_deref(), std::time::Duration::from_millis(400))
    }).await.map_err(super::err)
}

#[derive(Serialize)]
pub struct VideoSource {
    pub target: bazza_capture::VideoTarget,
    pub screen_id: String,
    pub label: String,
    pub is_primary: bool,
}

/// Displays available for video recording.
#[tauri::command]
pub async fn list_video_sources(app: AppHandle) -> CmdResult<Vec<VideoSource>> {
    let ffmpeg = if let Some(runtime) = app.try_state::<RuntimeState>() {
        runtime.ffmpeg.clone()
    } else {
        app.state::<AppState>().ffmpeg.clone()
    };
    tauri::async_runtime::spawn_blocking(move || video_sources(ffmpeg.as_deref()))
        .await.map_err(super::err)
}

fn video_sources(ffmpeg: Option<&std::path::Path>) -> Vec<VideoSource> {
    bazza_capture::list_monitors(ffmpeg)
        .into_iter()
        .map(|m| VideoSource {
            label: if m.width > 0 { format!("{} ({}x{})", m.name, m.width, m.height) } else { m.name },
            is_primary: m.primary,
            target: bazza_capture::VideoTarget::Monitor { index: m.index },
            screen_id: m.screen_id,
        })
        .collect()
}

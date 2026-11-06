//! Event names and payloads emitted to every window.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const DETECTION: &str = "detection";
pub const LEVELS: &str = "levels";
pub const CAPTION: &str = "caption";
pub const RECORDING_STATE: &str = "recording_state";
pub const MODEL_PROGRESS: &str = "model_progress";
pub const PROCESSING: &str = "processing";
pub const NOTICE: &str = "notice";
pub const CHAT_TOKEN: &str = "chat_token";
pub const CHAT_DONE: &str = "chat_done";
pub const PARTICIPANTS: &str = "participants";
pub const LIVE_NOTES: &str = "live_notes";
pub const UI_LOCALE_CHANGED: &str = "ui-locale-changed";

#[derive(Serialize, Clone)]
pub struct UiLocalePayload {
    pub locale: crate::ui_locale::UiLocale,
}

#[derive(Serialize, Clone)]
pub struct ParticipantsPayload {
    pub meeting_id: String,
    pub participants: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct DetectionPayload {
    pub app: String,
    pub label: String,
    pub title: String,
    pub confidence: f32,
    /// Title of the calendar event happening now, if any.
    pub event: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct LevelsPayload {
    pub mic_db: f32,
    pub sys_db: f32,
}

#[derive(Serialize, Clone)]
pub struct RecordingStatePayload {
    pub state: &'static str,
    pub meeting_id: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Serialize, Clone)]
pub struct ModelProgressPayload {
    pub id: String,
    pub done: u64,
    pub total: u64,
    pub status: &'static str,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct ProcessingPayload {
    pub meeting_id: String,
    pub stage: &'static str,
    pub progress: f32,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TranscriptionPhase {
    Reading,
    Loading,
    Decoding,
}

#[derive(Clone, Serialize)]
struct TranscriptionProcessingPayload<'a> {
    #[serde(flatten)]
    processing: ProcessingPayload,
    transcription: TranscriptionProgress<'a>,
}

#[derive(Clone, Serialize)]
struct TranscriptionProgress<'a> {
    phase: TranscriptionPhase,
    progress: Option<f32>,
    model: Option<&'a str>,
}

/// Keep the aggregate stage interval used by other processing consumers.
/// A separate fraction gives the transcription UI the engine's actual progress.
pub(crate) fn emit_transcription(
    app: &AppHandle,
    meeting_id: &str,
    phase: TranscriptionPhase,
    model: Option<&str>,
    percent: Option<u8>,
) {
    let progress = percent.map(|percent| f32::from(percent) / 100.0);
    let _ = app.emit(PROCESSING, TranscriptionProcessingPayload {
        processing: ProcessingPayload {
            meeting_id: meeting_id.to_owned(),
            stage: "transcribe",
            progress: 0.1 + 0.35 * progress.unwrap_or(0.0),
            error: None,
        },
        transcription: TranscriptionProgress { phase, progress, model },
    });
}

#[derive(Serialize, Clone)]
pub struct NoticePayload {
    pub level: &'static str,
    #[serde(flatten)]
    pub message: crate::ui_message::UiMessage,
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MeetingStatus {
    Recording,
    Processing,
    Ready,
    Failed,
}

impl MeetingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub app: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: i64,
    pub has_video: bool,
    pub status: MeetingStatus,
    pub folder_id: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Last processing error (cleared when a re-run succeeds).
    #[serde(default)]
    pub error: Option<String>,
    /// True until the user renames the meeting; auto-titles only replace auto titles.
    #[serde(default = "default_true")]
    pub title_auto: bool,
    /// Attendee names seen in the meeting window or calendar event.
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub calendar_uid: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Meeting {
    /// A fresh meeting that is being recorded right now.
    pub fn new_recording(title: &str, app: Option<&str>) -> Meeting {
        let now = Utc::now();
        Meeting {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            app: app.map(str::to_string),
            started_at: now,
            ended_at: None,
            duration_ms: 0,
            has_video: false,
            status: MeetingStatus::Recording,
            folder_id: None,
            created_at: now,
            error: None,
            title_auto: true,
            participants: Vec::new(),
            calendar_uid: None,
        }
    }
}

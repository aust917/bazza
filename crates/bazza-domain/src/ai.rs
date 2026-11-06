use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub description: String,
    pub body: String,
    pub builtin: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Summary {
    pub meeting_id: String,
    pub template_id: String,
    pub markdown: String,
    pub structured: serde_json::Value,
    pub model: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: String,
    /// "meeting" | "all" (folders arrive in M4)
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub title: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub id: String,
    pub chat_id: String,
    pub role: String,
    pub content: String,
    pub citations: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChunkHit {
    pub meeting_id: String,
    pub meeting_title: String,
    pub started_at: DateTime<Utc>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub rank: f64,
}

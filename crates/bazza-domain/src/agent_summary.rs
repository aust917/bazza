//! Provenance and input revisions for an explicitly requested agent summary.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryProvider {
    Codex,
    Claude,
}

impl SummaryProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSummaryCommit {
    pub meeting_id: String,
    pub expected_lifecycle_revision: String,
    pub expected_transcript_revision: String,
    pub expected_source_id: String,
    pub expected_summary_revision: String,
    pub markdown: String,
    pub structured_json: serde_json::Value,
    pub template_id: String,
    pub model: String,
    pub provider: SummaryProvider,
}

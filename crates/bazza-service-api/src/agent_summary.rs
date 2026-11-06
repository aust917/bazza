//! Explicit desktop summary commit. This private RPC is separate from MCP and worker uploads.
use crate::desktop::{desktop_body_fits, desktop_id, desktop_revision, DesktopMutationOutcome};
use bazza_domain::agent_summary::AgentSummaryCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// The transport caps the full framed JSON at 128 KiB. Keep room for its envelope.
pub const MAX_AGENT_SUMMARY_REQUEST_BYTES: usize = 120 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitDesktopAgentSummaryRequest {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub idempotency_key: String,
    pub result: AgentSummaryCommit,
}

impl CommitDesktopAgentSummaryRequest {
    pub fn valid(&self) -> bool {
        let result = &self.result;
        !self.library_id.is_nil()
            && !self.library_epoch.is_nil()
            && desktop_revision(&self.idempotency_key)
            && desktop_id(&result.meeting_id)
            && desktop_revision(&result.expected_lifecycle_revision)
            && desktop_revision(&result.expected_transcript_revision)
            && desktop_id(&result.expected_source_id)
            && desktop_revision(&result.expected_summary_revision)
            && desktop_id(&result.template_id)
            && !result.model.trim().is_empty()
            && result.model.len() <= 256
            && !result.model.contains('\0')
            && !result.markdown.trim().is_empty()
            && !result.markdown.contains('\0')
            && result.structured_json.is_object()
            && desktop_body_fits(self, MAX_AGENT_SUMMARY_REQUEST_BYTES)
    }

    pub fn accepts(&self, outcome: &DesktopMutationOutcome) -> bool {
        outcome.entity_id == self.result.meeting_id
            && desktop_revision(&outcome.revision)
            && outcome.revision != self.result.expected_summary_revision
            && (1..=9_007_199_254_740_991).contains(&outcome.change_sequence)
            && outcome.job_id.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_domain::agent_summary::SummaryProvider;

    #[test]
    fn rejects_oversized_escaped_payloads_and_stale_receipts() {
        let mut request = CommitDesktopAgentSummaryRequest {
            library_id: Uuid::new_v4(),
            library_epoch: Uuid::new_v4(),
            idempotency_key: "save-1".into(),
            result: AgentSummaryCommit {
                meeting_id: "meeting-1".into(),
                expected_lifecycle_revision: "lifecycle-1".into(),
                expected_transcript_revision: "transcript-1".into(),
                expected_source_id: "source-1".into(),
                expected_summary_revision: "summary-1".into(),
                markdown: "Синтетический итог".into(),
                structured_json: serde_json::json!({}),
                template_id: "general".into(),
                model: "test-model".into(),
                provider: SummaryProvider::Claude,
            },
        };
        assert!(request.valid());
        let mut receipt = DesktopMutationOutcome {
            entity_id: request.result.meeting_id.clone(),
            revision: request.result.expected_summary_revision.clone(),
            change_sequence: 1,
            job_id: None,
            dedup_until: chrono::Utc::now(),
        };
        assert!(!request.accepts(&receipt));
        receipt.revision = "summary-2".into();
        assert!(request.accepts(&receipt));
        request.result.markdown = "\t".repeat(MAX_AGENT_SUMMARY_REQUEST_BYTES / 2);
        request.result.markdown.push('x');
        assert!(request.result.markdown.len() < MAX_AGENT_SUMMARY_REQUEST_BYTES);
        assert!(!request.valid());
    }
}

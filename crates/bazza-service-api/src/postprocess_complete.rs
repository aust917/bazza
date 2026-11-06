//! Terminal receipt for the private fixture post-process executor.
use crate::{
    desktop::{desktop_body_fits, desktop_revision},
    postprocess::{PostProcessJobLease, PostProcessSourceFence},
    worker::{WorkerProof, WorkerSecret},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_COMPLETE_CAPABILITY: &str =
    "experimental_fixture_post_process_complete";
const MAX_WIRE_BYTES: usize = 4096;
const MAX_CHANGE_SEQUENCE: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteFixturePostProcessRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
}

impl CompleteFixturePostProcessRequest {
    pub fn valid(&self) -> bool {
        self.lease.valid()
            && self.lease.worker_session_id == self.proof.session_id
            && self.lease.library_epoch == self.proof.library_epoch
            && self.lease.input.source_id.is_some()
            && !self.operation_id.is_nil()
            && desktop_body_fits(self, MAX_WIRE_BYTES)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureCleanupOutcome {
    Accepted,
    SkippedEmptyTranscript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureExcludedStage {
    OutsideFixtureScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureSummaryOutcome {
    Committed { revision: String },
    OutsideFixtureScope {},
}

/// A replay returns the historical terminal result. Its source fence is
/// evidence of the completed input and grants no authority to act on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteFixturePostProcessReceipt {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub input: PostProcessSourceFence,
    pub cleanup: FixtureCleanupOutcome,
    pub embeddings: FixtureExcludedStage,
    pub summary: FixtureSummaryOutcome,
    pub webhook: FixtureExcludedStage,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

impl CompleteFixturePostProcessReceipt {
    pub fn valid_for(&self, request: &CompleteFixturePostProcessRequest) -> bool {
        let lease = &request.lease;
        request.valid()
            && self.operation_id == request.operation_id
            && self.claim_id == lease.claim_id
            && self.worker_session_id == lease.worker_session_id
            && self.library_epoch == lease.library_epoch
            && self.job_id == lease.job_id
            && self.meeting_id == lease.meeting_id
            && desktop_revision(&self.job_revision)
            && self.job_revision != lease.job_revision
            && self.input == lease.input
            && match &self.summary {
                FixtureSummaryOutcome::Committed { revision } => desktop_revision(revision),
                FixtureSummaryOutcome::OutsideFixtureScope {} => true,
            }
            && self.change_sequence > lease.change_sequence
            && self.change_sequence <= MAX_CHANGE_SEQUENCE
            && self.dedup_until >= lease.expires_at
            && self.dedup_until
                <= Utc::now() + Duration::seconds(crate::DEDUP_RETENTION_SECONDS as i64 + 5)
            && desktop_body_fits(self, MAX_WIRE_BYTES)
    }
}

#[cfg(test)]
#[path = "postprocess_complete_tests.rs"]
mod tests;

//! Close the mandatory fixture steps without completing optional execution.
use crate::{
    desktop::desktop_body_fits,
    postprocess::PostProcessJobLease,
    worker::{WorkerProof, WorkerSecret},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_READY_CAPABILITY: &str = "experimental_fixture_post_process_ready";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessReadyRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
}

impl PostProcessReadyRequest {
    pub fn valid(&self) -> bool {
        self.lease.valid()
            && self.lease.worker_session_id == self.proof.session_id
            && self.lease.library_epoch == self.proof.library_epoch
            && self.lease.input.source_id.is_some()
            && !self.operation_id.is_nil()
            && desktop_body_fits(self, 4096)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessReadyReceipt {
    pub lease: PostProcessJobLease,
}

impl PostProcessReadyReceipt {
    pub fn valid_for(&self, request: &PostProcessReadyRequest) -> bool {
        let old = &request.lease;
        let new = &self.lease;
        request.valid()
            && new.valid()
            && new.operation_id == request.operation_id
            && new.claim_id == old.claim_id
            && new.worker_session_id == old.worker_session_id
            && new.library_epoch == old.library_epoch
            && new.job_id == old.job_id
            && new.meeting_id == old.meeting_id
            && new.job_revision != old.job_revision
            && new.input.capture_id == old.input.capture_id
            && new.input.capture_generation == old.input.capture_generation
            && new.input.checkpoint_revision != old.input.checkpoint_revision
            && new.input.lifecycle_revision != old.input.lifecycle_revision
            && new.input.transcript_revision == old.input.transcript_revision
            && new.input.source_id == old.input.source_id
            && new.expires_at == old.expires_at
            && new.change_sequence > old.change_sequence
            && desktop_body_fits(self, 4096)
    }
}

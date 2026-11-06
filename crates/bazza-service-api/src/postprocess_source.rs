//! Private staged automatic local ASR output. Transfer shapes and limits are
//! shared with cleanup, while a distinct RPC kind and receipt fence the result.
use crate::{
    desktop::{desktop_body_fits, MAX_DESKTOP_DATA_BYTES},
    postprocess::PostProcessJobLease,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::postprocess_cleanup::{
    PostProcessCleanupOperation as PostProcessSourceOperation,
    PostProcessCleanupRequest as PostProcessSourceRequest,
    PostProcessCleanupStatus as PostProcessSourceStatus,
    PostProcessCleanupUploadState as PostProcessSourceUploadState,
    MAX_POST_PROCESS_CLEANUP_BODY_BYTES as MAX_POST_PROCESS_SOURCE_BODY_BYTES,
    MAX_POST_PROCESS_CLEANUP_CHUNKS as MAX_POST_PROCESS_SOURCE_CHUNKS,
    MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES as MAX_POST_PROCESS_SOURCE_REQUEST_BYTES,
};
pub use bazza_domain::local_asr::{
    LocalAsrBody, LocalAsrCaption, LocalAsrOutcome, MAX_LOCAL_ASR_CAPTIONS,
};

pub const FIXTURE_POST_PROCESS_SOURCE_CAPABILITY: &str = "experimental_fixture_post_process_source";
const _: () = assert!(
    MAX_POST_PROCESS_SOURCE_BODY_BYTES == bazza_domain::local_asr::MAX_LOCAL_ASR_BODY_BYTES
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSourceReceipt {
    pub operation_id: Uuid,
    pub upload_id: Uuid,
    pub lease: PostProcessJobLease,
    pub outcome: LocalAsrOutcome,
    pub segment_count: u32,
}

impl PostProcessSourceReceipt {
    pub fn valid_for(&self, request: &PostProcessSourceRequest) -> bool {
        let old = &request.lease;
        let new = &self.lease;
        matches!(request.operation, PostProcessSourceOperation::Commit { .. })
            && self.operation_id == request.operation_id
            && self.upload_id == request.operation.upload_id()
            && new.valid()
            && new.operation_id == request.operation_id
            && new.claim_id == old.claim_id
            && new.worker_session_id == request.proof.session_id
            && new.library_epoch == request.proof.library_epoch
            && new.job_id == old.job_id
            && new.meeting_id == old.meeting_id
            && new.job_revision != old.job_revision
            && new.input.capture_id == old.input.capture_id
            && new.input.capture_generation == old.input.capture_generation
            && new.input.lifecycle_revision == old.input.lifecycle_revision
            && new.input.source_id.is_some()
            && new.input.source_id != old.input.source_id
            && new.input.checkpoint_revision != old.input.checkpoint_revision
            && new.input.transcript_revision != old.input.transcript_revision
            && new.expires_at == old.expires_at
            && new.change_sequence > old.change_sequence
            && self.segment_count as usize <= MAX_LOCAL_ASR_CAPTIONS
            && (self.outcome == LocalAsrOutcome::Complete || self.segment_count == 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostProcessSourceReply {
    Upload(PostProcessSourceStatus),
    Committed(PostProcessSourceReceipt),
}

impl PostProcessSourceReply {
    pub fn valid_for(&self, request: &PostProcessSourceRequest) -> bool {
        request.valid()
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
            && match self {
                Self::Upload(status) => {
                    status.valid_for(request)
                        && !(matches!(request.operation, PostProcessSourceOperation::Commit { .. })
                            && status.state == PostProcessSourceUploadState::Committed)
                }
                Self::Committed(receipt) => receipt.valid_for(request),
            }
    }
}

#[cfg(test)]
#[path = "postprocess_source_tests.rs"]
mod tests;

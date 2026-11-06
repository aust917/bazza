//! Publish one generated summary from a live fixture worker after cleanup.
use crate::{
    desktop::{desktop_body_fits, desktop_revision},
    postprocess::PostProcessSourceFence,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::postprocess_cleanup::{
    PostProcessCleanupOperation as PostProcessSummaryOperation,
    PostProcessCleanupRequest as PostProcessSummaryRequest,
    PostProcessCleanupStatus as PostProcessSummaryStatus,
    PostProcessCleanupUploadState as PostProcessSummaryUploadState,
};
pub use bazza_domain::worker_summary::{WorkerGeneratedSummary, MAX_WORKER_SUMMARY_BODY_BYTES};

pub const FIXTURE_POST_PROCESS_SUMMARY_CAPABILITY: &str =
    "experimental_fixture_post_process_summary";

pub fn post_process_summary_request_valid(request: &PostProcessSummaryRequest) -> bool {
    request.lease.input.source_id.is_some()
        && request.valid()
        && match &request.operation {
            PostProcessSummaryOperation::Begin { total_bytes, .. } => {
                *total_bytes <= MAX_WORKER_SUMMARY_BODY_BYTES
            }
            PostProcessSummaryOperation::Append {
                byte_offset, text, ..
            } => byte_offset
                .checked_add(text.len() as u64)
                .is_some_and(|end| end <= MAX_WORKER_SUMMARY_BODY_BYTES),
            _ => true,
        }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSummaryReceipt {
    pub operation_id: Uuid,
    pub upload_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub input: PostProcessSourceFence,
    pub summary_revision: String,
    pub change_sequence: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostProcessSummaryReply {
    Upload(PostProcessSummaryStatus),
    Committed(PostProcessSummaryReceipt),
}

impl PostProcessSummaryReply {
    pub fn valid_for(&self, request: &PostProcessSummaryRequest) -> bool {
        post_process_summary_request_valid(request)
            && desktop_body_fits(self, 4096)
            && match self {
                Self::Upload(status) => {
                    status.total_bytes <= MAX_WORKER_SUMMARY_BODY_BYTES
                        && status.valid_for(request)
                        && !(matches!(
                            request.operation,
                            PostProcessSummaryOperation::Commit { .. }
                        ) && status.state == PostProcessSummaryUploadState::Committed)
                }
                Self::Committed(receipt) => {
                    let lease = &request.lease;
                    matches!(
                        request.operation,
                        PostProcessSummaryOperation::Commit { .. }
                    ) && receipt.operation_id == request.operation_id
                        && receipt.upload_id == request.operation.upload_id()
                        && receipt.claim_id == lease.claim_id
                        && receipt.worker_session_id == lease.worker_session_id
                        && receipt.library_epoch == lease.library_epoch
                        && receipt.job_id == lease.job_id
                        && receipt.meeting_id == lease.meeting_id
                        && receipt.job_revision == lease.job_revision
                        && receipt.input == lease.input
                        && desktop_revision(&receipt.summary_revision)
                        && receipt.change_sequence > lease.change_sequence
                        && receipt.change_sequence <= 9_007_199_254_740_991
                }
            }
    }
}

//! Private staged anonymous speaker results. The distinct RPC kind binds a
//! shared upload to this operation, and the receipt pins the selected source.
use crate::{
    desktop::{desktop_body_fits, MAX_DESKTOP_DATA_BYTES},
    postprocess::PostProcessJobLease,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::postprocess_cleanup::{
    PostProcessCleanupOperation as PostProcessSpeakersOperation,
    PostProcessCleanupRequest as PostProcessSpeakersRequest,
    PostProcessCleanupStatus as PostProcessSpeakersStatus,
    PostProcessCleanupUploadState as PostProcessSpeakersUploadState,
    MAX_POST_PROCESS_CLEANUP_BODY_BYTES as MAX_POST_PROCESS_SPEAKERS_BODY_BYTES,
    MAX_POST_PROCESS_CLEANUP_CHUNKS as MAX_POST_PROCESS_SPEAKERS_CHUNKS,
    MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES as MAX_POST_PROCESS_SPEAKERS_REQUEST_BYTES,
};
pub use bazza_domain::speaker_result::{
    SpeakerAssignment, SpeakerResultBody, SpeakerResultOutcome, SpeakerSkipReason,
    MAX_SPEAKER_ASSIGNMENTS,
};

pub const FIXTURE_POST_PROCESS_SPEAKERS_CAPABILITY: &str =
    "experimental_fixture_post_process_speakers";
const _: () = assert!(
    MAX_POST_PROCESS_SPEAKERS_BODY_BYTES
        == bazza_domain::speaker_result::MAX_SPEAKER_RESULT_BODY_BYTES
);

pub fn post_process_speakers_request_valid(request: &PostProcessSpeakersRequest) -> bool {
    request.lease.input.source_id.is_some() && request.valid()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSpeakersReceipt {
    pub operation_id: Uuid,
    pub upload_id: Uuid,
    pub lease: PostProcessJobLease,
    pub outcome: SpeakerResultOutcome,
    pub changed_segments: u32,
}

impl PostProcessSpeakersReceipt {
    fn valid_for(&self, request: &PostProcessSpeakersRequest) -> bool {
        let old = &request.lease;
        let new = &self.lease;
        let transcript_changed = new.input.transcript_revision != old.input.transcript_revision;
        matches!(
            request.operation,
            PostProcessSpeakersOperation::Commit { .. }
        ) && self.operation_id == request.operation_id
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
            && new.input.source_id == old.input.source_id
            && new.input.checkpoint_revision != old.input.checkpoint_revision
            && new.expires_at == old.expires_at
            && new.change_sequence > old.change_sequence
            && self.outcome.valid_changed_segments(self.changed_segments)
            && transcript_changed == (self.changed_segments > 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostProcessSpeakersReply {
    Upload(PostProcessSpeakersStatus),
    Committed(PostProcessSpeakersReceipt),
}

impl PostProcessSpeakersReply {
    pub fn valid_for(&self, request: &PostProcessSpeakersRequest) -> bool {
        post_process_speakers_request_valid(request)
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
            && match self {
                Self::Upload(status) => {
                    status.valid_for(request)
                        && !(matches!(
                            request.operation,
                            PostProcessSpeakersOperation::Commit { .. }
                        ) && status.state == PostProcessSpeakersUploadState::Committed)
                }
                Self::Committed(receipt) => receipt.valid_for(request),
            }
    }
}

#[cfg(test)]
#[path = "postprocess_speakers_tests.rs"]
mod tests;

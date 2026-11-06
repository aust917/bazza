//! Private video publication under an existing worker-owned media grant.
use crate::postprocess_media::PostProcessMediaRequest;
pub use bazza_domain::capture_screen::{PostProcessVideoFailure, PostProcessVideoOutcome};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_VIDEO_CAPABILITY: &str = "experimental_fixture_post_process_video";
pub const MAX_POST_PROCESS_VIDEO_CALL_MS: u64 = 60_000;
pub const MAX_VIDEO_BYTES: u64 = 9_007_199_254_740_991;

/// The path is derived independently at both ends and never arrives on wire.
pub fn video_staging_name(claim_id: Uuid, owner_nonce: Uuid) -> String {
    format!(".bazza-video-{claim_id}-{owner_nonce}.mp4")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostProcessVideoAction {
    Publish {},
    Unavailable { reason: PostProcessVideoFailure },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessVideoRequest {
    pub media: PostProcessMediaRequest,
    pub result: PostProcessVideoAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessVideoReceipt {
    pub claim_id: Uuid,
    pub job_id: String,
    pub job_revision: String,
    pub meeting_id: Uuid,
    pub capture_id: Uuid,
    pub change_sequence: i64,
    pub outcome: PostProcessVideoOutcome,
}

impl PostProcessVideoReceipt {
    pub fn valid_for(&self, request: &PostProcessVideoRequest) -> bool {
        let lease = &request.media.lease;
        request.media.valid()
            && self.claim_id == lease.claim_id
            && self.job_id == lease.job_id
            && self.job_revision == lease.job_revision
            && self.meeting_id == lease.meeting_id
            && self.capture_id == lease.input.capture_id
            && (1..=MAX_VIDEO_BYTES as i64).contains(&self.change_sequence)
            && match (request.result, self.outcome) {
                (
                    PostProcessVideoAction::Publish {},
                    PostProcessVideoOutcome::Published { bytes },
                ) => (1..=MAX_VIDEO_BYTES).contains(&bytes),
                (
                    PostProcessVideoAction::Unavailable { reason },
                    PostProcessVideoOutcome::Unavailable { reason: recorded },
                ) => reason == recorded,
                _ => false,
            }
    }
}

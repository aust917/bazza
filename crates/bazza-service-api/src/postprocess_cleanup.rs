//! Private staged automatic overlays. Upload bodies never travel in receipts,
//! and receiving historical metadata does not renew or rebind a worker claim.
use crate::{
    desktop::{desktop_body_fits, desktop_revision, MAX_DESKTOP_DATA_BYTES},
    postprocess::PostProcessJobLease,
    worker::{WorkerProof, WorkerSecret},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_CLEANUP_CAPABILITY: &str =
    "experimental_fixture_post_process_cleanup";
pub const MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES: usize = 32 * 1024;
pub const MAX_POST_PROCESS_CLEANUP_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_POST_PROCESS_CLEANUP_UPDATES: usize = 10_000;
pub const MAX_POST_PROCESS_CLEANUP_CHUNKS: u64 = 4096;
pub const POST_PROCESS_CLEANUP_TTL_SECONDS: i64 = 3600;
pub const MAX_POST_PROCESS_CLEANUP_OPEN_UPLOADS: u16 = 16;
pub const MAX_POST_PROCESS_CLEANUP_RESERVED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SAFE: i64 = 9_007_199_254_740_991;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessCleanupRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
    pub operation: PostProcessCleanupOperation,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostProcessCleanupOperation {
    Begin {
        upload_id: Uuid,
        total_bytes: u64,
        sha256: String,
    },
    Append {
        upload_id: Uuid,
        sequence: u64,
        byte_offset: u64,
        text: String,
        sha256: String,
    },
    Status {
        upload_id: Uuid,
    },
    Commit {
        upload_id: Uuid,
    },
    Abort {
        upload_id: Uuid,
    },
}

impl PostProcessCleanupOperation {
    pub fn upload_id(&self) -> Uuid {
        match self {
            Self::Begin { upload_id, .. }
            | Self::Append { upload_id, .. }
            | Self::Status { upload_id }
            | Self::Commit { upload_id }
            | Self::Abort { upload_id } => *upload_id,
        }
    }
}

impl std::fmt::Debug for PostProcessCleanupOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Begin {
                upload_id,
                total_bytes,
                ..
            } => f
                .debug_struct("Begin")
                .field("upload_id", upload_id)
                .field("total_bytes", total_bytes)
                .finish_non_exhaustive(),
            Self::Append {
                upload_id,
                sequence,
                byte_offset,
                text,
                ..
            } => f
                .debug_struct("Append")
                .field("upload_id", upload_id)
                .field("sequence", sequence)
                .field("byte_offset", byte_offset)
                .field("fragment_bytes", &text.len())
                .finish_non_exhaustive(),
            Self::Status { upload_id } => f
                .debug_struct("Status")
                .field("upload_id", upload_id)
                .finish(),
            Self::Commit { upload_id } => f
                .debug_struct("Commit")
                .field("upload_id", upload_id)
                .finish(),
            Self::Abort { upload_id } => f
                .debug_struct("Abort")
                .field("upload_id", upload_id)
                .finish(),
        }
    }
}

impl std::fmt::Debug for PostProcessCleanupRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostProcessCleanupRequest")
            .field("proof", &self.proof)
            .field("lease", &self.lease)
            .field("fence", &self.fence)
            .field("operation_id", &self.operation_id)
            .field("operation", &self.operation)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessCleanupUpdate {
    pub segment_id: i64,
    pub text: String,
}

impl std::fmt::Debug for PostProcessCleanupUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostProcessCleanupUpdate")
            .field("segment_id", &self.segment_id)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

impl PostProcessCleanupUpdate {
    pub fn valid(&self) -> bool {
        (1..=MAX_SAFE).contains(&self.segment_id)
    }
}

/// Validate decoded shape/count/budget without sorting, deduplicating or changing
/// text. Store must verify the original exact stream length/hash before decode.
/// Duplicate IDs retain input order for last-wins application; missing IDs and
/// manual overlays are resolved only by the scoped commit transaction.
pub fn post_process_cleanup_body_valid(updates: &[PostProcessCleanupUpdate]) -> bool {
    updates.len() <= MAX_POST_PROCESS_CLEANUP_UPDATES
        && updates.iter().all(PostProcessCleanupUpdate::valid)
        && desktop_body_fits(&updates, MAX_POST_PROCESS_CLEANUP_BODY_BYTES as usize)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessCleanupUploadState {
    Open,
    Committed,
    Aborted,
    Expired,
    Invalidated,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessCleanupStatus {
    pub upload_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub checkpoint_revision: String,
    pub state: PostProcessCleanupUploadState,
    pub total_bytes: u64,
    pub received_bytes: u64,
    pub next_sequence: u64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessCleanupOutcome {
    Applied,
    Noop,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessCleanupReceipt {
    pub operation_id: Uuid,
    pub upload_id: Uuid,
    pub lease: PostProcessJobLease,
    pub changed_segments: u32,
    pub outcome: PostProcessCleanupOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostProcessCleanupReply {
    Upload(PostProcessCleanupStatus),
    Committed(PostProcessCleanupReceipt),
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl PostProcessCleanupRequest {
    pub fn valid(&self) -> bool {
        self.lease.valid()
            && !self.proof.session_id.is_nil()
            && !self.proof.library_epoch.is_nil()
            && self.proof.session_id == self.lease.worker_session_id
            && self.proof.library_epoch == self.lease.library_epoch
            && !self.operation_id.is_nil()
            && !self.operation.upload_id().is_nil()
            && match &self.operation {
                PostProcessCleanupOperation::Begin {
                    total_bytes,
                    sha256: hash,
                    ..
                } => {
                    (1..=MAX_POST_PROCESS_CLEANUP_BODY_BYTES).contains(total_bytes) && sha256(hash)
                }
                PostProcessCleanupOperation::Append {
                    sequence,
                    byte_offset,
                    text,
                    sha256: hash,
                    ..
                } => {
                    (1..=MAX_POST_PROCESS_CLEANUP_CHUNKS).contains(sequence)
                        && !text.is_empty()
                        && byte_offset
                            .checked_add(text.len() as u64)
                            .is_some_and(|end| end <= MAX_POST_PROCESS_CLEANUP_BODY_BYTES)
                        && sha256(hash)
                }
                _ => true,
            }
            && desktop_body_fits(self, MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES)
    }
}

impl PostProcessCleanupStatus {
    pub fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        if self.upload_id != request.operation.upload_id()
            || self.claim_id != request.lease.claim_id
            || self.worker_session_id != request.proof.session_id
            || self.library_epoch != request.proof.library_epoch
            || self.job_id != request.lease.job_id
            || self.meeting_id != request.lease.meeting_id
            || self.checkpoint_revision != request.lease.input.checkpoint_revision
            || !desktop_revision(&self.checkpoint_revision)
            || !(1..=MAX_POST_PROCESS_CLEANUP_BODY_BYTES).contains(&self.total_bytes)
            || self.received_bytes > self.total_bytes
            || !(1..=MAX_POST_PROCESS_CLEANUP_CHUNKS + 1).contains(&self.next_sequence)
            || self.next_sequence - 1 > self.received_bytes
            || (self.received_bytes == 0) != (self.next_sequence == 1)
            || self.expires_at.timestamp_millis() <= 0
            || self.expires_at
                > Utc::now() + Duration::seconds(POST_PROCESS_CLEANUP_TTL_SECONDS + 5)
            || (self.state == PostProcessCleanupUploadState::Committed
                && self.received_bytes != self.total_bytes)
        {
            return false;
        }
        match &request.operation {
            PostProcessCleanupOperation::Begin { total_bytes, .. } => {
                self.total_bytes == *total_bytes
            }
            PostProcessCleanupOperation::Append {
                sequence,
                byte_offset,
                text,
                ..
            } if self.state == PostProcessCleanupUploadState::Open => {
                byte_offset
                    .checked_add(text.len() as u64)
                    .is_some_and(|end| self.received_bytes >= end)
                    && self.next_sequence > *sequence
            }
            PostProcessCleanupOperation::Abort { .. }
            | PostProcessCleanupOperation::Commit { .. } => {
                self.state != PostProcessCleanupUploadState::Open
            }
            _ => true,
        }
    }
}

impl PostProcessCleanupReceipt {
    pub fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        let old = &request.lease;
        let new = &self.lease;
        matches!(
            request.operation,
            PostProcessCleanupOperation::Commit { .. }
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
            && new.dedup_until >= new.expires_at
            && match self.outcome {
                PostProcessCleanupOutcome::Applied => {
                    (1..=MAX_POST_PROCESS_CLEANUP_UPDATES as u32).contains(&self.changed_segments)
                        && new.input.transcript_revision != old.input.transcript_revision
                }
                PostProcessCleanupOutcome::Noop => {
                    self.changed_segments == 0
                        && new.input.transcript_revision == old.input.transcript_revision
                }
            }
    }
}

impl PostProcessCleanupReply {
    pub fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        request.valid()
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
            && match self {
                Self::Upload(status) => status.valid_for(request),
                Self::Committed(receipt) => receipt.valid_for(request),
            }
    }
}

#[cfg(test)]
#[path = "postprocess_cleanup_tests.rs"]
mod tests;

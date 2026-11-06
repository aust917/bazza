//! Fixture-only registered post-process claims and their current input stage.
use crate::desktop::{desktop_body_fits, desktop_id, desktop_revision, desktop_scope};
use crate::worker::{
    CaptureProvider, CaptureSelection, RegisteredWorker, WorkerCapability, WorkerProof,
    WorkerSecret,
};
use bazza_domain::local_asr::LocalAsrOutcome;
use bazza_domain::speaker_result::SpeakerResultOutcome;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_CAPABILITY: &str = "experimental_fixture_post_process_claims";
pub const POST_PROCESS_MAX_DATA_BYTES: u32 = 65_024;
pub const POST_PROCESS_MAX_ITEMS: u16 = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSourceFence {
    pub capture_id: Uuid,
    pub capture_generation: Uuid,
    pub checkpoint_revision: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostProcessStage {
    Captured,
    CapturedLive,
    LocalAsr {
        outcome: LocalAsrOutcome,
    },
    AutomaticCleanup {
        local_asr: Option<LocalAsrOutcome>,
    },
    Speakers {
        local_asr: LocalAsrOutcome,
        outcome: SpeakerResultOutcome,
    },
    SonioxSpeakers {
        outcome: SpeakerResultOutcome,
    },
    SonioxReady {
        speakers: SpeakerResultOutcome,
        cleanup: bool,
        generated_summary_revision: Option<String>,
    },
    Ready {
        local_asr: LocalAsrOutcome,
        speakers: SpeakerResultOutcome,
        cleanup: bool,
        generated_summary_revision: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJob {
    pub job_id: String,
    pub meeting_id: Uuid,
    pub revision: String,
    pub input: PostProcessSourceFence,
    pub provider: CaptureSelection,
    pub stage: PostProcessStage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobCursor {
    pub library_scope: String,
    pub library_epoch: Uuid,
    pub worker_session_id: Uuid,
    pub snapshot_sequence: i64,
    pub eligible_at: DateTime<Utc>,
    pub after_job_id: String,
    pub query_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobsRequest {
    pub proof: WorkerProof,
    pub cursor: Option<PostProcessJobCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}

impl PostProcessJobsRequest {
    pub fn new(proof: WorkerProof) -> Self {
        Self {
            proof,
            cursor: None,
            limit: 20,
            max_bytes: POST_PROCESS_MAX_DATA_BYTES,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobPage {
    pub library_scope: String,
    pub snapshot_sequence: i64,
    pub eligible_at: DateTime<Utc>,
    pub items: Vec<PostProcessJob>,
    pub next: Option<PostProcessJobCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimPostProcessJobRequest {
    pub proof: WorkerProof,
    /// Original descriptor from the bounded list; never refresh on conflict.
    pub job: PostProcessJob,
    pub claim_id: Uuid,
    pub operation_id: Uuid,
    pub fence: WorkerSecret,
    pub ttl_seconds: i64,
}

/// An immutable operation receipt, not a promise that its lease is still live.
/// A replay never refreshes this revision or expiry. Every write reauthenticates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobLease {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub input: PostProcessSourceFence,
    pub expires_at: DateTime<Utc>,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewPostProcessJobRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub operation_id: Uuid,
    pub fence: WorkerSecret,
    pub ttl_seconds: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PostProcessFailureCode {
    PostProcessFailed,
    PostProcessInputUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailPostProcessJobRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub operation_id: Uuid,
    pub fence: WorkerSecret,
    pub error_code: PostProcessFailureCode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessFailureReceipt {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub lifecycle_revision: String,
    pub error_code: PostProcessFailureCode,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

fn proof_valid(proof: &WorkerProof) -> bool {
    !proof.session_id.is_nil() && !proof.library_epoch.is_nil()
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn provider_valid(value: &CaptureSelection) -> bool {
    value.model.as_ref().is_none_or(|model| {
        !model.is_empty()
            && model.len() <= 128
            && model.as_bytes()[0].is_ascii_alphanumeric()
            && model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
    }) && value.language.as_ref().is_none_or(|language| {
        (2..=64).contains(&language.len())
            && language.split('-').all(|part| {
                !part.is_empty()
                    && part.len() <= 8
                    && part.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            && language.split('-').next().is_some_and(|part| {
                part.len() >= 2 && part.bytes().all(|b| b.is_ascii_alphabetic())
            })
    })
}
pub fn post_process_worker_valid(
    worker: &RegisteredWorker,
    proof: &WorkerProof,
    epoch: Uuid,
) -> bool {
    proof_valid(proof)
        && worker.session_id == proof.session_id
        && worker.library_epoch == epoch
        && proof.library_epoch == epoch
        && worker.peer_pid != 0
        && worker.capabilities.contains(&WorkerCapability::PostProcess)
        && worker.expires_at > Utc::now()
}
impl PostProcessSourceFence {
    pub fn valid(&self) -> bool {
        !self.capture_id.is_nil()
            && !self.capture_generation.is_nil()
            && desktop_revision(&self.checkpoint_revision)
            && desktop_revision(&self.lifecycle_revision)
            && desktop_revision(&self.transcript_revision)
            && self.source_id.as_ref().is_none_or(|id| desktop_id(id))
    }
}
impl PostProcessJob {
    pub fn valid(&self) -> bool {
        desktop_id(&self.job_id)
            && !self.meeting_id.is_nil()
            && desktop_revision(&self.revision)
            && self.input.valid()
            && provider_valid(&self.provider)
            && match self.stage {
                PostProcessStage::CapturedLive
                | PostProcessStage::LocalAsr { .. }
                | PostProcessStage::Speakers { .. }
                | PostProcessStage::AutomaticCleanup { local_asr: Some(_) } => {
                    self.provider.provider == CaptureProvider::Local
                        && self.input.source_id.is_some()
                }
                PostProcessStage::Ready {
                    local_asr,
                    cleanup,
                    ref generated_summary_revision,
                    ..
                } => {
                    // A registered complete combined source is local ASR even
                    // when the immutable capture selection was Soniox. The
                    // Store still verifies its source/job publication binding.
                    (self.provider.provider == CaptureProvider::Local
                        || local_asr == LocalAsrOutcome::Complete)
                        && self.input.source_id.is_some()
                        && generated_summary_revision
                            .as_ref()
                            .is_none_or(|revision| cleanup && desktop_revision(revision))
                }
                PostProcessStage::SonioxSpeakers { .. } => {
                    self.provider.provider == CaptureProvider::Soniox
                        && self.input.source_id.is_some()
                }
                PostProcessStage::SonioxReady {
                    cleanup,
                    ref generated_summary_revision,
                    ..
                } => {
                    self.provider.provider == CaptureProvider::Soniox
                        && self.input.source_id.is_some()
                        && generated_summary_revision
                            .as_ref()
                            .is_none_or(|revision| cleanup && desktop_revision(revision))
                }
                _ => true,
            }
    }
}
impl PostProcessJobCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && !self.library_epoch.is_nil()
            && !self.worker_session_id.is_nil()
            && self.snapshot_sequence >= 0
            && self.snapshot_sequence <= 9_007_199_254_740_991
            && self.eligible_at.timestamp_millis() >= 0
            && desktop_id(&self.after_job_id)
            && hash(&self.query_hash)
    }
}
impl PostProcessJobsRequest {
    pub fn valid(&self) -> bool {
        proof_valid(&self.proof)
            && (1..=POST_PROCESS_MAX_ITEMS).contains(&self.limit)
            && (1024..=POST_PROCESS_MAX_DATA_BYTES).contains(&self.max_bytes)
            && self.cursor.as_ref().is_none_or(|cursor| {
                cursor.valid()
                    && cursor.library_epoch == self.proof.library_epoch
                    && cursor.worker_session_id == self.proof.session_id
            })
    }
}
impl PostProcessJobPage {
    pub fn valid_for(&self, request: &PostProcessJobsRequest) -> bool {
        if !desktop_scope(&self.library_scope)
            || self.snapshot_sequence < 0
            || self.snapshot_sequence > 9_007_199_254_740_991
            || self.eligible_at.timestamp_millis() < 0
            || self.eligible_at > Utc::now() + Duration::seconds(5)
            || self.items.len() > request.limit as usize
            || !desktop_body_fits(self, request.max_bytes as usize)
            || self.items.iter().any(|job| !job.valid())
            || self
                .items
                .windows(2)
                .any(|items| items[0].job_id >= items[1].job_id)
        {
            return false;
        }
        if let Some(cursor) = &request.cursor {
            if self.library_scope != cursor.library_scope
                || self.snapshot_sequence != cursor.snapshot_sequence
                || self.eligible_at != cursor.eligible_at
                || self
                    .items
                    .first()
                    .is_some_and(|job| job.job_id <= cursor.after_job_id)
            {
                return false;
            }
        }
        self.next.as_ref().is_none_or(|next| {
            next.valid()
                && next.library_scope == self.library_scope
                && next.snapshot_sequence == self.snapshot_sequence
                && next.eligible_at == self.eligible_at
                && next.library_epoch == request.proof.library_epoch
                && next.worker_session_id == request.proof.session_id
                && self
                    .items
                    .last()
                    .is_some_and(|last| last.job_id == next.after_job_id)
                && request
                    .cursor
                    .as_ref()
                    .is_none_or(|old| old.query_hash == next.query_hash)
        })
    }
}
impl PostProcessJobLease {
    pub fn valid(&self) -> bool {
        !self.operation_id.is_nil()
            && !self.claim_id.is_nil()
            && !self.worker_session_id.is_nil()
            && !self.library_epoch.is_nil()
            && desktop_id(&self.job_id)
            && !self.meeting_id.is_nil()
            && desktop_revision(&self.job_revision)
            && self.input.valid()
            && self.change_sequence > 0
            && self.change_sequence <= 9_007_199_254_740_991
            && self.expires_at.timestamp_millis() > 0
            && self.dedup_until >= self.expires_at
            && self.dedup_until
                <= Utc::now() + Duration::seconds(crate::DEDUP_RETENTION_SECONDS as i64 + 5)
    }
    fn owned_by(&self, proof: &WorkerProof) -> bool {
        self.worker_session_id == proof.session_id && self.library_epoch == proof.library_epoch
    }
    pub fn claimed_for(
        &self,
        request: &ClaimPostProcessJobRequest,
        worker: &RegisteredWorker,
    ) -> bool {
        self.valid()
            && self.operation_id == request.operation_id
            && self.claim_id == request.claim_id
            && self.owned_by(&request.proof)
            && self.job_id == request.job.job_id
            && self.meeting_id == request.job.meeting_id
            && self.input == request.job.input
            && self.job_revision != request.job.revision
            && self.expires_at <= worker.expires_at
            && self.expires_at <= Utc::now() + Duration::seconds(request.ttl_seconds + 5)
    }
    pub fn renewed_for(
        &self,
        request: &RenewPostProcessJobRequest,
        worker: &RegisteredWorker,
    ) -> bool {
        self.valid()
            && self.operation_id == request.operation_id
            && self.claim_id == request.lease.claim_id
            && self.owned_by(&request.proof)
            && self.job_id == request.lease.job_id
            && self.meeting_id == request.lease.meeting_id
            && self.input == request.lease.input
            && self.job_revision != request.lease.job_revision
            && self.expires_at <= worker.expires_at
            && self.expires_at <= Utc::now() + Duration::seconds(request.ttl_seconds + 5)
            && self.change_sequence > request.lease.change_sequence
    }
}
impl ClaimPostProcessJobRequest {
    pub fn valid(&self) -> bool {
        proof_valid(&self.proof)
            && self.job.valid()
            && !self.claim_id.is_nil()
            && !self.operation_id.is_nil()
            && (5..=300).contains(&self.ttl_seconds)
    }
}
impl RenewPostProcessJobRequest {
    pub fn valid(&self) -> bool {
        proof_valid(&self.proof)
            && self.lease.valid()
            && self.lease.owned_by(&self.proof)
            && !self.operation_id.is_nil()
            && (5..=300).contains(&self.ttl_seconds)
    }
}
impl FailPostProcessJobRequest {
    pub fn valid(&self) -> bool {
        proof_valid(&self.proof)
            && self.lease.valid()
            && self.lease.owned_by(&self.proof)
            && !self.operation_id.is_nil()
    }
}
impl PostProcessFailureReceipt {
    pub fn valid_for(&self, request: &FailPostProcessJobRequest) -> bool {
        self.operation_id == request.operation_id
            && self.claim_id == request.lease.claim_id
            && self.worker_session_id == request.proof.session_id
            && self.library_epoch == request.proof.library_epoch
            && self.job_id == request.lease.job_id
            && self.meeting_id == request.lease.meeting_id
            && desktop_revision(&self.job_revision)
            && self.job_revision != request.lease.job_revision
            && desktop_revision(&self.lifecycle_revision)
            && self.lifecycle_revision != request.lease.input.lifecycle_revision
            && self.error_code == request.error_code
            && self.change_sequence > request.lease.change_sequence
            && self.change_sequence <= 9_007_199_254_740_991
            && self.dedup_until.timestamp_millis() > 0
            && self.dedup_until
                <= Utc::now() + Duration::seconds(crate::DEDUP_RETENTION_SECONDS as i64 + 5)
    }
}

#[cfg(test)]
#[path = "postprocess_tests.rs"]
mod tests;

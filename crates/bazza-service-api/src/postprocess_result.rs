//! One bounded automatic title result from an authenticated post-process owner.
//! This does not finish the job or publish document, media or provider output.
use crate::{
    desktop::{desktop_body_fits, desktop_id, desktop_revision, MAX_DESKTOP_DATA_BYTES},
    postprocess::PostProcessJobLease,
    worker::{WorkerProof, WorkerSecret},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_RESULT_CAPABILITY: &str =
    "experimental_fixture_post_process_auto_title";
pub const MAX_POST_PROCESS_AUTO_TITLE_BYTES: usize = 512;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitPostProcessAutoTitleRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
    pub expected_metadata_revision: String,
    pub title: String,
}

impl std::fmt::Debug for CommitPostProcessAutoTitleRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitPostProcessAutoTitleRequest")
            .field("proof", &self.proof)
            .field("lease", &self.lease)
            .field("fence", &self.fence)
            .field("operation_id", &self.operation_id)
            .field(
                "expected_metadata_revision",
                &self.expected_metadata_revision,
            )
            .field("title_bytes", &self.title.len())
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessAutoTitleReceipt {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub metadata_revision: String,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

impl CommitPostProcessAutoTitleRequest {
    pub fn valid(&self) -> bool {
        self.lease.valid()
            && !self.proof.session_id.is_nil()
            && !self.proof.library_epoch.is_nil()
            && self.proof.session_id == self.lease.worker_session_id
            && self.proof.library_epoch == self.lease.library_epoch
            && !self.operation_id.is_nil()
            && desktop_revision(&self.expected_metadata_revision)
            && !self.title.trim().is_empty()
            && self.title.len() <= MAX_POST_PROCESS_AUTO_TITLE_BYTES
            && !self.title.contains('\0')
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
    }
}

impl PostProcessAutoTitleReceipt {
    pub fn valid_for(&self, request: &CommitPostProcessAutoTitleRequest) -> bool {
        self.operation_id == request.operation_id
            && self.claim_id == request.lease.claim_id
            && self.worker_session_id == request.proof.session_id
            && self.library_epoch == request.proof.library_epoch
            && self.job_id == request.lease.job_id
            && desktop_id(&self.job_id)
            && self.meeting_id == request.lease.meeting_id
            && desktop_revision(&self.metadata_revision)
            && self.metadata_revision != request.expected_metadata_revision
            && self.change_sequence > request.lease.change_sequence
            && self.change_sequence <= 9_007_199_254_740_991
            && self.dedup_until.timestamp_millis() > 0
            && self.dedup_until
                <= Utc::now() + Duration::seconds(crate::DEDUP_RETENTION_SECONDS as i64 + 5)
            && desktop_body_fits(self, 4096)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postprocess::PostProcessSourceFence;

    fn request() -> CommitPostProcessAutoTitleRequest {
        let session = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        CommitPostProcessAutoTitleRequest {
            proof: WorkerProof {
                session_id: session,
                secret: WorkerSecret::random(),
                library_epoch: epoch,
            },
            lease: PostProcessJobLease {
                operation_id: Uuid::new_v4(),
                claim_id: Uuid::new_v4(),
                worker_session_id: session,
                library_epoch: epoch,
                job_id: "post-process-job".into(),
                meeting_id: Uuid::new_v4(),
                job_revision: "claimed-revision".into(),
                input: PostProcessSourceFence {
                    capture_id: Uuid::new_v4(),
                    capture_generation: Uuid::new_v4(),
                    checkpoint_revision: "checkpoint-original".into(),
                    lifecycle_revision: "capture-finalized".into(),
                    transcript_revision: "captured-transcript".into(),
                    source_id: Some("captured-source".into()),
                },
                expires_at: Utc::now() + Duration::seconds(60),
                change_sequence: 5,
                dedup_until: Utc::now() + Duration::days(7),
            },
            fence: WorkerSecret::random(),
            operation_id: Uuid::new_v4(),
            expected_metadata_revision: "metadata-original".into(),
            title: "  Private title Орион  ".into(),
        }
    }

    #[test]
    fn request_preserves_bounded_title_and_allows_inert_historical_replay() {
        let mut request = request();
        assert!(request.valid());
        request.title = "я".repeat(256);
        assert!(request.valid());
        request.title.push('x');
        assert!(!request.valid());
        for title in ["", " \t\n", "bad\0title"] {
            request.title = title.into();
            assert!(!request.valid());
        }
        request.title = " exact ".into();
        request.lease.expires_at = Utc::now() - Duration::seconds(1);
        assert!(request.valid(), "history does not renew authority");
        request.proof.session_id = Uuid::new_v4();
        assert!(!request.valid());
    }

    #[test]
    fn request_denies_authority_selectors_and_redacts_title_and_secrets() {
        let request = request();
        let debug = format!("{request:?}");
        assert!(!debug.contains(&request.title));
        assert!(!debug.contains(request.proof.secret.expose()));
        assert!(!debug.contains(request.fence.expose()));
        for field in [
            "actor",
            "peer_pid",
            "path",
            "claim_token",
            "title_auto",
            "status",
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = serde_json::json!("forbidden selector");
            assert!(serde_json::from_value::<CommitPostProcessAutoTitleRequest>(value).is_err());
        }
    }

    #[test]
    fn receipt_binds_operation_claim_target_and_metadata_without_title() {
        let request = request();
        let receipt = PostProcessAutoTitleReceipt {
            operation_id: request.operation_id,
            claim_id: request.lease.claim_id,
            worker_session_id: request.proof.session_id,
            library_epoch: request.proof.library_epoch,
            job_id: request.lease.job_id.clone(),
            meeting_id: request.lease.meeting_id,
            metadata_revision: "metadata-generated".into(),
            change_sequence: 6,
            dedup_until: Utc::now() + Duration::days(7),
        };
        assert!(receipt.valid_for(&request));
        for fault in 0..8 {
            let mut bad = receipt.clone();
            match fault {
                0 => bad.operation_id = Uuid::new_v4(),
                1 => bad.claim_id = Uuid::new_v4(),
                2 => bad.worker_session_id = Uuid::new_v4(),
                3 => bad.library_epoch = Uuid::new_v4(),
                4 => bad.job_id = "another-job".into(),
                5 => bad.meeting_id = Uuid::new_v4(),
                6 => bad.metadata_revision = request.expected_metadata_revision.clone(),
                _ => bad.change_sequence = request.lease.change_sequence,
            }
            assert!(!bad.valid_for(&request), "fault {fault}");
        }
        let mut value = serde_json::to_value(receipt).unwrap();
        value["title"] = serde_json::json!(request.title);
        assert!(serde_json::from_value::<PostProcessAutoTitleReceipt>(value).is_err());
    }
}

//! Publish a speaker outcome for the current fenced transcript source.
use super::*;
use crate::service_data::postprocess_speaker_install;
use bazza_domain::speaker_result::{SpeakerResultBody, SpeakerResultOutcome};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSpeakersReceipt {
    pub operation_id: Uuid,
    pub upload_id: Uuid,
    pub lease: PostProcessJobLease,
    pub outcome: SpeakerResultOutcome,
    pub changed_segments: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostProcessSpeakersReply {
    Upload(PostProcessCleanupStatus),
    Committed(PostProcessSpeakersReceipt),
}

impl StagedReply for PostProcessSpeakersReply {
    const KIND: UploadKind = UploadKind::Speakers;
    const TOOL: &'static str = "post_process_speakers";

    fn upload(status: PostProcessCleanupStatus) -> Self {
        Self::Upload(status)
    }

    fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        request.valid()
            && request.lease.input.source_id.is_some()
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
            && match self {
                Self::Upload(status) => {
                    status.valid_for(request)
                        && !(matches!(
                            request.operation,
                            PostProcessCleanupOperation::Commit { .. }
                        ) && status.state == PostProcessCleanupUploadState::Committed)
                }
                Self::Committed(receipt) => {
                    let old = &request.lease;
                    let new = &receipt.lease;
                    matches!(
                        request.operation,
                        PostProcessCleanupOperation::Commit { .. }
                    ) && receipt.operation_id == request.operation_id
                        && receipt.upload_id == request.operation.upload_id()
                        && cleanup_lease_valid(new)
                        && postprocess::same_claim_rotation(old, new, request.operation_id)
                        && new.input.source_id == old.input.source_id
                        && new.input.lifecycle_revision == old.input.lifecycle_revision
                        && (new.input.transcript_revision != old.input.transcript_revision)
                            == (receipt.changed_segments > 0)
                        && receipt
                            .outcome
                            .valid_changed_segments(receipt.changed_segments)
                }
            }
    }

    fn commit(
        tx: &Transaction<'_>,
        request: &PostProcessCleanupRequest,
        upload: &Upload,
        token: &str,
    ) -> DataResult<Self> {
        let job = postprocess::candidate_on(tx, &request.lease.job_id)?;
        let body = SpeakerResultBody::parse(&body_on(tx, upload)?)
            .map_err(|_| DataError::Invalid("speaker result body"))?;
        let installed = postprocess_speaker_install::install_on(
            tx,
            &job,
            &body,
            &format!("worker:{}", request.proof.session_id),
        )?;
        let outcome = body.outcome();
        tx.execute(
            "INSERT INTO service_post_process_speaker_results(job_id,source_id,outcome_json) VALUES(?1,?2,?3)",
            params![
                job.job_id,
                job.input.source_id.as_deref().ok_or(DataError::Forbidden)?,
                serde_json::to_string(&outcome)?
            ],
        )?;
        let lease = postprocess::advance_result_on(
            tx,
            &request.proof,
            &request.lease,
            token,
            postprocess::InstalledResult::Speakers {
                transcript_revision: &installed.revision,
            },
            request.operation_id,
        )?;
        let receipt = PostProcessSpeakersReceipt {
            operation_id: request.operation_id,
            upload_id: upload.status.upload_id,
            lease,
            outcome,
            changed_segments: installed.changed_segments,
        };
        tx.execute(
            "UPDATE service_post_process_cleanup_uploads SET receipt_json=?2 WHERE id=?1",
            params![
                upload.status.upload_id.to_string(),
                serde_json::to_string(&receipt)?
            ],
        )?;
        Ok(Self::Committed(receipt))
    }
}

impl Store {
    pub fn service_post_process_speakers(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessCleanupRequest,
    ) -> DataResult<PostProcessSpeakersReply> {
        self.service_post_process_upload(peer, epoch, request)
    }
}

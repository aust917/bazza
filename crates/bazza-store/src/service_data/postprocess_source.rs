//! Private local ASR result on the shared staged worker transport.
use super::*;
use crate::service_data::postprocess_source_install;
use bazza_domain::local_asr::{LocalAsrBody, LocalAsrOutcome, MAX_LOCAL_ASR_CAPTIONS};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSourceReceipt {
    pub operation_id: Uuid,
    pub upload_id: Uuid,
    pub lease: PostProcessJobLease,
    pub outcome: LocalAsrOutcome,
    pub segment_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PostProcessSourceReply {
    Upload(PostProcessCleanupStatus),
    Committed(PostProcessSourceReceipt),
}

impl StagedReply for PostProcessSourceReply {
    const KIND: UploadKind = UploadKind::LocalSource;
    const TOOL: &'static str = "post_process_local_source";

    fn upload(status: PostProcessCleanupStatus) -> Self {
        Self::Upload(status)
    }

    fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        request.valid()
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
                        && receipt.segment_count as usize <= MAX_LOCAL_ASR_CAPTIONS
                        && (receipt.outcome == LocalAsrOutcome::Complete
                            || receipt.segment_count == 0)
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
        let body = LocalAsrBody::parse(&body_on(tx, upload)?)
            .map_err(|_| DataError::Invalid("local ASR body"))?;
        let result = postprocess_source_install::install_on(
            tx,
            &job,
            &body,
            &format!("worker:{}", request.proof.session_id),
        )?;
        let lease = postprocess::advance_result_on(
            tx,
            &request.proof,
            &request.lease,
            token,
            postprocess::InstalledResult::LocalSource {
                transcript_revision: &result.revision,
                source: &result.source_id,
            },
            request.operation_id,
        )?;
        let receipt = PostProcessSourceReceipt {
            operation_id: request.operation_id,
            upload_id: upload.status.upload_id,
            lease,
            outcome: body.outcome,
            segment_count: result.caption_count,
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
    pub fn service_post_process_source(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessCleanupRequest,
    ) -> DataResult<PostProcessSourceReply> {
        self.service_post_process_upload(peer, epoch, request)
    }
}

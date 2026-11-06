//! Commit configured output and its provenance under one live claim.
use super::*;
use crate::service_data::postprocess_summary_install;
use bazza_domain::worker_summary::{WorkerGeneratedSummary, MAX_WORKER_SUMMARY_BODY_BYTES};

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
    Upload(PostProcessCleanupStatus),
    Committed(PostProcessSummaryReceipt),
}

impl StagedReply for PostProcessSummaryReply {
    const KIND: UploadKind = UploadKind::GeneratedSummary;
    const TOOL: &'static str = "post_process_summary";

    fn upload(status: PostProcessCleanupStatus) -> Self {
        Self::Upload(status)
    }

    fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        request.valid()
            && request.lease.input.source_id.is_some()
            && desktop_body_fits(self, 4096)
            && match self {
                Self::Upload(status) => {
                    status.total_bytes <= MAX_WORKER_SUMMARY_BODY_BYTES
                        && status.valid_for(request)
                        && !(matches!(
                            request.operation,
                            PostProcessCleanupOperation::Commit { .. }
                        ) && status.state == PostProcessCleanupUploadState::Committed)
                }
                Self::Committed(receipt) => {
                    let lease = &request.lease;
                    matches!(
                        request.operation,
                        PostProcessCleanupOperation::Commit { .. }
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

    fn commit(
        tx: &Transaction<'_>,
        request: &PostProcessCleanupRequest,
        upload: &Upload,
        token: &str,
    ) -> DataResult<Self> {
        let lease = &request.lease;
        let job = postprocess::candidate_on(tx, &lease.job_id)?;
        let source = job.input.source_id.as_deref().ok_or(DataError::Forbidden)?;
        let body = WorkerGeneratedSummary::parse(&body_on(tx, upload)?)
            .map_err(|_| DataError::Invalid("generated summary body"))?;
        let template_id = body.template_id.clone();
        let template_revision = body.expected_template_revision.clone();
        let template_generation = body.expected_template_generation.clone();
        let installed = postprocess_summary_install::install_on(
            tx,
            &job,
            body,
            &format!("worker:{}", request.proof.session_id),
        )?;
        tx.execute(
            "INSERT INTO service_post_process_generated_summaries(
                job_id,source_id,transcript_revision,summary_revision,template_id,template_revision,template_generation)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![job.job_id,source,job.input.transcript_revision,installed.revision,
                template_id,template_revision,template_generation],
        )?;
        // The document changes independently of the lease. Verify the same
        // live binding again before accepting potentially large output.
        if binding_on(tx, request)? != token {
            return Err(DataError::Forbidden);
        }
        let receipt = PostProcessSummaryReceipt {
            operation_id: request.operation_id,
            upload_id: upload.status.upload_id,
            claim_id: lease.claim_id,
            worker_session_id: lease.worker_session_id,
            library_epoch: lease.library_epoch,
            job_id: lease.job_id.clone(),
            meeting_id: lease.meeting_id,
            job_revision: lease.job_revision.clone(),
            input: lease.input.clone(),
            summary_revision: installed.revision,
            change_sequence: installed.change_sequence,
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
    pub fn service_post_process_summary(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessCleanupRequest,
    ) -> DataResult<PostProcessSummaryReply> {
        self.service_post_process_upload(peer, epoch, request)
    }
}

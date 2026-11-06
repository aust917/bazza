use super::*;
use crate::{
    postprocess::PostProcessSourceFence,
    worker::{WorkerProof, WorkerSecret},
};
use chrono::{Duration, Utc};

fn commit() -> (PostProcessSourceRequest, PostProcessSourceReceipt) {
    let now = Utc::now();
    let proof = WorkerProof {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        library_epoch: Uuid::new_v4(),
    };
    let upload_id = Uuid::new_v4();
    let request = PostProcessSourceRequest {
        lease: PostProcessJobLease {
            operation_id: Uuid::new_v4(),
            claim_id: Uuid::new_v4(),
            worker_session_id: proof.session_id,
            library_epoch: proof.library_epoch,
            job_id: "job-local-asr".into(),
            meeting_id: Uuid::new_v4(),
            job_revision: "claimed-job".into(),
            input: PostProcessSourceFence {
                capture_id: Uuid::new_v4(),
                capture_generation: Uuid::new_v4(),
                checkpoint_revision: "captured-checkpoint".into(),
                lifecycle_revision: "processing-life".into(),
                transcript_revision: "captured-transcript".into(),
                source_id: Some("captured-source".into()),
            },
            // Historical receipts retain this exact expired authority.
            expires_at: now - Duration::seconds(1),
            change_sequence: 5,
            dedup_until: now + Duration::days(7),
        },
        proof,
        fence: WorkerSecret::random(),
        operation_id: Uuid::new_v4(),
        operation: PostProcessSourceOperation::Commit { upload_id },
    };
    let mut lease = request.lease.clone();
    lease.operation_id = request.operation_id;
    lease.job_revision = "local-source-job".into();
    lease.input.checkpoint_revision = "source-selected".into();
    lease.input.transcript_revision = "local-transcript".into();
    lease.input.source_id = Some("immutable-local-source".into());
    lease.change_sequence += 1;
    let reply = PostProcessSourceReceipt {
        operation_id: request.operation_id,
        upload_id,
        lease,
        outcome: LocalAsrOutcome::Complete,
        segment_count: 1,
    };
    (request, reply)
}

#[test]
fn successful_receipt_changes_only_the_live_source_fences() {
    let (request, receipt) = commit();
    let valid = |value| PostProcessSourceReply::Committed(value).valid_for(&request);
    assert!(valid(receipt.clone()));
    let mut forged = receipt.clone();
    forged.lease.input.source_id = request.lease.input.source_id.clone();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.source_id = None;
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.transcript_revision = request.lease.input.transcript_revision.clone();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.checkpoint_revision = request.lease.input.checkpoint_revision.clone();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.lifecycle_revision = "forged-ready".into();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.capture_id = Uuid::new_v4();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.expires_at += Duration::seconds(1);
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.segment_count = MAX_LOCAL_ASR_CAPTIONS as u32 + 1;
    assert!(!valid(forged));
    let mut empty = receipt;
    empty.segment_count = 0;
    assert!(valid(empty.clone()));
    empty.outcome = LocalAsrOutcome::NoModel;
    assert!(valid(empty.clone()));
    empty.segment_count = 1;
    assert!(!valid(empty));
}

#[test]
fn successful_receipt_is_closed_and_cannot_substitute_for_a_progress_reply() {
    let (mut request, receipt) = commit();
    let reply = PostProcessSourceReply::Committed(receipt);
    assert!(reply.valid_for(&request));
    request.operation = PostProcessSourceOperation::Status {
        upload_id: request.operation.upload_id(),
    };
    assert!(!reply.valid_for(&request));
    let wire = serde_json::to_value(&reply).unwrap();
    for field in ["model", "language", "body", "captions", "sha256", "fence"] {
        let mut forged = wire.clone();
        forged["data"][field] = serde_json::json!("private");
        assert!(serde_json::from_value::<PostProcessSourceReply>(forged).is_err());
    }
}

#[test]
fn terminal_progress_never_impersonates_a_successful_commit_receipt() {
    let (mut request, _) = commit();
    let mut status = PostProcessSourceStatus {
        upload_id: request.operation.upload_id(),
        claim_id: request.lease.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        checkpoint_revision: request.lease.input.checkpoint_revision.clone(),
        state: PostProcessSourceUploadState::Committed,
        total_bytes: 7,
        received_bytes: 7,
        next_sequence: 2,
        expires_at: Utc::now() + Duration::minutes(30),
    };
    assert!(!PostProcessSourceReply::Upload(status.clone()).valid_for(&request));
    status.state = PostProcessSourceUploadState::Expired;
    assert!(PostProcessSourceReply::Upload(status.clone()).valid_for(&request));

    status.state = PostProcessSourceUploadState::Committed;
    request.operation = PostProcessSourceOperation::Status {
        upload_id: request.operation.upload_id(),
    };
    assert!(PostProcessSourceReply::Upload(status).valid_for(&request));
}

use super::*;
use crate::{
    postprocess::PostProcessSourceFence,
    worker::{WorkerProof, WorkerSecret},
};
use chrono::{Duration, Utc};

fn commit() -> (PostProcessSpeakersRequest, PostProcessSpeakersReceipt) {
    let now = Utc::now();
    let proof = WorkerProof {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        library_epoch: Uuid::new_v4(),
    };
    let upload_id = Uuid::new_v4();
    let request = PostProcessSpeakersRequest {
        lease: PostProcessJobLease {
            operation_id: Uuid::new_v4(),
            claim_id: Uuid::new_v4(),
            worker_session_id: proof.session_id,
            library_epoch: proof.library_epoch,
            job_id: "job-speakers".into(),
            meeting_id: Uuid::new_v4(),
            job_revision: "claimed-job".into(),
            input: PostProcessSourceFence {
                capture_id: Uuid::new_v4(),
                capture_generation: Uuid::new_v4(),
                checkpoint_revision: "local-source-checkpoint".into(),
                lifecycle_revision: "processing-life".into(),
                transcript_revision: "local-transcript".into(),
                source_id: Some("immutable-local-source".into()),
            },
            expires_at: now - Duration::seconds(1),
            change_sequence: 5,
            dedup_until: now + Duration::days(7),
        },
        proof,
        fence: WorkerSecret::random(),
        operation_id: Uuid::new_v4(),
        operation: PostProcessSpeakersOperation::Commit { upload_id },
    };
    let mut lease = request.lease.clone();
    lease.operation_id = request.operation_id;
    lease.job_revision = "speakers-job".into();
    lease.input.checkpoint_revision = "speakers-checkpoint".into();
    lease.input.transcript_revision = "speaker-labels-transcript".into();
    lease.change_sequence += 1;
    let receipt = PostProcessSpeakersReceipt {
        operation_id: request.operation_id,
        upload_id,
        lease,
        outcome: SpeakerResultOutcome::Applied,
        changed_segments: 1,
    };
    (request, receipt)
}

#[test]
fn receipt_rotates_job_and_checkpoint_and_pins_the_local_source() {
    let (request, receipt) = commit();
    let valid = |receipt| PostProcessSpeakersReply::Committed(receipt).valid_for(&request);
    assert!(valid(receipt.clone()));

    let mut forged = receipt.clone();
    forged.lease.input.source_id = Some("replacement".into());
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.lifecycle_revision = "forged-ready".into();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.capture_generation = Uuid::new_v4();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.expires_at += Duration::seconds(1);
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.job_revision = request.lease.job_revision.clone();
    assert!(!valid(forged));
    let mut forged = receipt.clone();
    forged.lease.input.checkpoint_revision = request.lease.input.checkpoint_revision.clone();
    assert!(!valid(forged));

    let mut missing_source = request;
    missing_source.lease.input.source_id = None;
    assert!(!post_process_speakers_request_valid(&missing_source));
}

#[test]
fn receipt_revision_reports_label_changes_and_preserves_the_skip_reason() {
    let (request, mut receipt) = commit();
    let valid = |receipt| PostProcessSpeakersReply::Committed(receipt).valid_for(&request);
    receipt.changed_segments = MAX_SPEAKER_ASSIGNMENTS as u32 + 1;
    assert!(!valid(receipt.clone()));
    receipt.changed_segments = 0;
    assert!(!valid(receipt.clone()));
    receipt.lease.input.transcript_revision = request.lease.input.transcript_revision.clone();
    assert!(valid(receipt.clone()));
    receipt.outcome = SpeakerResultOutcome::Skipped {
        reason: SpeakerSkipReason::EmptyTranscript,
    };
    assert!(valid(receipt.clone()));
    receipt.changed_segments = 1;
    receipt.lease.input.transcript_revision = "changed".into();
    assert!(!valid(receipt));
}

#[test]
fn receipt_has_no_body_and_commit_cannot_succeed_as_terminal_progress() {
    let (mut request, receipt) = commit();
    let reply = PostProcessSpeakersReply::Committed(receipt);
    let encoded = serde_json::to_value(&reply).unwrap();
    for field in [
        "assignments",
        "profile_id",
        "model",
        "body",
        "labels",
        "sha256",
    ] {
        let mut forged = encoded.clone();
        forged["data"][field] = serde_json::json!("private");
        assert!(serde_json::from_value::<PostProcessSpeakersReply>(forged).is_err());
    }

    let mut status = PostProcessSpeakersStatus {
        upload_id: request.operation.upload_id(),
        claim_id: request.lease.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        checkpoint_revision: request.lease.input.checkpoint_revision.clone(),
        state: PostProcessSpeakersUploadState::Committed,
        total_bytes: 7,
        received_bytes: 7,
        next_sequence: 2,
        expires_at: Utc::now() + Duration::minutes(30),
    };
    assert!(!PostProcessSpeakersReply::Upload(status.clone()).valid_for(&request));
    status.state = PostProcessSpeakersUploadState::Expired;
    assert!(PostProcessSpeakersReply::Upload(status.clone()).valid_for(&request));
    status.state = PostProcessSpeakersUploadState::Committed;
    request.operation = PostProcessSpeakersOperation::Status {
        upload_id: request.operation.upload_id(),
    };
    assert!(!reply.valid_for(&request));
    assert!(PostProcessSpeakersReply::Upload(status).valid_for(&request));
}

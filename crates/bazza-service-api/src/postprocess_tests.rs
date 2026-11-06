use super::*;
use crate::worker::{CaptureProvider, WorkerSecret};

fn fixture() -> (
    RegisteredWorker,
    ClaimPostProcessJobRequest,
    PostProcessJobLease,
) {
    let epoch = Uuid::new_v4();
    let session = Uuid::new_v4();
    let worker = RegisteredWorker {
        session_id: session,
        library_epoch: epoch,
        peer_pid: 42,
        capabilities: vec![WorkerCapability::PostProcess],
        expires_at: Utc::now() + Duration::seconds(200),
    };
    let job = PostProcessJob {
        job_id: "job-1".into(),
        meeting_id: Uuid::new_v4(),
        revision: "job-original".into(),
        stage: PostProcessStage::Captured,
        input: PostProcessSourceFence {
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            checkpoint_revision: "checkpoint-original".into(),
            lifecycle_revision: "finalized-life".into(),
            transcript_revision: "raw-revision".into(),
            source_id: Some("captured-source".into()),
        },
        provider: CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("chosen-model".into()),
            language: Some("ru-RU".into()),
        },
    };
    let request = ClaimPostProcessJobRequest {
        proof: WorkerProof {
            session_id: session,
            secret: WorkerSecret::random(),
            library_epoch: epoch,
        },
        job: job.clone(),
        claim_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        ttl_seconds: 30,
    };
    let lease = PostProcessJobLease {
        operation_id: request.operation_id,
        claim_id: request.claim_id,
        worker_session_id: session,
        library_epoch: epoch,
        job_id: job.job_id,
        meeting_id: job.meeting_id,
        job_revision: "job-claimed".into(),
        input: job.input,
        expires_at: Utc::now() + Duration::seconds(30),
        change_sequence: 4,
        dedup_until: Utc::now() + Duration::days(7),
    };
    (worker, request, lease)
}

#[test]
fn checkpoint_is_required_validated_and_cannot_be_filled_into_legacy_history() {
    let (worker, request, lease) = fixture();
    let mut input = serde_json::to_value(&request.job.input).unwrap();
    input.as_object_mut().unwrap().remove("checkpoint_revision");
    assert!(serde_json::from_value::<PostProcessSourceFence>(input).is_err());
    let mut history = serde_json::to_value(&lease).unwrap();
    history["input"]
        .as_object_mut()
        .unwrap()
        .remove("checkpoint_revision");
    assert!(serde_json::from_value::<PostProcessJobLease>(history).is_err());
    for value in [
        String::new(),
        "bad/checkpoint".into(),
        "x".repeat(129),
        "bad\0checkpoint".into(),
    ] {
        let mut invalid = request.job.clone();
        invalid.input.checkpoint_revision = value;
        assert!(!invalid.valid());
    }
    let mut changed = lease;
    changed.input.checkpoint_revision = "checkpoint-next".into();
    assert!(changed.valid());
    assert!(!changed.claimed_for(&request, &worker));
}

#[test]
fn closed_worker_job_messages_never_accept_authority_paths_or_dynamic_failures() {
    let (_, request, lease) = fixture();
    assert!(request.valid());
    for field in [
        "peer_pid",
        "actor",
        "executor",
        "job_kind",
        "claim_token",
        "path",
    ] {
        let mut value = serde_json::to_value(&request).unwrap();
        value[field] = serde_json::json!("forbidden selector");
        assert!(serde_json::from_value::<ClaimPostProcessJobRequest>(value).is_err());
    }
    let fail = FailPostProcessJobRequest {
        proof: request.proof,
        lease,
        operation_id: Uuid::new_v4(),
        fence: request.fence,
        error_code: PostProcessFailureCode::PostProcessFailed,
    };
    let mut value = serde_json::to_value(&fail).unwrap();
    value["error_code"] = serde_json::json!("C:/private/path: arbitrary stderr");
    assert!(serde_json::from_value::<FailPostProcessJobRequest>(value).is_err());
    assert!(!format!("{fail:?}").contains(fail.fence.expose()));
    assert!(!format!("{fail:?}").contains(fail.proof.secret.expose()));
}

#[test]
fn receipts_bind_the_original_claim_and_keep_expired_history_inert() {
    let (worker, request, lease) = fixture();
    assert!(lease.claimed_for(&request, &worker));
    for mutation in 0..8 {
        let mut forged = lease.clone();
        match mutation {
            0 => forged.operation_id = Uuid::new_v4(),
            1 => forged.claim_id = Uuid::new_v4(),
            2 => forged.worker_session_id = Uuid::new_v4(),
            3 => forged.library_epoch = Uuid::new_v4(),
            4 => forged.meeting_id = Uuid::new_v4(),
            5 => forged.input.transcript_revision = "foreign-source".into(),
            6 => forged.expires_at = worker.expires_at + Duration::seconds(1),
            _ => forged.job_revision = request.job.revision.clone(),
        }
        assert!(!forged.claimed_for(&request, &worker), "forgery {mutation}");
    }
    let mut historical = lease;
    historical.expires_at = Utc::now() - Duration::seconds(1);
    assert!(historical.claimed_for(&request, &worker));
    // Merely receiving history never changes its expiry or creates a fresh
    // nonce. The server must recheck the real live claim for each new write.
    assert!(historical.expires_at < Utc::now());
}

#[test]
fn renew_and_failure_replies_cannot_reuse_a_job_revision_or_change_source() {
    let (worker, claim, lease) = fixture();
    let renew = RenewPostProcessJobRequest {
        proof: claim.proof.clone(),
        lease: lease.clone(),
        operation_id: Uuid::new_v4(),
        fence: claim.fence.clone(),
        ttl_seconds: 30,
    };
    let mut renewed = lease.clone();
    renewed.operation_id = renew.operation_id;
    renewed.job_revision = "renewed-job".into();
    renewed.change_sequence += 1;
    assert!(renewed.renewed_for(&renew, &worker));
    renewed.change_sequence = lease.change_sequence;
    assert!(!renewed.renewed_for(&renew, &worker));
    let request = FailPostProcessJobRequest {
        proof: claim.proof,
        lease: lease.clone(),
        operation_id: Uuid::new_v4(),
        fence: claim.fence,
        error_code: PostProcessFailureCode::PostProcessInputUnavailable,
    };
    let mut receipt = PostProcessFailureReceipt {
        operation_id: request.operation_id,
        claim_id: lease.claim_id,
        worker_session_id: lease.worker_session_id,
        library_epoch: lease.library_epoch,
        job_id: lease.job_id.clone(),
        meeting_id: lease.meeting_id,
        job_revision: "failed-job".into(),
        lifecycle_revision: "failed-life".into(),
        error_code: request.error_code,
        change_sequence: lease.change_sequence + 2,
        dedup_until: Utc::now() + Duration::days(7),
    };
    assert!(receipt.valid_for(&request));
    receipt.lifecycle_revision = lease.input.lifecycle_revision;
    assert!(!receipt.valid_for(&request));
}

#[test]
fn page_validation_checks_budget_progress_and_terminal_page_snapshot() {
    let (_, claim, _) = fixture();
    let mut request = PostProcessJobsRequest::new(claim.proof.clone());
    let scope = "a".repeat(32);
    let next = PostProcessJobCursor {
        library_scope: scope.clone(),
        library_epoch: claim.proof.library_epoch,
        worker_session_id: claim.proof.session_id,
        snapshot_sequence: 8,
        eligible_at: Utc::now(),
        after_job_id: claim.job.job_id.clone(),
        query_hash: "b".repeat(64),
    };
    let mut page = PostProcessJobPage {
        library_scope: scope,
        snapshot_sequence: 8,
        eligible_at: next.eligible_at,
        items: vec![claim.job.clone()],
        next: Some(next.clone()),
    };
    assert!(page.valid_for(&request));
    page.next.as_mut().unwrap().after_job_id = "skipped-row".into();
    assert!(!page.valid_for(&request));
    page.next = Some(next.clone());
    request.cursor = Some(next);
    page.items.clear();
    page.next = None;
    assert!(page.valid_for(&request));
    page.library_scope = "c".repeat(32);
    assert!(!page.valid_for(&request));
    request.cursor = None;
    request.limit = 100;
    request.max_bytes = 1024;
    page.library_scope = "a".repeat(32);
    page.items = (0..10)
        .map(|n| {
            let mut job = claim.job.clone();
            job.job_id = format!("job-{n:03}");
            job
        })
        .collect();
    assert!(page.items.iter().all(PostProcessJob::valid));
    assert!(!desktop_body_fits(&page, request.max_bytes as usize));
    assert!(!page.valid_for(&request));
}

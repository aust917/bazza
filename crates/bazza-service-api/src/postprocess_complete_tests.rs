use super::*;
use serde_json::json;

fn completion() -> (
    CompleteFixturePostProcessRequest,
    CompleteFixturePostProcessReceipt,
) {
    let now = Utc::now();
    let session = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    let request = CompleteFixturePostProcessRequest {
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
            job_id: "fixture-post-process".into(),
            meeting_id: Uuid::new_v4(),
            job_revision: "running-job".into(),
            input: PostProcessSourceFence {
                capture_id: Uuid::new_v4(),
                capture_generation: Uuid::new_v4(),
                checkpoint_revision: "ready-checkpoint".into(),
                lifecycle_revision: "ready-life".into(),
                transcript_revision: "cleaned-transcript".into(),
                source_id: Some("local-source".into()),
            },
            expires_at: now - Duration::seconds(10),
            change_sequence: 4,
            dedup_until: now + Duration::days(7) - Duration::seconds(30),
        },
        fence: WorkerSecret::random(),
        operation_id: Uuid::new_v4(),
    };
    let receipt = CompleteFixturePostProcessReceipt {
        operation_id: request.operation_id,
        claim_id: request.lease.claim_id,
        worker_session_id: session,
        library_epoch: epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        job_revision: "completed-job".into(),
        input: request.lease.input.clone(),
        cleanup: FixtureCleanupOutcome::Accepted,
        embeddings: FixtureExcludedStage::OutsideFixtureScope,
        summary: FixtureSummaryOutcome::Committed {
            revision: "published-summary".into(),
        },
        webhook: FixtureExcludedStage::OutsideFixtureScope,
        change_sequence: 5,
        dedup_until: now + Duration::days(7),
    };
    (request, receipt)
}

#[test]
fn historical_receipt_is_inert_and_pins_every_input_revision() {
    let (request, receipt) = completion();
    assert!(request.valid(), "an expired lease can request exact replay");
    assert!(receipt.valid_for(&request));

    let mut forged = receipt.clone();
    forged.input.source_id = Some("replacement-source".into());
    assert!(!forged.valid_for(&request));
    let mut forged = receipt.clone();
    forged.input.lifecycle_revision = "other-life".into();
    assert!(!forged.valid_for(&request));
    let mut forged = receipt.clone();
    forged.input.checkpoint_revision = "other-checkpoint".into();
    assert!(!forged.valid_for(&request));
    let mut forged = receipt.clone();
    forged.job_revision.clone_from(&request.lease.job_revision);
    assert!(!forged.valid_for(&request));
    let mut forged = receipt.clone();
    forged.change_sequence = MAX_CHANGE_SEQUENCE + 1;
    assert!(!forged.valid_for(&request));
    let mut forged = receipt.clone();
    forged.dedup_until = request.lease.expires_at - Duration::seconds(1);
    assert!(!forged.valid_for(&request));
    let mut forged = receipt;
    forged.summary = FixtureSummaryOutcome::Committed {
        revision: "contains\nunsafe bytes".into(),
    };
    assert!(!forged.valid_for(&request));
}

#[test]
fn completion_has_no_caller_selected_output_or_returned_authority() {
    let (request, mut receipt) = completion();
    let encoded_request = serde_json::to_value(&request).unwrap();
    for field in ["status", "cleanup", "summary", "webhook"] {
        let mut forged = encoded_request.clone();
        forged[field] = json!("done");
        assert!(serde_json::from_value::<CompleteFixturePostProcessRequest>(forged).is_err());
    }

    receipt.cleanup = FixtureCleanupOutcome::SkippedEmptyTranscript;
    receipt.summary = FixtureSummaryOutcome::OutsideFixtureScope {};
    assert!(receipt.valid_for(&request));
    let encoded = serde_json::to_value(&receipt).unwrap();
    assert_eq!(encoded["summary"], json!({"kind": "outside_fixture_scope"}));
    assert_eq!(encoded["cleanup"], "skipped_empty_transcript");
    for field in ["lease", "expires_at", "proof", "fence", "token"] {
        assert!(encoded.get(field).is_none());
        let mut forged = encoded.clone();
        forged[field] = json!("not a terminal receipt");
        assert!(serde_json::from_value::<CompleteFixturePostProcessReceipt>(forged).is_err());
    }
    let mut forged = encoded.clone();
    forged["embeddings"] = json!("committed");
    assert!(serde_json::from_value::<CompleteFixturePostProcessReceipt>(forged).is_err());
    let mut forged = encoded;
    forged["summary"]["revision"] = json!("spurious");
    assert!(serde_json::from_value::<CompleteFixturePostProcessReceipt>(forged).is_err());
}

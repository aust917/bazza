use super::*;
use crate::postprocess::PostProcessSourceFence;
use chrono::Duration;

fn request() -> PostProcessCleanupRequest {
    let session = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    PostProcessCleanupRequest {
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
            job_id: "cleanup-job".into(),
            meeting_id: Uuid::new_v4(),
            job_revision: "claimed-job".into(),
            input: PostProcessSourceFence {
                capture_id: Uuid::new_v4(),
                capture_generation: Uuid::new_v4(),
                checkpoint_revision: "checkpoint-old".into(),
                lifecycle_revision: "finalized-life".into(),
                transcript_revision: "transcript-old".into(),
                source_id: Some("captured-source".into()),
            },
            expires_at: Utc::now() + Duration::seconds(120),
            change_sequence: 5,
            dedup_until: Utc::now() + Duration::days(7),
        },
        fence: WorkerSecret::random(),
        operation_id: Uuid::new_v4(),
        operation: PostProcessCleanupOperation::Begin {
            upload_id: Uuid::new_v4(),
            total_bytes: 2,
            sha256: "a".repeat(64),
        },
    }
}

fn status(request: &PostProcessCleanupRequest) -> PostProcessCleanupStatus {
    PostProcessCleanupStatus {
        upload_id: request.operation.upload_id(),
        claim_id: request.lease.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        checkpoint_revision: request.lease.input.checkpoint_revision.clone(),
        state: PostProcessCleanupUploadState::Open,
        total_bytes: 2,
        received_bytes: 0,
        next_sequence: 1,
        expires_at: Utc::now() + Duration::hours(1),
    }
}

fn commit_request() -> PostProcessCleanupRequest {
    let mut request = request();
    request.operation = PostProcessCleanupOperation::Commit {
        upload_id: request.operation.upload_id(),
    };
    request
}

fn receipt(request: &PostProcessCleanupRequest) -> PostProcessCleanupReceipt {
    let mut lease = request.lease.clone();
    lease.operation_id = request.operation_id;
    lease.job_revision = "job-after-cleanup".into();
    lease.input.checkpoint_revision = "checkpoint-next".into();
    lease.input.transcript_revision = "transcript-next".into();
    lease.change_sequence += 2;
    lease.dedup_until = Utc::now() + Duration::days(7);
    PostProcessCleanupReceipt {
        operation_id: request.operation_id,
        upload_id: request.operation.upload_id(),
        lease,
        changed_segments: 1,
        outcome: PostProcessCleanupOutcome::Applied,
    }
}

#[test]
fn closed_operations_and_debug_do_not_accept_or_expose_body_hash_or_authority() {
    let mut request = request();
    let hash = "a".repeat(64);
    request.operation = PostProcessCleanupOperation::Append {
        upload_id: request.operation.upload_id(),
        sequence: 1,
        byte_offset: 0,
        text: "PRIVATE CLEANUP BODY\0🙂".into(),
        sha256: hash.clone(),
    };
    assert!(request.valid());
    let debug = format!("{request:?}");
    for private in [
        "PRIVATE CLEANUP BODY",
        hash.as_str(),
        request.proof.secret.expose(),
        request.fence.expose(),
    ] {
        assert!(!debug.contains(private));
    }
    let update = PostProcessCleanupUpdate {
        segment_id: 1,
        text: "PRIVATE UPDATE".into(),
    };
    assert!(!format!("{update:?}").contains(&update.text));
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["operation"]["op"], "append");
    for field in ["actor", "peer_pid", "path", "raw", "manual", "provider"] {
        let mut bad = value.clone();
        bad[field] = serde_json::json!("forbidden");
        assert!(serde_json::from_value::<PostProcessCleanupRequest>(bad).is_err());
    }
    let mut bad = value;
    bad["operation"]["op"] = serde_json::json!("replace_raw");
    assert!(serde_json::from_value::<PostProcessCleanupRequest>(bad).is_err());
    assert!(serde_json::from_str::<PostProcessCleanupUpdate>(
        r#"{"segment_id":1,"text":"x","manual":true}"#
    )
    .is_err());
}

#[test]
fn request_budget_includes_proof_lease_and_json_escaping_and_preserves_history_shape() {
    let mut request = request();
    assert!(request.valid());
    request.lease.expires_at = Utc::now() - Duration::seconds(1);
    assert!(
        request.valid(),
        "historical receipts do not refresh expired authority"
    );
    let upload_id = request.operation.upload_id();
    for total_bytes in [0, MAX_POST_PROCESS_CLEANUP_BODY_BYTES + 1] {
        request.operation = PostProcessCleanupOperation::Begin {
            upload_id,
            total_bytes,
            sha256: "a".repeat(64),
        };
        assert!(!request.valid());
    }
    request.operation = PostProcessCleanupOperation::Begin {
        upload_id,
        total_bytes: MAX_POST_PROCESS_CLEANUP_BODY_BYTES,
        sha256: "a".repeat(64),
    };
    assert!(request.valid());
    request.operation = PostProcessCleanupOperation::Append {
        upload_id,
        sequence: 1,
        byte_offset: 0,
        text: String::new(),
        sha256: "a".repeat(64),
    };
    let overhead = serde_json::to_vec(&request).unwrap().len();
    if let PostProcessCleanupOperation::Append { text, .. } = &mut request.operation {
        *text = "x".repeat(MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES - overhead);
    }
    assert_eq!(
        serde_json::to_vec(&request).unwrap().len(),
        MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES
    );
    assert!(request.valid());
    if let PostProcessCleanupOperation::Append { text, .. } = &mut request.operation {
        text.push('x');
    }
    assert!(!request.valid());
    for (sequence, byte_offset, text, hash) in [
        (0, 0, "x".into(), "a".repeat(64)),
        (
            MAX_POST_PROCESS_CLEANUP_CHUNKS + 1,
            0,
            "x".into(),
            "a".repeat(64),
        ),
        (1, u64::MAX, "x".into(), "a".repeat(64)),
        (1, 0, String::new(), "a".repeat(64)),
        (1, 0, "x".into(), "A".repeat(64)),
        (1, 0, "\u{1}".repeat(6000), "a".repeat(64)),
    ] {
        request.operation = PostProcessCleanupOperation::Append {
            upload_id,
            sequence,
            byte_offset,
            text,
            sha256: hash,
        };
        assert!(!request.valid());
    }
}

#[test]
fn body_validation_preserves_empty_nul_unicode_duplicate_order_and_sparse_ids() {
    let updates = vec![
        PostProcessCleanupUpdate {
            segment_id: 9,
            text: " raw\0🙂 ".into(),
        },
        PostProcessCleanupUpdate {
            segment_id: 9,
            text: String::new(),
        },
        PostProcessCleanupUpdate {
            segment_id: MAX_SAFE,
            text: "last sparse target".into(),
        },
    ];
    assert!(post_process_cleanup_body_valid(&updates));
    let json = serde_json::to_string(&updates).unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<PostProcessCleanupUpdate>>(&json).unwrap(),
        updates
    );
    assert!(post_process_cleanup_body_valid(&[]));
    for segment_id in [0, -1, MAX_SAFE + 1] {
        assert!(!post_process_cleanup_body_valid(&[
            PostProcessCleanupUpdate {
                segment_id,
                text: "x".into()
            }
        ]));
    }
    let mut many = (1..=MAX_POST_PROCESS_CLEANUP_UPDATES)
        .map(|segment_id| PostProcessCleanupUpdate {
            segment_id: segment_id as i64,
            text: String::new(),
        })
        .collect::<Vec<_>>();
    assert!(post_process_cleanup_body_valid(&many));
    many.push(PostProcessCleanupUpdate {
        segment_id: 1,
        text: String::new(),
    });
    assert!(!post_process_cleanup_body_valid(&many));
    let escaped = PostProcessCleanupUpdate {
        segment_id: 1,
        text: "\u{1}".repeat(MAX_POST_PROCESS_CLEANUP_BODY_BYTES as usize / 6 + 1),
    };
    assert!(!post_process_cleanup_body_valid(&[escaped]));
}

#[test]
fn upload_snapshots_pin_starting_checkpoint_and_progress_without_freshness_promises() {
    let request = request();
    let original = status(&request);
    assert!(PostProcessCleanupReply::Upload(original.clone()).valid_for(&request));
    for fault in 0..15 {
        let mut bad = original.clone();
        match fault {
            0 => bad.upload_id = Uuid::new_v4(),
            1 => bad.claim_id = Uuid::new_v4(),
            2 => bad.worker_session_id = Uuid::new_v4(),
            3 => bad.library_epoch = Uuid::new_v4(),
            4 => bad.job_id = "other-job".into(),
            5 => bad.meeting_id = Uuid::new_v4(),
            6 => bad.checkpoint_revision = "next-checkpoint".into(),
            7 => bad.total_bytes = 0,
            8 => bad.received_bytes = 3,
            9 => bad.next_sequence = 0,
            10 => bad.next_sequence = MAX_POST_PROCESS_CLEANUP_CHUNKS + 2,
            11 => bad.state = PostProcessCleanupUploadState::Committed,
            12 => bad.expires_at = DateTime::from_timestamp(0, 0).unwrap(),
            13 => bad.next_sequence = 2,
            _ => bad.expires_at = Utc::now() + Duration::days(365),
        }
        assert!(
            !PostProcessCleanupReply::Upload(bad).valid_for(&request),
            "fault {fault}"
        );
    }
    let mut past = original;
    past.state = PostProcessCleanupUploadState::Expired;
    past.expires_at = Utc::now() - Duration::hours(1);
    assert!(PostProcessCleanupReply::Upload(past).valid_for(&request));
}

#[test]
fn append_ack_and_terminal_operation_kinds_are_checked() {
    let mut request = request();
    let mut status = status(&request);
    let upload_id = request.operation.upload_id();
    request.operation = PostProcessCleanupOperation::Append {
        upload_id,
        sequence: 1,
        byte_offset: 0,
        text: "[]".into(),
        sha256: "a".repeat(64),
    };
    assert!(!PostProcessCleanupReply::Upload(status.clone()).valid_for(&request));
    status.received_bytes = 2;
    status.next_sequence = 2;
    assert!(PostProcessCleanupReply::Upload(status.clone()).valid_for(&request));
    for operation in [
        PostProcessCleanupOperation::Abort { upload_id },
        PostProcessCleanupOperation::Commit { upload_id },
    ] {
        request.operation = operation;
        assert!(!PostProcessCleanupReply::Upload(status.clone()).valid_for(&request));
        let mut terminal = status.clone();
        terminal.state = PostProcessCleanupUploadState::Committed;
        assert!(PostProcessCleanupReply::Upload(terminal).valid_for(&request));
    }
}

#[test]
fn committed_receipt_rotates_checkpoint_and_job_but_preserves_original_input_and_expiry() {
    let mut request = commit_request();
    let original = receipt(&request);
    assert!(PostProcessCleanupReply::Committed(original.clone()).valid_for(&request));
    for fault in 0..20 {
        let mut bad = original.clone();
        match fault {
            0 => bad.operation_id = Uuid::new_v4(),
            1 => bad.upload_id = Uuid::new_v4(),
            2 => bad.lease.operation_id = Uuid::new_v4(),
            3 => bad.lease.claim_id = Uuid::new_v4(),
            4 => bad.lease.worker_session_id = Uuid::new_v4(),
            5 => bad.lease.library_epoch = Uuid::new_v4(),
            6 => bad.lease.job_id = "other-job".into(),
            7 => bad.lease.meeting_id = Uuid::new_v4(),
            8 => bad.lease.job_revision = request.lease.job_revision.clone(),
            9 => {
                bad.lease.input.checkpoint_revision =
                    request.lease.input.checkpoint_revision.clone()
            }
            10 => bad.lease.input.lifecycle_revision = "other-life".into(),
            11 => bad.lease.input.capture_id = Uuid::new_v4(),
            12 => bad.lease.input.capture_generation = Uuid::new_v4(),
            13 => bad.lease.input.source_id = None,
            14 => bad.lease.expires_at += Duration::seconds(1),
            15 => bad.lease.change_sequence = request.lease.change_sequence,
            16 => bad.changed_segments = MAX_POST_PROCESS_CLEANUP_UPDATES as u32 + 1,
            17 => {
                bad.lease.input.transcript_revision =
                    request.lease.input.transcript_revision.clone()
            }
            18 => bad.lease.dedup_until = bad.lease.expires_at - Duration::seconds(1),
            _ => bad.lease.dedup_until = Utc::now() + Duration::days(365),
        }
        assert!(
            !PostProcessCleanupReply::Committed(bad).valid_for(&request),
            "fault {fault}"
        );
    }
    let mut noop = original;
    noop.changed_segments = 0;
    noop.outcome = PostProcessCleanupOutcome::Noop;
    noop.lease.input.transcript_revision = request.lease.input.transcript_revision.clone();
    assert!(PostProcessCleanupReply::Committed(noop.clone()).valid_for(&request));
    noop.lease.expires_at = Utc::now() - Duration::seconds(1);
    request.lease.expires_at = noop.lease.expires_at;
    assert!(
        PostProcessCleanupReply::Committed(noop.clone()).valid_for(&request),
        "old receipt does not renew a lease"
    );
    request.operation = PostProcessCleanupOperation::Status {
        upload_id: request.operation.upload_id(),
    };
    assert!(!PostProcessCleanupReply::Committed(noop).valid_for(&request));
}

//! Authorization, retry and resource bounds for private cleanup staging.
//! Controlled database clock faults do not stand in for OS peer verification.
use super::*;

fn peer(f: &Fixture) -> u32 {
    f.store
        .conn
        .lock()
        .query_row(
            "SELECT peer_pid FROM service_worker_sessions WHERE id=?1",
            [f.proof.session_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn begin(f: &Fixture, upload_id: Uuid, total_bytes: u64) -> PostProcessCleanupRequest {
    request(
        f,
        PostProcessCleanupOperation::Begin {
            upload_id,
            total_bytes,
            sha256: sha(b"[]"),
        },
    )
}

fn denied_without_writes(f: &Fixture, request: &PostProcessCleanupRequest) {
    let before = database(&f.store);
    assert!(matches!(call(f, request), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
}

fn conflict_without_writes(f: &Fixture, request: &PostProcessCleanupRequest) {
    let before = database(&f.store);
    assert!(matches!(
        call(f, request),
        Err(DataError::IdempotencyConflict)
    ));
    assert_eq!(database(&f.store), before);
}

fn upload_status(reply: PostProcessCleanupReply) -> PostProcessCleanupStatus {
    let PostProcessCleanupReply::Upload(status) = reply else {
        panic!("upload status expected")
    };
    status
}

/// Expire the current claim, then issue a new claim through the real service
/// state machine. The original checkpoint remains unchanged during reclaim.
fn reclaim(f: &mut Fixture, another_worker: bool) {
    let pid = peer(f);
    f.store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [&f.lease.job_id],
        )
        .unwrap();
    if another_worker {
        let registration = WorkerRegistration {
            session_id: Uuid::new_v4(),
            secret: WorkerSecret::random(),
            capabilities: vec![WorkerCapability::PostProcess],
        };
        f.store
            .service_register_worker(pid, f.epoch, &registration, 300)
            .unwrap();
        f.proof = WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: f.epoch,
        };
    }
    let job = f
        .store
        .service_post_process_jobs(pid, f.epoch, &PostProcessJobsRequest::new(f.proof.clone()))
        .unwrap()
        .items
        .into_iter()
        .find(|job| job.job_id == f.lease.job_id)
        .unwrap();
    let fence = WorkerSecret::random();
    let lease = f
        .store
        .service_claim_post_process_job(
            pid,
            f.epoch,
            &ClaimPostProcessJobRequest {
                proof: f.proof.clone(),
                job,
                claim_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                fence: fence.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap();
    assert_ne!(lease.claim_id, f.lease.claim_id);
    assert_eq!(lease.input, f.lease.input);
    f.lease = lease;
    f.fence = fence;
}

#[test]
fn every_exact_history_authenticates_before_replay_or_argument_hash_checks() {
    for fault in 0..7 {
        let f = fixture(CaptureProvider::Local);
        let text = body(&[(f.ids[0], "private automatic result")]);
        let (aborted_id, begin, append) = stage(&f, &text);
        let status = request(
            &f,
            PostProcessCleanupOperation::Status {
                upload_id: aborted_id,
            },
        );
        call(&f, &status).unwrap();
        let abort = request(
            &f,
            PostProcessCleanupOperation::Abort {
                upload_id: aborted_id,
            },
        );
        call(&f, &abort).unwrap();
        let (committed_id, _, _) = stage(&f, &text);
        let commit = request(
            &f,
            PostProcessCleanupOperation::Commit {
                upload_id: committed_id,
            },
        );
        assert!(matches!(
            call(&f, &commit).unwrap(),
            PostProcessCleanupReply::Committed(_)
        ));
        let histories = [begin, append[0].clone(), status, abort, commit];
        let actual_pid = peer(&f);
        match fault {
            0 => {
                f.store
                    .conn
                    .lock()
                    .execute(
                        "UPDATE service_worker_sessions SET revoked=1 WHERE id=?1",
                        [f.proof.session_id.to_string()],
                    )
                    .unwrap();
            }
            1 => {
                f.store.conn.lock().execute(
                    "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
                    [f.proof.session_id.to_string()],
                ).unwrap();
            }
            2 => {
                f.store.conn.lock().execute(
                    "UPDATE service_worker_sessions SET capabilities_json='[\"capture\"]' WHERE id=?1",
                    [f.proof.session_id.to_string()],
                ).unwrap();
            }
            3 => {
                f.store
                    .conn
                    .lock()
                    .execute(
                        "UPDATE service_worker_sessions SET library_epoch=?2 WHERE id=?1",
                        params![f.proof.session_id.to_string(), Uuid::new_v4().to_string()],
                    )
                    .unwrap();
            }
            _ => {}
        }
        for mut historical in histories {
            if fault == 4 {
                historical.proof.secret = WorkerSecret::random();
            }
            let before = database(&f.store);
            let result = f.store.service_post_process_cleanup(
                if fault == 5 {
                    actual_pid + 1
                } else {
                    actual_pid
                },
                if fault == 6 { Uuid::new_v4() } else { f.epoch },
                &historical,
            );
            assert!(matches!(result, Err(DataError::Forbidden)), "fault {fault}");
            assert_eq!(database(&f.store), before, "auth fault {fault} wrote state");
        }
    }
}

#[test]
fn fresh_wrong_fence_is_denied_and_a_changed_historical_fence_conflicts() {
    let f = fixture(CaptureProvider::Soniox);
    let text = body(&[(f.ids[0], "automatic")]);
    let (upload_id, mut historical, _) = stage(&f, &text);
    let mut commit = request(&f, PostProcessCleanupOperation::Commit { upload_id });
    commit.fence = WorkerSecret::random();
    denied_without_writes(&f, &commit);
    historical.fence = WorkerSecret::random();
    conflict_without_writes(&f, &historical);
}

#[test]
fn explicit_renew_preserves_upload_but_commit_requires_the_new_exact_lease() {
    let mut f = fixture(CaptureProvider::Local);
    let text = body(&[(f.ids[0], "renewed result\0 exact")]);
    let (upload_id, historical, _) = stage(&f, &text);
    let initial = upload_status(call(&f, &historical).unwrap());
    let old_lease = f.lease.clone();
    f.lease = f
        .store
        .service_renew_post_process_job(
            peer(&f),
            f.epoch,
            &RenewPostProcessJobRequest {
                proof: f.proof.clone(),
                lease: old_lease.clone(),
                fence: f.fence.clone(),
                operation_id: Uuid::new_v4(),
                ttl_seconds: 180,
            },
        )
        .unwrap();
    assert_eq!(f.lease.claim_id, old_lease.claim_id);
    assert_eq!(f.lease.input, old_lease.input);
    assert_ne!(f.lease.job_revision, old_lease.job_revision);
    assert!(f.lease.expires_at > old_lease.expires_at);
    let status = upload_status(
        call(
            &f,
            &request(&f, PostProcessCleanupOperation::Status { upload_id }),
        )
        .unwrap(),
    );
    assert_eq!(status.expires_at, initial.expires_at);
    assert_eq!(status.checkpoint_revision, initial.checkpoint_revision);
    let mut stale = request(&f, PostProcessCleanupOperation::Commit { upload_id });
    stale.lease = old_lease;
    let before = database(&f.store);
    assert!(matches!(call(&f, &stale), Err(DataError::Conflict { .. })));
    assert_eq!(database(&f.store), before);
    let commit = request(&f, PostProcessCleanupOperation::Commit { upload_id });
    let PostProcessCleanupReply::Committed(receipt) = call(&f, &commit).unwrap() else {
        panic!("committed result expected")
    };
    assert_eq!(receipt.changed_segments, 1);
    assert_eq!(receipt.lease.expires_at, f.lease.expires_at);
    assert_eq!(receipt.lease.claim_id, f.lease.claim_id);
    assert_eq!(
        call(&f, &historical).unwrap(),
        PostProcessCleanupReply::Upload(initial)
    );
}

#[test]
fn reclaimed_claim_cannot_adopt_old_upload_or_refresh_old_operation_history() {
    let mut f = fixture(CaptureProvider::Soniox);
    let text = body(&[(f.ids[0], "owned by first claim")]);
    let (upload_id, historical, append) = stage(&f, &text);
    let historical_reply = call(&f, &historical).unwrap();
    let old_lease = f.lease.clone();
    let old_fence = f.fence.clone();
    reclaim(&mut f, false);
    assert_eq!(call(&f, &historical).unwrap(), historical_reply);
    for operation in [
        PostProcessCleanupOperation::Status { upload_id },
        PostProcessCleanupOperation::Commit { upload_id },
        PostProcessCleanupOperation::Abort { upload_id },
    ] {
        let current_claim = request(&f, operation);
        denied_without_writes(&f, &current_claim);
        let mut retired_claim = current_claim;
        retired_claim.lease = old_lease.clone();
        retired_claim.fence = old_fence.clone();
        denied_without_writes(&f, &retired_claim);
    }
    let mut old_append = append[0].clone();
    old_append.operation_id = Uuid::new_v4();
    denied_without_writes(&f, &old_append);
    let mut adopted_append = old_append;
    adopted_append.lease = f.lease.clone();
    adopted_append.fence = f.fence.clone();
    denied_without_writes(&f, &adopted_append);
    assert!(payload_count(&f.store, upload_id) > 0);
}

#[test]
fn expiry_erases_text_but_preserves_upload_and_chunk_identity_past_operation_dedup() {
    let f = fixture(CaptureProvider::Local);
    let text = body(&[(f.ids[0], "retire this private body\0🙂")]);
    let (upload_id, historical, append) = stage(&f, &text);
    let historical_reply = call(&f, &historical).unwrap();
    assert!(payload_count(&f.store, upload_id) > 0);
    f.store.conn.lock().execute(
        "UPDATE service_post_process_cleanup_uploads SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
        [upload_id.to_string()],
    ).unwrap();
    assert_eq!(
        f.store
            .service_expire_post_process_cleanup_uploads(1)
            .unwrap(),
        1
    );
    assert_eq!(
        f.store
            .service_expire_post_process_cleanup_uploads(1)
            .unwrap(),
        0
    );
    assert_eq!(payload_count(&f.store, upload_id), 0);
    assert_eq!(call(&f, &historical).unwrap(), historical_reply);
    let status = upload_status(
        call(
            &f,
            &request(&f, PostProcessCleanupOperation::Status { upload_id }),
        )
        .unwrap(),
    );
    assert_eq!(status.state, PostProcessCleanupUploadState::Expired);
    assert_eq!(status.received_bytes, text.len() as u64);
    let input_before = f.lease.input.clone();
    let expired_commit = upload_status(
        call(
            &f,
            &request(&f, PostProcessCleanupOperation::Commit { upload_id }),
        )
        .unwrap(),
    );
    assert_eq!(expired_commit.state, PostProcessCleanupUploadState::Expired);
    assert_eq!(
        postprocess::candidate_on(&f.store.conn.lock(), &f.lease.job_id)
            .unwrap()
            .input,
        input_before
    );
    f.store.conn.lock().execute(
        "UPDATE service_idempotency SET expires_at='2000-01-01T00:00:00Z' WHERE principal=?1 AND tool=?2",
        params![format!("worker:{}", f.proof.session_id), PostProcessCleanupReply::TOOL],
    ).unwrap();
    let retained = upload_status(call(&f, &historical).unwrap());
    assert_eq!(retained.state, PostProcessCleanupUploadState::Expired);
    assert_eq!(retained.upload_id, upload_id);
    assert_eq!(payload_count(&f.store, upload_id), 0);
    let mut changed_begin = historical.clone();
    changed_begin.operation_id = Uuid::new_v4();
    if let PostProcessCleanupOperation::Begin { total_bytes, .. } = &mut changed_begin.operation {
        *total_bytes += 1;
    }
    conflict_without_writes(&f, &changed_begin);
    let mut changed_append = append[0].clone();
    changed_append.operation_id = Uuid::new_v4();
    if let PostProcessCleanupOperation::Append { text, sha256, .. } = &mut changed_append.operation
    {
        text.push('x');
        *sha256 = sha(text.as_bytes());
    }
    conflict_without_writes(&f, &changed_append);
    assert_eq!(payload_count(&f.store, upload_id), 0);
}

#[test]
fn operation_uuid_is_global_across_uploads_and_changed_chunks_never_replace_payload() {
    let f = fixture(CaptureProvider::Local);
    let text = body(&[(f.ids[0], "original staged value")]);
    let (upload_id, mut same_operation, append) = stage(&f, &text);
    if let PostProcessCleanupOperation::Begin { upload_id, .. } = &mut same_operation.operation {
        *upload_id = Uuid::new_v4();
    }
    conflict_without_writes(&f, &same_operation);
    let mut changed = append[0].clone();
    if let PostProcessCleanupOperation::Append { text, sha256, .. } = &mut changed.operation {
        text.push('!');
        *sha256 = sha(text.as_bytes());
    }
    conflict_without_writes(&f, &changed);
    changed.operation_id = Uuid::new_v4();
    conflict_without_writes(&f, &changed);
    let mut cross_kind = request(&f, PostProcessCleanupOperation::Status { upload_id });
    cross_kind.operation_id = append[0].operation_id;
    conflict_without_writes(&f, &cross_kind);
    let mut out_of_order = append[0].clone();
    out_of_order.operation_id = Uuid::new_v4();
    if let PostProcessCleanupOperation::Append { sequence, .. } = &mut out_of_order.operation {
        *sequence += 1;
    }
    let before = database(&f.store);
    assert!(matches!(
        call(&f, &out_of_order),
        Err(DataError::Invalid(_))
    ));
    assert_eq!(database(&f.store), before);
}

#[test]
fn open_upload_count_is_global_across_workers_and_housekeeping_releases_reservations() {
    let mut f = fixture(CaptureProvider::Local);
    let mut uploads = Vec::new();
    for _ in 0..MAX_POST_PROCESS_CLEANUP_OPEN_UPLOADS {
        let upload = Uuid::new_v4();
        call(&f, &begin(&f, upload, 2)).unwrap();
        uploads.push(upload);
    }
    reclaim(&mut f, true);
    let blocked = begin(&f, Uuid::new_v4(), 2);
    let before = database(&f.store);
    assert!(matches!(call(&f, &blocked), Err(DataError::Busy)));
    assert_eq!(database(&f.store), before);
    f.store.conn.lock().execute(
        "UPDATE service_post_process_cleanup_uploads SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
        [uploads[0].to_string()],
    ).unwrap();
    assert_eq!(
        f.store
            .service_expire_post_process_cleanup_uploads(1)
            .unwrap(),
        1
    );
    assert_eq!(
        upload_status(call(&f, &blocked).unwrap()).state,
        PostProcessCleanupUploadState::Open
    );
    let open: i64 = f
        .store
        .conn
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM service_post_process_cleanup_uploads WHERE state='open'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(open, i64::from(MAX_POST_PROCESS_CLEANUP_OPEN_UPLOADS));
}

#[test]
fn declared_byte_quota_is_enforced_before_body_allocation_and_abort_releases_it() {
    let f = fixture(CaptureProvider::Soniox);
    let mut uploads = Vec::new();
    for _ in 0..MAX_POST_PROCESS_CLEANUP_RESERVED_BYTES / MAX_POST_PROCESS_CLEANUP_BODY_BYTES {
        let upload = Uuid::new_v4();
        call(&f, &begin(&f, upload, MAX_POST_PROCESS_CLEANUP_BODY_BYTES)).unwrap();
        uploads.push(upload);
    }
    let reserved: i64 = f
        .store
        .conn
        .lock()
        .query_row(
            "SELECT SUM(total_bytes) FROM service_post_process_cleanup_uploads WHERE state='open'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reserved as u64, MAX_POST_PROCESS_CLEANUP_RESERVED_BYTES);
    let blocked = begin(&f, Uuid::new_v4(), 2);
    let before = database(&f.store);
    assert!(matches!(call(&f, &blocked), Err(DataError::Busy)));
    assert_eq!(database(&f.store), before);
    assert_eq!(payload_count(&f.store, uploads[0]), 0);
    let abort = request(
        &f,
        PostProcessCleanupOperation::Abort {
            upload_id: uploads[0],
        },
    );
    assert_eq!(
        upload_status(call(&f, &abort).unwrap()).state,
        PostProcessCleanupUploadState::Aborted
    );
    call(
        &f,
        &begin(&f, Uuid::new_v4(), MAX_POST_PROCESS_CLEANUP_BODY_BYTES),
    )
    .unwrap();
    let reserved: i64 = f
        .store
        .conn
        .lock()
        .query_row(
            "SELECT SUM(total_bytes) FROM service_post_process_cleanup_uploads WHERE state='open'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reserved as u64, MAX_POST_PROCESS_CLEANUP_RESERVED_BYTES);
}

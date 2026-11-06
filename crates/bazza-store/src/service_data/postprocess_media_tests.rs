//! Database admission only. Core proves the kernel owner and terminal/files.
use super::*;
use chrono::Duration;
use rusqlite::types::Value;

const CAPTURE_PID: u32 = 41;
const PID: u32 = 42;

struct Fixture {
    store: Store,
    epoch: Uuid,
    capture_worker: Uuid,
    proof: WorkerProof,
    worker: RegisteredWorker,
    request: PostProcessMediaRequest,
}

fn register(
    store: &Store,
    epoch: Uuid,
    pid: u32,
    capability: WorkerCapability,
) -> (WorkerProof, RegisteredWorker) {
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities: vec![capability],
    };
    let worker = store
        .service_register_worker(pid, epoch, &registration, 120)
        .unwrap();
    (
        WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: epoch,
        },
        worker,
    )
}

fn fixture() -> Fixture {
    let store = Store::open_in_memory().unwrap();
    let epoch = Uuid::new_v4();
    let (capture_proof, _) = register(&store, epoch, CAPTURE_PID, WorkerCapability::Capture);
    let provider = CaptureSelection {
        provider: CaptureProvider::Local,
        model: Some("whisper-small".into()),
        language: Some("ru".into()),
    };
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Synthetic media admission".into(),
        app: None,
        started_at: Utc::now() - Duration::seconds(2),
        participants: vec![],
        provider: provider.clone(),
    };
    let capture_lease = store
        .service_prepare_capture(CAPTURE_PID, epoch, &capture_proof, &start)
        .unwrap();
    store
        .service_begin_capture(
            CAPTURE_PID,
            epoch,
            &capture_proof,
            &capture_lease,
            &start.fence,
        )
        .unwrap();
    store
        .service_finalize_capture(
            CAPTURE_PID,
            epoch,
            &capture_proof,
            &capture_lease,
            &start.fence,
            &CaptureTerminal {
                raw_screen: None,
                soniox_outcome: None,
                retain_local_live: false,
                status: CaptureTerminalStatus::Completed,
                provider: provider.clone(),
                duration_ms: 1000,
                ended_at: start.started_at + Duration::seconds(1),
                has_video: false,
                last_confirmed_sequence: 1,
                captions: vec![ConfirmedCaption {
                    sequence: 1,
                    start_ms: 0,
                    end_ms: 1000,
                    speaker: None,
                    text: "immutable captured input".into(),
                }],
            },
        )
        .unwrap();
    let (proof, worker) = register(&store, epoch, PID, WorkerCapability::PostProcess);
    let job = store
        .service_post_process_jobs(PID, epoch, &PostProcessJobsRequest::new(proof.clone()))
        .unwrap()
        .items
        .remove(0);
    let fence = WorkerSecret::random();
    let lease = store
        .service_claim_post_process_job(
            PID,
            epoch,
            &ClaimPostProcessJobRequest {
                proof: proof.clone(),
                job,
                claim_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                fence: fence.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap();
    let request = PostProcessMediaRequest {
        proof: proof.clone(),
        lease,
        fence,
        owner_nonce: Uuid::new_v4(),
        expected_provider: provider,
    };
    Fixture {
        store,
        epoch,
        capture_worker: capture_proof.session_id,
        proof,
        worker,
        request,
    }
}

fn read(f: &Fixture, request: &PostProcessMediaRequest) -> DataResult<PostProcessMediaContext> {
    f.store
        .service_post_process_media_context(PID, f.epoch, request)
}

type Database = Vec<(String, Vec<Vec<Value>>)>;
fn database(store: &Store) -> Database {
    let conn = store.conn.lock();
    let tables = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|name| {
            let query = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
            let columns = conn.prepare(&query).unwrap().column_count();
            let order = (1..=columns)
                .map(|column| column.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let rows = conn
                .prepare(&format!("{query} ORDER BY {order}"))
                .unwrap()
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<Vec<Value>>>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

#[test]
fn media_context_returns_original_capture_identity_and_performs_no_database_writes() {
    let f = fixture();
    // The historical capture registration need not remain live. Only the
    // current post-process registration supplies authority to this read.
    f.store.conn.lock().execute("UPDATE service_worker_sessions SET revoked=1,expires_at='2000-01-01T00:00:00Z' WHERE id=?1",[f.capture_worker.to_string()]).unwrap();
    let before = database(&f.store);
    let context = read(&f, &f.request).unwrap();
    assert_eq!(context.capture_worker_session_id, f.capture_worker);
    assert_ne!(context.capture_worker_session_id, f.proof.session_id);
    assert_eq!(context.last_confirmed_sequence, 1);
    assert_eq!(context.provider, f.request.expected_provider);
    assert_eq!(context.expires_at, f.request.lease.expires_at);
    assert_eq!(context.library_scope.len(), 32);
    assert_eq!(read(&f, &f.request).unwrap(), context);
    let mut next_owner = f.request.clone();
    next_owner.owner_nonce = Uuid::new_v4();
    assert_eq!(read(&f, &next_owner).unwrap(), context);
    assert_eq!(database(&f.store), before);
    let debug = format!("{:?}", f.request);
    assert!(!debug.contains(f.proof.secret.expose()));
    assert!(!debug.contains(f.request.fence.expose()));
}

#[test]
fn current_pid_epoch_proof_capability_and_registration_precede_capture_reads() {
    let f = fixture();
    let before = database(&f.store);
    for (pid, epoch) in [(PID + 1, f.epoch), (PID, Uuid::new_v4())] {
        assert!(matches!(
            f.store
                .service_post_process_media_context(pid, epoch, &f.request),
            Err(DataError::Forbidden)
        ));
    }
    let mut wrong = f.request.clone();
    wrong.proof.secret = WorkerSecret::random();
    assert!(matches!(read(&f, &wrong), Err(DataError::Forbidden)));
    wrong = f.request.clone();
    wrong.fence = WorkerSecret::random();
    assert!(matches!(read(&f, &wrong), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
    let (foreign, _) = register(&f.store, f.epoch, 45, WorkerCapability::PostProcess);
    wrong = f.request.clone();
    wrong.proof = foreign;
    wrong.lease.worker_session_id = wrong.proof.session_id;
    let before = database(&f.store);
    assert!(matches!(
        f.store
            .service_post_process_media_context(45, f.epoch, &wrong),
        Err(DataError::Forbidden)
    ));
    assert_eq!(database(&f.store), before);
    for sql in [
        "UPDATE service_worker_sessions SET capabilities_json='[\"capture\"]' WHERE id=?1",
        "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
    ] {
        f.store
            .conn
            .lock()
            .execute(sql, [f.proof.session_id.to_string()])
            .unwrap();
        let before = database(&f.store);
        assert!(matches!(read(&f, &f.request), Err(DataError::Forbidden)));
        assert_eq!(database(&f.store), before);
        f.store.conn.lock().execute("UPDATE service_worker_sessions SET capabilities_json='[\"post_process\"]',expires_at=?2 WHERE id=?1",params![f.proof.session_id.to_string(),f.worker.expires_at]).unwrap();
    }
    f.store
        .service_close_worker(PID, f.epoch, &f.proof)
        .unwrap();
    // Valid JSON with an invalid provider shape would fail target decoding if
    // a revoked registration were allowed to inspect its capture first.
    f.store
        .conn
        .lock()
        .execute("UPDATE service_capture_leases SET provider_json='{}'", [])
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(read(&f, &f.request), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
}

#[test]
fn exact_provider_and_actual_stored_expiry_cannot_be_changed_by_the_caller() {
    let f = fixture();
    let before = database(&f.store);
    for fault in 0..6 {
        let mut wrong = f.request.clone();
        match fault {
            0 => wrong.expected_provider.provider = CaptureProvider::Soniox,
            1 => wrong.expected_provider.model = None,
            2 => wrong.expected_provider.language = Some("en".into()),
            3 => wrong.lease.expires_at += Duration::seconds(1),
            4 => wrong.lease.expires_at -= Duration::seconds(1),
            _ => wrong.lease.expires_at = Utc::now() - Duration::seconds(1),
        }
        assert!(
            matches!(read(&f, &wrong), Err(DataError::Forbidden)),
            "fault {fault}"
        );
    }
    let mut wrong = f.request.clone();
    wrong.owner_nonce = Uuid::nil();
    assert!(matches!(read(&f, &wrong), Err(DataError::Invalid(_))));
    assert_eq!(database(&f.store), before);
    f.store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [&f.request.lease.job_id],
        )
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(read(&f, &f.request), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
}

#[test]
fn meeting_source_lifecycle_and_claim_generation_are_pinned_to_the_live_binding() {
    for fault in 0..7 {
        let f = fixture();
        let mut wrong = f.request.clone();
        match fault {
            0 => wrong.lease.meeting_id = Uuid::new_v4(),
            1 => wrong.lease.input.capture_id = Uuid::new_v4(),
            2 => wrong.lease.input.capture_generation = Uuid::new_v4(),
            3 => wrong.lease.input.source_id = Some(Uuid::new_v4().to_string()),
            4 => wrong.lease.input.lifecycle_revision = Uuid::new_v4().to_string(),
            5 => wrong.lease.input.transcript_revision = Uuid::new_v4().to_string(),
            _ => wrong.lease.claim_id = Uuid::new_v4(),
        }
        let before = database(&f.store);
        assert!(
            matches!(read(&f, &wrong), Err(DataError::Forbidden)),
            "fault {fault}"
        );
        assert_eq!(database(&f.store), before);
    }
    for sql in [
        "UPDATE service_meeting_state SET tombstoned=1 WHERE meeting_id=?1",
        "UPDATE meetings SET status='ready' WHERE id=?1",
        "UPDATE service_jobs SET cancel_requested=1 WHERE entity_id=?1",
    ] {
        let f = fixture();
        f.store
            .conn
            .lock()
            .execute(sql, [f.request.lease.meeting_id.to_string()])
            .unwrap();
        let before = database(&f.store);
        assert!(
            matches!(read(&f, &f.request), Err(DataError::Forbidden)),
            "{sql}"
        );
        assert_eq!(database(&f.store), before);
    }
}

#[test]
fn actual_source_replacement_invalidates_media_admission_without_refreshing_input() {
    let f = fixture();
    let id = f.request.lease.meeting_id.to_string();
    let context = MutationContext {
        principal: "fixture-user".into(),
        idempotency_key: "replace-media-source".into(),
    };
    let replacement = TranscriptWrite {
        expected_revision: f.request.lease.input.transcript_revision.clone(),
        segments: vec![crate::NewSegment {
            start_ms: 0,
            end_ms: 1000,
            speaker: None,
            text: "replacement input".into(),
        }],
        source_label: None,
        language: None,
        origin: TranscriptImportOrigin::Imported,
    };
    assert!(matches!(
        f.store
            .service_replace_transcript(&context, &id, &replacement),
        Err(DataError::Busy)
    ));
    // Privileged fixture-only status fault exercises the real source mutation;
    // this read protocol never relaxes the production Processing guard.
    f.store
        .conn
        .lock()
        .execute("UPDATE meetings SET status='ready' WHERE id=?1", [&id])
        .unwrap();
    f.store
        .service_replace_transcript(&context, &id, &replacement)
        .unwrap();
    f.store
        .conn
        .lock()
        .execute("UPDATE meetings SET status='processing' WHERE id=?1", [&id])
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(
        read(&f, &f.request),
        Err(DataError::Conflict { .. })
    ));
    assert_eq!(database(&f.store), before);
}

#[test]
fn renew_and_reclaim_require_the_current_receipt_fence_and_expiry() {
    let f = fixture();
    let renewed = f
        .store
        .service_renew_post_process_job(
            PID,
            f.epoch,
            &RenewPostProcessJobRequest {
                proof: f.proof.clone(),
                lease: f.request.lease.clone(),
                operation_id: Uuid::new_v4(),
                fence: f.request.fence.clone(),
                ttl_seconds: 120,
            },
        )
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(
        read(&f, &f.request),
        Err(DataError::Conflict { .. })
    ));
    let mut current = f.request.clone();
    current.lease = renewed;
    assert_eq!(
        read(&f, &current).unwrap().expires_at,
        current.lease.expires_at
    );
    assert_eq!(database(&f.store), before);
    f.store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [&current.lease.job_id],
        )
        .unwrap();
    let (proof, _) = register(&f.store, f.epoch, 45, WorkerCapability::PostProcess);
    let job = f
        .store
        .service_post_process_jobs(45, f.epoch, &PostProcessJobsRequest::new(proof.clone()))
        .unwrap()
        .items
        .remove(0);
    let fence = WorkerSecret::random();
    let lease = f
        .store
        .service_claim_post_process_job(
            45,
            f.epoch,
            &ClaimPostProcessJobRequest {
                proof: proof.clone(),
                job,
                claim_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                fence: fence.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(read(&f, &current), Err(DataError::Forbidden)));
    current.proof = proof;
    current.lease = lease;
    current.fence = fence;
    let context = f
        .store
        .service_post_process_media_context(45, f.epoch, &current)
        .unwrap();
    assert_eq!(context.capture_worker_session_id, f.capture_worker);
    assert_eq!(context.expires_at, current.lease.expires_at);
    assert_eq!(database(&f.store), before);
}

#[test]
fn capture_worker_and_confirmed_sequence_projections_are_bounded_and_checked() {
    for sequence in [-1_i64, 10_001] {
        let f = fixture();
        f.store
            .conn
            .lock()
            .execute(
                "UPDATE service_capture_leases SET last_confirmed_sequence=?1",
                [sequence],
            )
            .unwrap();
        let before = database(&f.store);
        assert!(matches!(
            read(&f, &f.request),
            Err(DataError::Invalid("post-process confirmed sequence"))
        ));
        assert_eq!(database(&f.store), before);
    }
    for id in ["not-a-uuid".to_string(), "a".repeat(128 * 1024)] {
        let f = fixture();
        let conn = f.store.conn.lock();
        conn.execute("INSERT INTO service_worker_sessions(id,peer_pid,secret_hash,library_epoch,capabilities_json,expires_at,created_at) SELECT ?1,peer_pid,secret_hash,library_epoch,capabilities_json,expires_at,created_at FROM service_worker_sessions WHERE id=?2",params![id,f.capture_worker.to_string()]).unwrap();
        conn.execute(
            "UPDATE service_capture_leases SET worker_session_id=?1",
            [&id],
        )
        .unwrap();
        drop(conn);
        let before = database(&f.store);
        assert!(matches!(
            read(&f, &f.request),
            Err(DataError::Invalid("post-process capture worker"))
        ));
        assert_eq!(database(&f.store), before);
    }
}

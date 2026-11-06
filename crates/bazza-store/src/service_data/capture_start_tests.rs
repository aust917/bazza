//! SQL state-machine tests only. Core tests must independently prove kernel
//! peer identity and the exact pristine/worker-owned OS artifact guard. PID 42
//! here is deliberately synthetic and does not replace those ownership checks.
use super::*;
use std::sync::{Arc, Barrier};

fn reserve(store: &Store) -> (Uuid, WorkerProof, CaptureStart, CaptureLease) {
    let epoch = Uuid::new_v4();
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities: vec![WorkerCapability::Capture],
    };
    store
        .service_register_worker(42, epoch, &registration, 30)
        .unwrap();
    let proof = WorkerProof {
        session_id: registration.session_id,
        secret: registration.secret,
        library_epoch: epoch,
    };
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Reserved synthetic capture".into(),
        app: Some("fixture".into()),
        started_at: Utc::now(),
        participants: vec!["Анна".into(), "Sam".into()],
        provider: CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("fixture-soniox".into()),
            language: Some("ru-RU".into()),
        },
    };
    let lease = store
        .service_prepare_capture(42, epoch, &proof, &start)
        .unwrap();
    (epoch, proof, start, lease)
}

fn phase(store: &Store, capture: Uuid) -> String {
    store
        .conn
        .lock()
        .query_row(
            "SELECT state FROM service_capture_leases WHERE id=?1",
            [capture.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn expire(store: &Store, proof: &WorkerProof) {
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [proof.session_id.to_string()],
        )
        .unwrap();
}

fn assert_no_jobs(store: &Store) {
    let conn = store.conn.lock();
    for table in ["service_jobs", "service_deletions"] {
        assert_eq!(
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn begin_requires_reserved_authority_and_live_lease_but_replays_after_expiry() {
    let store = Store::open_in_memory().unwrap();
    let (epoch, proof, start, lease) = reserve(&store);
    assert_eq!(phase(&store, lease.capture_id), "reserved");
    assert!(store.service_pending_captures(32).unwrap().is_empty());
    assert!(store
        .service_pending_reservations(epoch, 32)
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.service_begin_capture(43, epoch, &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_begin_capture(42, Uuid::new_v4(), &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    let mut wrong = proof.clone();
    wrong.secret = WorkerSecret::random();
    assert!(matches!(
        store.service_begin_capture(42, epoch, &wrong, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    let mut foreign_lease = lease.clone();
    foreign_lease.generation = Uuid::new_v4();
    assert!(matches!(
        store.service_begin_capture(42, epoch, &proof, &foreign_lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_begin_capture(42, epoch, &proof, &lease, &WorkerSecret::random()),
        Err(DataError::Forbidden)
    ));
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_worker_sessions SET capabilities_json='[\"post_process\"]' WHERE id=?1",
            [proof.session_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        store.service_begin_capture(42, epoch, &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_worker_sessions SET capabilities_json='[\"capture\"]' WHERE id=?1",
            [proof.session_id.to_string()],
        )
        .unwrap();
    let premature = CaptureTerminal {
        raw_screen: None,
        soniox_outcome: None,
        retain_local_live: false,
        status: CaptureTerminalStatus::Completed,
        provider: start.provider.clone(),
        duration_ms: 0,
        ended_at: start.started_at,
        has_video: false,
        last_confirmed_sequence: 0,
        captions: Vec::new(),
    };
    assert!(matches!(
        store.service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &premature),
        Err(DataError::Busy)
    ));
    expire(&store, &proof);
    assert!(matches!(
        store.service_begin_capture(42, epoch, &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    assert_eq!(
        store
            .service_prepare_capture(42, epoch, &proof, &start)
            .unwrap(),
        lease
    );
    assert_eq!(phase(&store, lease.capture_id), "reserved");
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_worker_sessions SET expires_at=?2 WHERE id=?1",
            params![
                proof.session_id.to_string(),
                Utc::now() + Duration::seconds(30)
            ],
        )
        .unwrap();
    assert_eq!(
        store
            .service_begin_capture(42, epoch, &proof, &lease, &start.fence)
            .unwrap(),
        lease
    );
    expire(&store, &proof);
    assert_eq!(
        store
            .service_begin_capture(42, epoch, &proof, &lease, &start.fence)
            .unwrap(),
        lease
    );
    assert_eq!(phase(&store, lease.capture_id), "capturing");
    assert_eq!(
        store.service_pending_captures(32).unwrap(),
        vec![lease.clone()]
    );
    assert!(matches!(
        store.service_abort_capture_context(42, &proof, &lease, &start.fence),
        Err(DataError::Busy)
    ));
    assert!(matches!(
        store.service_abort_capture(42, &proof, &lease, &start.fence),
        Err(DataError::Busy)
    ));
    assert!(store
        .service_abort_capture_receipt(42, &proof, &lease, &start.fence)
        .unwrap()
        .is_none());
    assert_no_jobs(&store);
}

#[test]
fn abort_retains_failed_card_provider_and_receipt_through_purge_without_starting_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&crate::database_path(temp.path())).unwrap();
    let (epoch, proof, start, lease) = reserve(&store);
    let id = lease.meeting_id.to_string();
    let provider_before: String = store
        .conn
        .lock()
        .query_row(
            "SELECT provider_json FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let key = format!("stt:{id}");
    let mut provenance = store
        .get_setting::<serde_json::Value>(&key)
        .unwrap()
        .unwrap();
    provenance["private_extension"] = serde_json::json!({"raw":" Exact текст \n"});
    store.set_setting(&key, &provenance).unwrap();
    store.set_setting("unrelated-setting", &"keep").unwrap();
    assert!(store
        .service_abort_capture_receipt(42, &proof, &lease, &start.fence)
        .unwrap()
        .is_none());
    assert!(matches!(
        store.service_abort_capture(43, &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_abort_capture(42, &proof, &lease, &WorkerSecret::random()),
        Err(DataError::Forbidden)
    ));
    expire(&store, &proof);
    let outcome = store
        .service_abort_capture(42, &proof, &lease, &start.fence)
        .unwrap();
    assert_eq!(phase(&store, lease.capture_id), "aborted");
    let meeting = store.get_meeting(&id).unwrap().unwrap();
    assert_eq!(meeting.status, crate::MeetingStatus::Failed);
    assert_eq!(meeting.error.as_deref(), Some("CAPTURE_NOT_STARTED"));
    assert_eq!(meeting.ended_at, Some(meeting.started_at));
    assert_eq!(meeting.duration_ms, 0);
    assert_eq!(meeting.participants, start.participants);
    assert_eq!(meeting.title, start.title);
    assert!(!temp.path().join("recordings").exists());
    provenance["capture_pending"] = false.into();
    provenance["capture_aborted"] = true.into();
    provenance["status"] = "incomplete".into();
    provenance["error"] = "Soniox transcription did not start.".into();
    assert_eq!(
        store.get_setting::<serde_json::Value>(&key).unwrap(),
        Some(provenance)
    );
    let provider_after: String = store
        .conn
        .lock()
        .query_row(
            "SELECT provider_json FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(provider_before, provider_after);
    assert_eq!(
        store.service_capture_selection(lease.capture_id).unwrap(),
        start.provider
    );
    assert_no_jobs(&store);
    assert_eq!(
        store
            .service_abort_capture_receipt(42, &proof, &lease, &start.fence)
            .unwrap(),
        Some(outcome.clone())
    );
    assert_eq!(
        store
            .service_abort_capture(42, &proof, &lease, &start.fence)
            .unwrap(),
        outcome
    );
    assert!(matches!(
        store.service_prepare_capture(42, epoch, &proof, &start),
        Err(DataError::Busy)
    ));
    let deletion = store
        .service_delete_meeting(
            &MutationContext {
                principal: "fixture-test".into(),
                idempotency_key: "purge-aborted".into(),
            },
            &id,
            &outcome.revision,
        )
        .unwrap();
    assert!(store
        .service_run_purge(deletion.job_id.as_deref().unwrap())
        .unwrap());
    assert!(store.get_meeting(&id).unwrap().is_none());
    assert_eq!(
        store
            .service_abort_capture_receipt(42, &proof, &lease, &start.fence)
            .unwrap(),
        Some(outcome.clone())
    );
    assert_eq!(
        store
            .service_abort_capture(42, &proof, &lease, &start.fence)
            .unwrap(),
        outcome
    );
    assert!(matches!(
        store.service_prepare_capture(42, epoch, &proof, &start),
        Err(DataError::Busy)
    ));
    assert_eq!(
        store
            .get_setting::<String>("unrelated-setting")
            .unwrap()
            .as_deref(),
        Some("keep")
    );
    let receipt: String = store
        .conn
        .lock()
        .query_row(
            "SELECT outcome_json FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!receipt.contains(start.fence.expose()));
    assert!(!receipt.contains(proof.secret.expose()));
}

#[test]
fn startup_reconciliation_only_considers_old_epoch_reservations() {
    let store = Store::open_in_memory().unwrap();
    let (old, proof, start, lease) = reserve(&store);
    let current = Uuid::new_v4();
    assert!(store
        .service_pending_reservations(old, 32)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.service_pending_reservations(current, 1).unwrap(),
        vec![lease.clone()]
    );
    assert!(matches!(
        store.service_pending_reservations(current, 0),
        Err(DataError::Invalid(_))
    ));
    assert!(matches!(
        store.service_pending_reservations(current, 33),
        Err(DataError::Invalid(_))
    ));
    assert!(matches!(
        store.service_recover_reserved_capture(old, &lease),
        Err(DataError::Forbidden)
    ));
    let mut foreign = lease.clone();
    foreign.generation = Uuid::new_v4();
    assert!(matches!(
        store.service_recover_reserved_capture(current, &foreign),
        Err(DataError::Forbidden)
    ));
    let outcome = store
        .service_recover_reserved_capture(current, &lease)
        .unwrap();
    assert_eq!(phase(&store, lease.capture_id), "aborted");
    assert_eq!(
        store
            .service_recover_reserved_capture(current, &lease)
            .unwrap(),
        outcome
    );
    assert_eq!(
        store
            .service_abort_capture_receipt(42, &proof, &lease, &start.fence)
            .unwrap(),
        Some(outcome)
    );
    assert!(store
        .service_pending_reservations(current, 32)
        .unwrap()
        .is_empty());
    assert_no_jobs(&store);

    let active = Store::open_in_memory().unwrap();
    let (epoch, proof, start, lease) = reserve(&active);
    active
        .service_begin_capture(42, epoch, &proof, &lease, &start.fence)
        .unwrap();
    assert!(active
        .service_pending_reservations(current, 32)
        .unwrap()
        .is_empty());
    assert!(matches!(
        active.service_recover_reserved_capture(current, &lease),
        Err(DataError::Busy)
    ));
    assert_eq!(phase(&active, lease.capture_id), "capturing");
}

#[test]
fn reserved_reattach_preserves_phase_and_invalidates_old_epoch_mutations() {
    let store = Store::open_in_memory().unwrap();
    let (old, proof, start, lease) = reserve(&store);
    expire(&store, &proof);
    let current = Uuid::new_v4();
    let attached = store
        .service_reattach_capture(42, current, &proof, lease.capture_id, &start.fence, 30)
        .unwrap();
    assert_eq!(attached.library_epoch, current);
    assert_eq!(attached.generation, lease.generation);
    assert_eq!(phase(&store, lease.capture_id), "reserved");
    assert_eq!(
        store
            .service_reattach_capture(42, current, &proof, lease.capture_id, &start.fence, 30)
            .unwrap(),
        attached
    );
    assert!(store
        .service_pending_reservations(current, 32)
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.service_begin_capture(42, old, &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_abort_capture(42, &proof, &lease, &start.fence),
        Err(DataError::Forbidden)
    ));
    let renewed = WorkerProof {
        library_epoch: current,
        ..proof
    };
    assert_eq!(
        store
            .service_begin_capture(42, current, &renewed, &attached, &start.fence)
            .unwrap(),
        attached
    );
    assert!(matches!(
        store.service_abort_capture(42, &renewed, &attached, &start.fence),
        Err(DataError::Busy)
    ));
}

#[test]
fn concurrent_begin_and_abort_have_exactly_one_sql_transition_winner() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (epoch, proof, start, lease) = reserve(&store);
    let gate = Arc::new(Barrier::new(3));
    let (began, aborted) = std::thread::scope(|scope| {
        let first = store.clone();
        let first_gate = gate.clone();
        let proof_ref = &proof;
        let lease_ref = &lease;
        let fence = &start.fence;
        let begin = scope.spawn(move || {
            first_gate.wait();
            first.service_begin_capture(42, epoch, proof_ref, lease_ref, fence)
        });
        let second = store.clone();
        let second_gate = gate.clone();
        let abort = scope.spawn(move || {
            second_gate.wait();
            second.service_abort_capture(42, proof_ref, lease_ref, fence)
        });
        gate.wait();
        (begin.join().unwrap(), abort.join().unwrap())
    });
    match (began, aborted) {
        (Ok(saved), Err(DataError::Busy)) => {
            assert_eq!(saved, lease);
            assert_eq!(phase(&store, lease.capture_id), "capturing");
        }
        (Err(DataError::Busy), Ok(_)) => {
            assert_eq!(phase(&store, lease.capture_id), "aborted");
        }
        other => panic!("expected one begin/abort winner, got {other:?}"),
    }
    assert_no_jobs(&store);
}

#[test]
fn late_begin_or_abort_failure_restores_phase_card_settings_and_receipt() {
    let store = Store::open_in_memory().unwrap();
    let (epoch, proof, start, lease) = reserve(&store);
    let id = lease.meeting_id.to_string();
    let original = store.get_meeting(&id).unwrap().unwrap();
    let setting = store
        .get_setting::<serde_json::Value>(&format!("stt:{id}"))
        .unwrap();
    let changes = store.service_changes(0, 100).unwrap().len();
    store.conn.lock().execute_batch("CREATE TRIGGER fail_begin_phase BEFORE UPDATE OF state ON service_capture_leases WHEN new.state='capturing' BEGIN SELECT RAISE(ABORT,'injected begin failure'); END;").unwrap();
    assert!(matches!(
        store.service_begin_capture(42, epoch, &proof, &lease, &start.fence),
        Err(DataError::Sqlite(_))
    ));
    assert_eq!(phase(&store, lease.capture_id), "reserved");
    store.conn.lock().execute_batch("DROP TRIGGER fail_begin_phase; CREATE TRIGGER fail_abort_receipt BEFORE UPDATE OF state ON service_capture_leases WHEN new.state='aborted' BEGIN SELECT RAISE(ABORT,'injected late abort failure'); END;").unwrap();
    assert!(matches!(
        store.service_abort_capture(42, &proof, &lease, &start.fence),
        Err(DataError::Sqlite(_))
    ));
    assert_eq!(phase(&store, lease.capture_id), "reserved");
    assert_eq!(store.get_meeting(&id).unwrap().unwrap(), original);
    assert_eq!(
        store
            .get_setting::<serde_json::Value>(&format!("stt:{id}"))
            .unwrap(),
        setting
    );
    assert_eq!(store.service_changes(0, 100).unwrap().len(), changes);
    assert_eq!(
        store
            .service_meeting(&id)
            .unwrap()
            .unwrap()
            .lifecycle_revision,
        lease.generation.to_string()
    );
    assert!(store
        .service_abort_capture_receipt(42, &proof, &lease, &start.fence)
        .unwrap()
        .is_none());
    assert_no_jobs(&store);
}

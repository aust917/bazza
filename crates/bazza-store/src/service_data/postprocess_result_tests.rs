//! Synthetic in-memory result commits; no native execution or media access.
use super::*;
use rusqlite::types::Value;

const PID: u32 = 42;

struct Fixture {
    store: Store,
    epoch: Uuid,
    request: CommitPostProcessAutoTitleRequest,
}

fn fixture() -> Fixture {
    let store = Store::open_in_memory().unwrap();
    let epoch = Uuid::new_v4();
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities: vec![WorkerCapability::Capture, WorkerCapability::PostProcess],
    };
    store
        .service_register_worker(PID, epoch, &registration, 120)
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
        title: "originaltitle".into(),
        app: Some("fixture".into()),
        started_at: Utc::now() - Duration::seconds(2),
        participants: vec!["Анна".into()],
        provider: CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("fixture-soniox".into()),
            language: Some("ru".into()),
        },
    };
    let capture = store
        .service_prepare_capture(PID, epoch, &proof, &start)
        .unwrap();
    store
        .service_begin_capture(PID, epoch, &proof, &capture, &start.fence)
        .unwrap();
    store
        .service_finalize_capture(
            PID,
            epoch,
            &proof,
            &capture,
            &start.fence,
            &CaptureTerminal {
                raw_screen: None,
                soniox_outcome: None,
                retain_local_live: false,
                status: CaptureTerminalStatus::Completed,
                provider: start.provider,
                duration_ms: 1000,
                ended_at: start.started_at + Duration::seconds(1),
                has_video: true,
                last_confirmed_sequence: 2,
                captions: vec![
                    ConfirmedCaption {
                        sequence: 1,
                        start_ms: 0,
                        end_ms: 500,
                        speaker: Some("Анна".into()),
                        text: "um rawquasar".into(),
                    },
                    ConfirmedCaption {
                        sequence: 2,
                        start_ms: 500,
                        end_ms: 1000,
                        speaker: None,
                        text: "rawneptune".into(),
                    },
                ],
            },
        )
        .unwrap();
    let id = capture.meeting_id.to_string();
    let segments = store.segments(&id).unwrap();
    let source = store
        .service_transcript_state(&id)
        .unwrap()
        .current_source_id
        .unwrap();
    // Seed both overlay kinds consistently before claiming. This is fixture
    // setup, never a bypass exposed by the registered result operation.
    for (segment, text, manual) in [
        (&segments[0], "automatic cleaned", false),
        (&segments[1], "", true),
    ] {
        let conn = store.conn.lock();
        conn.execute(
            "UPDATE segments SET clean_text=?2,clean_text_manual=?3 WHERE id=?1",
            params![segment.id, text, manual],
        )
        .unwrap();
        conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,?3,?4)",params![source,segment.id.to_string(),text,manual]).unwrap();
    }
    store
        .set_setting(&format!("speaker_embeddings:policy:{id}"), &vec![0.25_f32])
        .unwrap();
    store.rebuild_chunks(&id).unwrap();
    store.service_rebuild_search().unwrap();
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
    let expected_metadata_revision = store
        .service_desktop_meeting(&id)
        .unwrap()
        .unwrap()
        .metadata_revision;
    Fixture {
        store,
        epoch,
        request: CommitPostProcessAutoTitleRequest {
            proof,
            lease,
            fence,
            operation_id: Uuid::new_v4(),
            expected_metadata_revision,
            title: "  autoquasar Орион  ".into(),
        },
    }
}

fn commit(
    f: &Fixture,
    request: &CommitPostProcessAutoTitleRequest,
) -> DataResult<PostProcessAutoTitleReceipt> {
    f.store
        .service_commit_post_process_auto_title(PID, f.epoch, request)
}

type Database = Vec<(String, Vec<Vec<Value>>)>;

fn rows(conn: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = conn.prepare(sql).unwrap();
    let count = statement.column_count();
    let result = statement
        .query_map([], |row| (0..count).map(|index| row.get(index)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    result
}

fn database(store: &Store) -> Database {
    let conn = store.conn.lock();
    let names = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let sql = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
            let count = conn.prepare(&sql).unwrap().column_count();
            let order = (1..=count)
                .map(|column| column.to_string())
                .collect::<Vec<_>>()
                .join(",");
            (name, rows(&conn, &format!("{sql} ORDER BY {order}")))
        })
        .collect()
}

fn protected(f: &Fixture) -> (Database, serde_json::Value, Vec<Vec<Value>>) {
    let mut database: Database = database(&f.store)
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "meetings"
                    | "service_meeting_state"
                    | "service_changes"
                    | "service_idempotency"
                    | "sqlite_sequence"
                    | "service_search_state"
            ) && !name.starts_with("service_search_documents")
                && !name.starts_with("service_search_fts")
        })
        .collect();
    let mut meeting = serde_json::to_value(
        f.store
            .service_meeting(&f.request.lease.meeting_id.to_string())
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    meeting.as_object_mut().unwrap().remove("metadata_revision");
    meeting["meeting"].as_object_mut().unwrap().remove("title");
    let conn = f.store.conn.lock();
    database.push(("protected_meeting_state".into(), rows(&conn,"SELECT meeting_id,content_kind,lifecycle_revision,tombstoned FROM service_meeting_state ORDER BY meeting_id")));
    let search = rows(
        &conn,
        "SELECT * FROM service_search_documents WHERE source_kind<>'title' ORDER BY id",
    );
    (database, meeting, search)
}

#[test]
fn exact_title_updates_only_metadata_and_title_search_without_rebuilding_sources() {
    let f = fixture();
    let before = protected(&f);
    let receipt = commit(&f, &f.request).unwrap();
    let after = f
        .store
        .service_desktop_meeting(&f.request.lease.meeting_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(after.title, f.request.title);
    assert!(
        after.title_auto,
        "automatic output must not pin a user title"
    );
    assert_eq!(after.metadata_revision, receipt.metadata_revision);
    assert_ne!(
        after.metadata_revision,
        f.request.expected_metadata_revision
    );
    assert_eq!(protected(&f), before);
    let hits = f
        .store
        .service_search(&SearchRequest {
            query: "autoquasar".into(),
            fields: vec!["title".into()],
            limit: 20,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].revision, receipt.metadata_revision);
    assert!(f
        .store
        .service_search(&SearchRequest {
            query: "originaltitle".into(),
            fields: vec!["title".into()],
            limit: 20,
            ..Default::default()
        })
        .unwrap()
        .hits
        .is_empty());
    let body: String = f
        .store
        .conn
        .lock()
        .query_row(
            "SELECT outcome_json FROM service_idempotency WHERE tool=?1",
            [TOOL],
            |row| row.get(0),
        )
        .unwrap();
    for private in [
        &f.request.title,
        f.request.proof.secret.expose(),
        f.request.fence.expose(),
    ] {
        assert!(!body.contains(private));
    }
    assert!(!format!("{:?}", f.request).contains(&f.request.title));
    // Metadata output does not rotate ownership or invalidate source cursors.
    let read = PostProcessTranscriptRequest {
        proof: f.request.proof.clone(),
        lease: f.request.lease.clone(),
        fence: f.request.fence.clone(),
        page: DesktopTranscriptRequest {
            meeting_id: f.request.lease.meeting_id.to_string(),
            source_id: None,
            expected_revision: f.request.lease.input.transcript_revision.clone(),
            representation: DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 10,
            max_bytes: 4096,
        },
    };
    assert_eq!(
        f.store
            .service_post_process_transcript(PID, f.epoch, &read)
            .unwrap()
            .items[0]
            .text,
        "um rawquasar"
    );
    assert_eq!(
        f.store
            .service_job(&f.request.lease.job_id)
            .unwrap()
            .revision,
        f.request.lease.job_revision
    );
}

#[test]
fn title_byte_boundary_and_invalid_shapes_are_atomic() {
    let f = fixture();
    let mut request = f.request.clone();
    request.title = "я".repeat(256);
    let receipt = commit(&f, &request).unwrap();
    assert_eq!(
        f.store
            .service_desktop_meeting(&request.lease.meeting_id.to_string())
            .unwrap()
            .unwrap()
            .title,
        request.title
    );
    request.expected_metadata_revision = receipt.metadata_revision;
    for title in [
        format!("{}x", "я".repeat(256)),
        " ".into(),
        "bad\0title".into(),
    ] {
        request.operation_id = Uuid::new_v4();
        request.title = title;
        let before = database(&f.store);
        assert!(matches!(commit(&f, &request), Err(DataError::Invalid(_))));
        assert_eq!(database(&f.store), before);
    }
}

#[test]
fn authentication_precedes_history_and_exact_replay_never_renews_or_reapplies() {
    let f = fixture();
    let receipt = commit(&f, &f.request).unwrap();
    let before = database(&f.store);
    assert_eq!(commit(&f, &f.request).unwrap(), receipt);
    assert_eq!(database(&f.store), before);
    for fault in 0..3 {
        let mut bad = f.request.clone();
        match fault {
            0 => bad.title = "different title".into(),
            1 => bad.expected_metadata_revision = receipt.metadata_revision.clone(),
            _ => bad.fence = WorkerSecret::random(),
        }
        assert!(matches!(
            commit(&f, &bad),
            Err(DataError::IdempotencyConflict)
        ));
    }
    let mut wrong = f.request.clone();
    wrong.proof.secret = WorkerSecret::random();
    assert!(matches!(commit(&f, &wrong), Err(DataError::Forbidden)));
    assert!(matches!(
        f.store
            .service_commit_post_process_auto_title(PID + 1, f.epoch, &f.request),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        f.store
            .service_commit_post_process_auto_title(PID, Uuid::new_v4(), &f.request),
        Err(DataError::Forbidden)
    ));
    assert_eq!(database(&f.store), before);
    for sql in [
        "UPDATE service_worker_sessions SET capabilities_json='[\"capture\"]' WHERE id=?1",
        "UPDATE service_worker_sessions SET revoked=1 WHERE id=?1",
        "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
    ] {
        let fault = fixture();
        commit(&fault, &fault.request).unwrap();
        fault
            .store
            .conn
            .lock()
            .execute(sql, [fault.request.proof.session_id.to_string()])
            .unwrap();
        let before = database(&fault.store);
        assert!(matches!(
            commit(&fault, &fault.request),
            Err(DataError::Forbidden)
        ));
        assert_eq!(database(&fault.store), before);
    }
    f.store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [&f.request.lease.job_id],
        )
        .unwrap();
    let before = database(&f.store);
    assert_eq!(commit(&f, &f.request).unwrap(), receipt);
    let mut next = f.request.clone();
    next.operation_id = Uuid::new_v4();
    next.expected_metadata_revision = receipt.metadata_revision;
    assert!(matches!(commit(&f, &next), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
}

#[test]
fn manual_rename_wins_even_with_fresh_metadata_and_history_remains_inert() {
    let f = fixture();
    let receipt = commit(&f, &f.request).unwrap();
    let rename = f
        .store
        .service_desktop_rename_meeting(
            &MutationContext {
                principal: "desktop".into(),
                idempotency_key: "user-rename".into(),
            },
            &f.request.lease.meeting_id.to_string(),
            &receipt.metadata_revision,
            "User title",
        )
        .unwrap();
    let before = database(&f.store);
    assert_eq!(commit(&f, &f.request).unwrap(), receipt);
    let mut next = f.request.clone();
    next.operation_id = Uuid::new_v4();
    assert!(matches!(commit(&f, &next), Err(DataError::Conflict { .. })));
    next.expected_metadata_revision = rename.revision;
    assert!(matches!(commit(&f, &next), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
    let renewed = f
        .store
        .service_renew_post_process_job(
            PID,
            f.epoch,
            &RenewPostProcessJobRequest {
                proof: f.request.proof.clone(),
                lease: f.request.lease.clone(),
                operation_id: Uuid::new_v4(),
                fence: f.request.fence.clone(),
                ttl_seconds: 120,
            },
        )
        .unwrap();
    assert_ne!(renewed.job_revision, f.request.lease.job_revision);
    assert_eq!(renewed.input, f.request.lease.input);
}

#[test]
fn late_receipt_failure_rolls_back_title_search_metadata_events_and_sequences() {
    let f = fixture();
    f.store.conn.lock().execute_batch("CREATE TRIGGER auto_title_fault BEFORE INSERT ON service_idempotency WHEN NEW.tool='post_process_auto_title' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    let before = database(&f.store);
    assert!(matches!(commit(&f, &f.request), Err(DataError::Sqlite(_))));
    assert_eq!(database(&f.store), before);
    f.store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER auto_title_fault")
        .unwrap();
    let mut forged = f.request.clone();
    forged.lease.change_sequence = 9_007_199_254_740_991;
    let before = database(&f.store);
    assert!(matches!(commit(&f, &forged), Err(DataError::Invalid(_))));
    assert_eq!(
        database(&f.store),
        before,
        "reply pin validation must happen before commit"
    );
    commit(&f, &f.request).unwrap();
}

#[test]
fn new_result_rejects_stale_source_lifecycle_cancel_and_retired_ownership() {
    for sql in [
        "UPDATE service_meeting_state SET lifecycle_revision='different-life' WHERE meeting_id=?1",
        "UPDATE service_transcript_state SET revision='different-transcript' WHERE meeting_id=?1",
        "UPDATE service_transcript_state SET current_source_id='foreign-source' WHERE meeting_id=?1",
        "UPDATE service_jobs SET cancel_requested=1 WHERE entity_id=?1",
        "UPDATE service_jobs SET revision='different-job' WHERE entity_id=?1",
        "UPDATE meetings SET status='ready' WHERE id=?1",
        "UPDATE service_meeting_state SET tombstoned=1 WHERE meeting_id=?1",
        "UPDATE service_worker_job_claims SET state='retired' WHERE job_id=(SELECT id FROM service_jobs WHERE entity_id=?1 AND kind='processing')",
    ] {
        let f = fixture();
        f.store.conn.lock().execute(sql,[f.request.lease.meeting_id.to_string()]).unwrap();
        let before = database(&f.store);
        assert!(commit(&f,&f.request).is_err(),"{sql}");
        assert_eq!(database(&f.store),before,"{sql}");
    }
    let f = fixture();
    let mut request = f.request.clone();
    request.fence = WorkerSecret::random();
    let before = database(&f.store);
    assert!(matches!(commit(&f, &request), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
}

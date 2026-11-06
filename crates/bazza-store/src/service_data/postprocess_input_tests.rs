//! Store snapshots only. Core separately proves kernel PID and artifact guards.
use super::*;
use rusqlite::types::Value;

const PID: u32 = 42;

struct Fixture {
    store: Store,
    epoch: Uuid,
    proof: WorkerProof,
    worker: RegisteredWorker,
    lease: PostProcessJobLease,
    fence: WorkerSecret,
}

fn register(
    store: &Store,
    epoch: Uuid,
    pid: u32,
    capabilities: Vec<WorkerCapability>,
) -> (WorkerProof, RegisteredWorker) {
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities,
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

fn capture(store: &Store, epoch: Uuid, proof: &WorkerProof, text: Option<&str>) -> Uuid {
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Synthetic lease input".into(),
        app: None,
        started_at: Utc::now() - Duration::seconds(2),
        participants: vec![],
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: None,
            language: Some("ru".into()),
        },
    };
    let lease = store
        .service_prepare_capture(PID, epoch, proof, &start)
        .unwrap();
    store
        .service_begin_capture(PID, epoch, proof, &lease, &start.fence)
        .unwrap();
    let captions = text
        .map(|text| {
            vec![ConfirmedCaption {
                sequence: 1,
                start_ms: 0,
                end_ms: 1000,
                speaker: Some("Анна".into()),
                text: text.into(),
            }]
        })
        .unwrap_or_default();
    store
        .service_finalize_capture(
            PID,
            epoch,
            proof,
            &lease,
            &start.fence,
            &CaptureTerminal {
                raw_screen: None,
                soniox_outcome: None,
                retain_local_live: false,
                status: CaptureTerminalStatus::Completed,
                provider: start.provider,
                duration_ms: 1000,
                ended_at: start.started_at + Duration::seconds(1),
                has_video: false,
                last_confirmed_sequence: captions.len() as u64,
                captions,
            },
        )
        .unwrap();
    lease.meeting_id
}

fn fixture(text: Option<&str>) -> Fixture {
    let store = Store::open_in_memory().unwrap();
    let epoch = Uuid::new_v4();
    let (proof, worker) = register(
        &store,
        epoch,
        PID,
        vec![WorkerCapability::Capture, WorkerCapability::PostProcess],
    );
    capture(&store, epoch, &proof, text);
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
    Fixture {
        store,
        epoch,
        proof,
        worker,
        lease,
        fence,
    }
}

fn request(f: &Fixture) -> PostProcessTranscriptRequest {
    PostProcessTranscriptRequest {
        proof: f.proof.clone(),
        lease: f.lease.clone(),
        fence: f.fence.clone(),
        page: DesktopTranscriptRequest {
            meeting_id: f.lease.meeting_id.to_string(),
            source_id: None,
            expected_revision: f.lease.input.transcript_revision.clone(),
            representation: DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 2,
            max_bytes: 2048,
        },
    }
}

fn read(f: &Fixture, request: &PostProcessTranscriptRequest) -> DataResult<DesktopTranscriptPage> {
    f.store
        .service_post_process_transcript(PID, f.epoch, request)
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
fn bounded_utf8_pages_match_desktop_reader_and_never_mutate_the_database() {
    let raw = "сырой🙂 \"exact\"\n".repeat(700);
    let f = fixture(Some(&raw));
    let before = database(&f.store);
    for representation in [
        DesktopTranscriptRepresentation::Raw,
        DesktopTranscriptRepresentation::Effective,
    ] {
        let mut request = request(&f);
        request.page.representation = representation;
        // The explicit original source is also allowed, never an archive.
        request.page.source_id = f.lease.input.source_id.clone();
        let mut text = String::new();
        let mut calls = 0;
        loop {
            let worker = read(&f, &request).unwrap();
            let desktop = f.store.service_desktop_transcript(&request.page).unwrap();
            assert_eq!(
                serde_json::to_value(&worker).unwrap(),
                serde_json::to_value(desktop).unwrap()
            );
            assert!(serde_json::to_vec(&worker).unwrap().len() <= 2048);
            assert_eq!(worker.state.document_state, DocumentState::Present);
            assert_eq!(worker.source_id, f.lease.input.source_id);
            for fragment in &worker.items {
                assert_eq!(fragment.byte_offset, text.len() as u64);
                assert_eq!(fragment.total_bytes, raw.len() as u64);
                text.push_str(&fragment.text);
            }
            calls += 1;
            assert!(calls < 128);
            request.page.cursor = worker.next;
            if request.page.cursor.is_none() {
                break;
            }
        }
        assert!(calls > 1);
        assert_eq!(text, raw);
    }
    assert_eq!(database(&f.store), before);
    let debug = format!("{:?}", request(&f));
    assert!(!debug.contains(f.fence.expose()));
    assert!(!debug.contains(f.proof.secret.expose()));
    assert!(!debug.contains("сырой"));
}

#[test]
fn every_page_rechecks_worker_pid_epoch_secret_capability_expiry_and_revocation() {
    let f = fixture(Some(&"input🙂".repeat(1000)));
    let mut r = request(&f);
    r.page.cursor = read(&f, &r).unwrap().next;
    assert!(r.page.cursor.is_some());
    let before = database(&f.store);
    assert!(matches!(
        f.store
            .service_post_process_transcript(PID + 1, f.epoch, &r),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        f.store
            .service_post_process_transcript(PID, Uuid::new_v4(), &r),
        Err(DataError::Forbidden)
    ));
    let mut wrong = r.clone();
    wrong.proof.secret = WorkerSecret::random();
    assert!(matches!(read(&f, &wrong), Err(DataError::Forbidden)));
    wrong = r.clone();
    wrong.proof.library_epoch = Uuid::new_v4();
    assert!(matches!(read(&f, &wrong), Err(DataError::Forbidden)));
    wrong = r.clone();
    wrong.fence = WorkerSecret::random();
    assert!(matches!(read(&f, &wrong), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
    let (foreign, _) = register(&f.store, f.epoch, 45, vec![WorkerCapability::PostProcess]);
    wrong = r.clone();
    wrong.proof = foreign;
    let before = database(&f.store);
    assert!(matches!(
        f.store.service_post_process_transcript(45, f.epoch, &wrong),
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
        assert!(matches!(read(&f, &r), Err(DataError::Forbidden)));
        assert_eq!(database(&f.store), before);
        f.store.conn.lock().execute("UPDATE service_worker_sessions SET capabilities_json='[\"capture\",\"post_process\"]',expires_at=?2 WHERE id=?1",params![f.proof.session_id.to_string(),f.worker.expires_at]).unwrap();
    }
    f.store
        .service_close_worker(PID, f.epoch, &f.proof)
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(read(&f, &r), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
}

#[test]
fn meeting_source_revision_and_cursor_pins_never_grant_archived_or_foreign_content() {
    let f = fixture(Some(&"original🙂".repeat(1000)));
    let original = request(&f);
    let other = capture(&f.store, f.epoch, &f.proof, Some("OTHER MEETING PRIVATE"));
    let archive = Uuid::new_v4().to_string();
    let archive_json=serde_json::to_string(&serde_json::json!([{"id":999,"start_ms":0,"end_ms":1000,"speaker":null,"text":"ARCHIVED PRIVATE"}])).unwrap();
    f.store.conn.lock().execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,raw_json,created_at) VALUES(?1,?2,'imported',?3,?4)",params![archive,f.lease.meeting_id.to_string(),archive_json,Utc::now()]).unwrap();
    let mut archive_page = original.page.clone();
    archive_page.source_id = Some(archive.clone());
    assert_eq!(
        f.store
            .service_desktop_transcript(&archive_page)
            .unwrap()
            .items[0]
            .text,
        "ARCHIVED PRIVATE"
    );
    let mut requests = vec![];
    let mut wrong = original.clone();
    wrong.page = archive_page;
    requests.push(wrong);
    let mut wrong = original.clone();
    wrong.page.meeting_id = other.to_string();
    requests.push(wrong);
    let mut wrong = original.clone();
    wrong.page.expected_revision = Uuid::new_v4().to_string();
    requests.push(wrong);
    let cursor = read(&f, &original).unwrap().next.unwrap();
    for field in 0..4 {
        let mut wrong = original.clone();
        let mut cursor = cursor.clone();
        match field {
            0 => cursor.source_id = Some(archive.clone()),
            1 => cursor.current_source_id = Some(archive.clone()),
            2 => cursor.meeting_id = other.to_string(),
            _ => cursor.revision = Uuid::new_v4().to_string(),
        }
        wrong.page.cursor = Some(cursor);
        requests.push(wrong);
    }
    let before = database(&f.store);
    for request in requests {
        assert!(matches!(read(&f, &request), Err(DataError::Forbidden)));
    }
    assert_eq!(database(&f.store), before);
}

#[test]
fn actual_transcript_replacement_invalidates_the_original_capture_lease() {
    let f = fixture(Some("immutable original raw"));
    let original = request(&f);
    let id = f.lease.meeting_id.to_string();
    let context = MutationContext {
        principal: "fixture-user".into(),
        idempotency_key: "replace-input".into(),
    };
    let replacement = TranscriptWrite {
        expected_revision: f.lease.input.transcript_revision.clone(),
        segments: vec![crate::NewSegment {
            start_ms: 0,
            end_ms: 1000,
            speaker: None,
            text: "NEW PRIVATE SOURCE".into(),
        }],
        source_label: Some("fixture replacement".into()),
        language: None,
        origin: TranscriptImportOrigin::Imported,
    };
    assert!(matches!(
        f.store
            .service_replace_transcript(&context, &id, &replacement),
        Err(DataError::Busy)
    ));
    // A privileged fixture status fault permits exercising the real mutation;
    // production's Processing guard is never relaxed by this read boundary.
    f.store
        .conn
        .lock()
        .execute("UPDATE meetings SET status='ready' WHERE id=?1", [&id])
        .unwrap();
    let changed = f
        .store
        .service_replace_transcript(&context, &id, &replacement)
        .unwrap();
    f.store
        .conn
        .lock()
        .execute("UPDATE meetings SET status='processing' WHERE id=?1", [&id])
        .unwrap();
    let mut archived = original.page.clone();
    archived.source_id = f.lease.input.source_id.clone();
    archived.expected_revision = changed.revision;
    assert_eq!(
        f.store.service_desktop_transcript(&archived).unwrap().items[0].text,
        "immutable original raw"
    );
    let before = database(&f.store);
    assert!(matches!(
        read(&f, &original),
        Err(DataError::Conflict { .. })
    ));
    let mut refreshed = original.clone();
    refreshed.page.expected_revision = archived.expected_revision;
    assert!(matches!(
        read(&f, &refreshed),
        Err(DataError::Conflict { .. })
    ));
    assert_eq!(database(&f.store), before);
}

#[test]
fn renewed_lease_can_resume_but_expired_or_reclaimed_lease_cannot_read() {
    let f = fixture(Some(&"resume🙂".repeat(1000)));
    let mut r = request(&f);
    r.page.cursor = read(&f, &r).unwrap().next;
    let renewed = f
        .store
        .service_renew_post_process_job(
            PID,
            f.epoch,
            &RenewPostProcessJobRequest {
                proof: f.proof.clone(),
                lease: f.lease.clone(),
                operation_id: Uuid::new_v4(),
                fence: f.fence.clone(),
                ttl_seconds: 120,
            },
        )
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(read(&f, &r), Err(DataError::Conflict { .. })));
    r.lease = renewed.clone();
    assert!(read(&f, &r).unwrap().items[0].byte_offset > 0);
    assert_eq!(database(&f.store), before);
    f.store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [&renewed.job_id],
        )
        .unwrap();
    let before = database(&f.store);
    assert!(matches!(read(&f, &r), Err(DataError::Forbidden)));
    assert_eq!(database(&f.store), before);
    let (proof, _) = register(&f.store, f.epoch, 45, vec![WorkerCapability::PostProcess]);
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
    assert!(matches!(read(&f, &r), Err(DataError::Forbidden)));
    r.proof = proof;
    r.lease = lease;
    r.fence = fence;
    assert!(
        f.store
            .service_post_process_transcript(45, f.epoch, &r)
            .unwrap()
            .items[0]
            .byte_offset
            > 0
    );
    assert_eq!(database(&f.store), before);
}

#[test]
fn absent_captured_input_is_empty_and_invalid_source_state_pairs_are_denied() {
    let f = fixture(None);
    let before = database(&f.store);
    let page = read(&f, &request(&f)).unwrap();
    assert_eq!(page.state.document_state, DocumentState::Absent);
    assert!(
        page.source_id.is_none()
            && page.state.current_source_id.is_none()
            && page.items.is_empty()
            && page.next.is_none()
    );
    assert_eq!(database(&f.store), before);
    for text in [None, Some("present source")] {
        for state in ["absent", "present", "deleted"] {
            if (text.is_none() && state == "absent") || (text.is_some() && state == "present") {
                continue;
            }
            let f = fixture(text);
            f.store
                .conn
                .lock()
                .execute(
                    "UPDATE service_transcript_state SET document_state=?2 WHERE meeting_id=?1",
                    params![f.lease.meeting_id.to_string(), state],
                )
                .unwrap();
            let before = database(&f.store);
            assert!(matches!(read(&f, &request(&f)), Err(DataError::Forbidden)));
            assert_eq!(database(&f.store), before);
        }
    }
}

//! Synthetic v15 lineage and transactional v16 checkpoint migration checks.
use super::*;
use crate::{migrations, Store};
use chrono::{Duration, Utc};
use rusqlite::{params, types::Value, Connection};
use uuid::Uuid;

struct LegacyCapture {
    job: String,
    meeting: String,
    capture: String,
    source: String,
}

fn legacy_database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrations::run_until(&conn, 15).unwrap();
    conn
}

fn seed_capture(conn: &Connection, failed: bool) -> LegacyCapture {
    let fixture = LegacyCapture {
        job: Uuid::new_v4().to_string(),
        meeting: Uuid::new_v4().to_string(),
        capture: Uuid::new_v4().to_string(),
        source: Uuid::new_v4().to_string(),
    };
    let session = Uuid::new_v4().to_string();
    let epoch = Uuid::new_v4().to_string();
    let generation = Uuid::new_v4().to_string();
    let life = Uuid::new_v4().to_string();
    let transcript = Uuid::new_v4().to_string();
    let provider =
        serde_json::json!({"provider":"soniox","model":"legacy-selected","language":"ru-RU"});
    let payload = serde_json::json!({"capture_id":fixture.capture,"capture_generation":generation,"transcript_revision":transcript,"provider":provider,"required_capabilities":["post_process"]});
    let receipt = serde_json::json!({"entity_id":fixture.meeting,"revision":life,"change_sequence":1,"job_id":fixture.job,"dedup_until":"2099-01-01T00:00:00Z"});
    conn.execute("INSERT INTO service_worker_sessions(id,peer_pid,secret_hash,library_epoch,capabilities_json,expires_at,created_at) VALUES(?1,42,?2,?3,'[\"capture\",\"post_process\"]','2099-01-01T00:00:00Z','2026-11-02T00:00:00Z')", params![session,"a".repeat(64),epoch]).unwrap();
    conn.execute("INSERT INTO meetings(id,title,started_at,ended_at,duration_ms,status,created_at,participants_json,title_auto) VALUES(?1,'Legacy exact title','2026-11-02T00:00:00Z','2026-11-02T00:00:01Z',1000,?2,'2026-11-02T00:00:00Z','[\"Анна\"]',1)", params![fixture.meeting,if failed {"failed"} else {"processing"}]).unwrap();
    conn.execute("INSERT INTO service_meeting_state(meeting_id,content_kind,metadata_revision,lifecycle_revision,tombstoned) VALUES(?1,'recorded',?2,?3,0)", params![fixture.meeting,Uuid::new_v4().to_string(),life]).unwrap();
    conn.execute("INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,mandatory,payload_json,created_at,updated_at) VALUES(?1,'processing',?2,'gui_worker','waiting_for_worker',?3,?4,0,?5,'2026-11-02T00:00:00Z','2026-11-02T00:00:00Z')", params![fixture.job,fixture.meeting,Uuid::new_v4().to_string(),session,payload.to_string()]).unwrap();
    conn.execute("INSERT INTO service_capture_leases(id,meeting_id,worker_session_id,library_epoch,generation,fence_hash,start_hash,provider_json,state,last_confirmed_sequence,finalize_hash,outcome_json,job_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'finalized',1,'original-finalize-hash',?9,?10,'2026-11-02T00:00:00Z')", params![fixture.capture,fixture.meeting,session,epoch,generation,"b".repeat(64),"c".repeat(64),provider.to_string(),receipt.to_string(),fixture.job]).unwrap();
    conn.execute("INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual) VALUES(?1,0,1000,'Анна',' Exact original raw Орион\n','',1)", [&fixture.meeting]).unwrap();
    let segment = conn.last_insert_rowid();
    let raw = serde_json::json!([{"id":segment,"start_ms":0,"end_ms":1000,"speaker":"Анна","text":" Exact original raw Орион\n"}]);
    conn.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,language,raw_json,created_at) VALUES(?1,?2,'captured','soniox','ru-RU',?3,'2026-11-02T00:00:00Z')", params![fixture.source,fixture.meeting,raw.to_string()]).unwrap();
    conn.execute("INSERT INTO service_transcript_state(meeting_id,revision,current_source_id,document_state) VALUES(?1,?2,?3,'present')", params![fixture.meeting,transcript,fixture.source]).unwrap();
    conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,'',1)", params![fixture.source,segment.to_string()]).unwrap();
    let caption = ConfirmedCaption {
        sequence: 1,
        start_ms: 0,
        end_ms: 1000,
        speaker: Some("Анна".into()),
        text: " Exact original raw Орион\n".into(),
    };
    let hash = postprocess::fingerprint(&caption, 131_072).unwrap();
    conn.execute("INSERT INTO service_capture_captions(capture_id,sequence,segment_id,caption_hash) VALUES(?1,1,?2,?3)", params![fixture.capture,segment,hash]).unwrap();
    fixture
}

type Database = Vec<(String, Vec<Vec<Value>>)>;
fn database(conn: &Connection) -> Database {
    let tables = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|name| {
            let query = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
            let columns = conn.prepare(&query).unwrap().column_count();
            let order = (1..=columns)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = conn.prepare(&format!("{query} ORDER BY {order}")).unwrap();
            let rows = statement
                .query_map([], |r| (0..columns).map(|n| r.get(n)).collect())
                .unwrap()
                .collect::<rusqlite::Result<Vec<Vec<Value>>>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}
fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn v18_through_v20_preserve_existing_checkpoints_and_staged_bytes() {
    let conn = legacy_database();
    let capture = seed_capture(&conn, false);
    migrations::run_until(&conn, 17).unwrap();
    let candidate = postprocess::candidate_on(&conn, &capture.job).unwrap();
    let upload = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO service_post_process_cleanup_uploads(
         id,worker_session_id,library_epoch,claim_id,job_id,meeting_id,checkpoint_revision,
         input_json,fence_hash,intent_hash,total_bytes,sha256,received_bytes,next_sequence,state,expires_at)
         VALUES(?1,'saved-worker','saved-epoch','saved-claim',?2,?3,?4,?5,
                'saved-fence','saved-intent',2,'saved-body-hash',2,2,'open','2026-11-02T00:01:00Z')",
        params![upload,capture.job,capture.meeting,candidate.input.checkpoint_revision,
            serde_json::to_string(&candidate.input).unwrap()],
    ).unwrap();
    conn.execute(
        "INSERT INTO service_post_process_cleanup_chunks VALUES(?1,1,0,2,'saved-chunk-hash','[]')",
        [&upload],
    )
    .unwrap();
    let before = database(&conn);
    migrations::run_until(&conn, 18).unwrap();
    let mut after = database(&conn);
    after.retain(|(name, _)| name != "service_post_process_local_sources");
    let (_, uploads) = after
        .iter_mut()
        .find(|(name, _)| name == "service_post_process_cleanup_uploads")
        .unwrap();
    assert_eq!(uploads[0].pop(), Some(Value::Text("cleanup".into())));
    assert_eq!(after, before);
    assert_eq!(
        postprocess::candidate_on(&conn, &capture.job).unwrap(),
        candidate
    );
    assert_eq!(count(&conn, "pragma_foreign_key_check"), 0);

    let second = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO service_post_process_cleanup_uploads
         SELECT ?2,worker_session_id,library_epoch,claim_id,job_id,meeting_id,checkpoint_revision,
                input_json,fence_hash,intent_hash,total_bytes,sha256,received_bytes,next_sequence,
                state,expires_at,receipt_json,'local_source'
         FROM service_post_process_cleanup_uploads WHERE id=?1",
        params![upload, second],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO service_post_process_cleanup_chunks
         SELECT ?2,sequence,byte_offset,bytes,sha256,text
         FROM service_post_process_cleanup_chunks WHERE upload_id=?1",
        params![upload, second],
    )
    .unwrap();
    let before_v19 = database(&conn);
    migrations::run_until(&conn, 19).unwrap();
    let after_v19 = database(&conn)
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "service_post_process_speaker_results" | "service_transcript_speaker_overlays"
            )
        })
        .collect::<Database>();
    assert_eq!(after_v19, before_v19);
    assert_eq!(
        postprocess::candidate_on(&conn, &capture.job).unwrap(),
        candidate
    );
    assert_eq!(count(&conn, "pragma_foreign_key_check"), 0);

    let before_v20 = database(&conn);
    migrations::run_until(&conn, 20).unwrap();
    let after_v20 = database(&conn)
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "service_post_process_completions" | "service_post_process_generated_summaries"
            )
        })
        .collect::<Database>();
    assert_eq!(after_v20, before_v20);
    assert_eq!(postprocess::candidate_on(&conn, &capture.job).unwrap(), candidate);
    assert_eq!(count(&conn, "pragma_foreign_key_check"), 0);
}

#[test]
fn v16_seeds_only_strict_captures_and_preserves_original_source_and_manual_empty_bytes() {
    let conn = legacy_database();
    let valid = seed_capture(&conn, false);
    let life = seed_capture(&conn, false);
    let transcript = seed_capture(&conn, false);
    let source = seed_capture(&conn, false);
    let provider = seed_capture(&conn, false);
    let failed = seed_capture(&conn, true);
    conn.execute(
        "UPDATE service_meeting_state SET lifecycle_revision='external-life' WHERE meeting_id=?1",
        [&life.meeting],
    )
    .unwrap();
    conn.execute(
        "UPDATE service_transcript_state SET revision='external-transcript' WHERE meeting_id=?1",
        [&transcript.meeting],
    )
    .unwrap();
    conn.execute("UPDATE service_transcript_state SET current_source_id='foreign-source' WHERE meeting_id=?1", [&source.meeting]).unwrap();
    conn.execute("UPDATE service_capture_leases SET provider_json='{\"provider\":\"local\",\"model\":\"other-model\",\"language\":\"en\"}' WHERE id=?1", [&provider.capture]).unwrap();
    conn.execute("INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,mandatory,payload_json,created_at,updated_at) VALUES('generic-job','processing',?1,'service','running','generic-revision','local',0,'{}','2026-11-02','2026-11-02')", [&valid.meeting]).unwrap();
    let retained = [
        "meetings",
        "segments",
        "service_transcript_sources",
        "service_transcript_state",
        "service_transcript_overlays",
        "service_capture_leases",
        "service_capture_captions",
    ];
    let before = database(&conn)
        .into_iter()
        .filter(|(name, _)| retained.contains(&name.as_str()))
        .collect::<Database>();
    migrations::run_until(&conn, 16).unwrap();
    assert_eq!(count(&conn, "service_post_process_anchors"), 1);
    assert_eq!(count(&conn, "service_post_process_checkpoints"), 1);
    let (job, phase, state, source_id): (String, String, String, Option<String>) = conn
        .query_row(
            "SELECT job_id,phase,state,source_id FROM service_post_process_checkpoints",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(job, valid.job);
    assert_eq!(phase, "captured");
    assert_eq!(state, "active");
    assert_eq!(source_id.as_deref(), Some(valid.source.as_str()));
    let after = database(&conn)
        .into_iter()
        .filter(|(name, _)| retained.contains(&name.as_str()))
        .collect::<Database>();
    assert_eq!(after, before);
    for excluded in [&life, &transcript, &source, &provider, &failed] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM service_post_process_checkpoints WHERE job_id=?1)",
                [&excluded.job],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!exists);
    }
    let before_read = database(&conn);
    let descriptor = postprocess::candidate_on(&conn, &valid.job).unwrap();
    assert!(!descriptor.input.checkpoint_revision.is_empty());
    assert_eq!(
        database(&conn),
        before_read,
        "candidate reads must not refresh checkpoints"
    );
    assert!(conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query([])
        .unwrap()
        .next()
        .unwrap()
        .is_none());
}

#[test]
fn v16_retires_old_bindings_and_interrupts_only_gui_postprocess_without_adopting_receipts() {
    let conn = legacy_database();
    let valid = seed_capture(&conn, false);
    let session: String = conn
        .query_row(
            "SELECT worker_session_id FROM service_capture_leases WHERE id=?1",
            [&valid.capture],
            |r| r.get(0),
        )
        .unwrap();
    let epoch: String = conn
        .query_row(
            "SELECT library_epoch FROM service_capture_leases WHERE id=?1",
            [&valid.capture],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute("UPDATE service_jobs SET state='running',claim_token='old-internal-token',lease_until='2099-01-01T00:00:00Z',revision='old-running-revision' WHERE id=?1", [&valid.job]).unwrap();
    conn.execute("INSERT INTO service_worker_job_claims(claim_id,job_id,worker_session_id,library_epoch,fence_hash,token_hash,input_hash,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'active','2026-11-02T00:00:00Z')", params![Uuid::new_v4().to_string(),valid.job,session,epoch,"d".repeat(64),"e".repeat(64),"f".repeat(64)]).unwrap();
    conn.execute("INSERT INTO service_worker_job_receipts(worker_session_id,operation_id,library_epoch,operation,arguments_hash,outcome_json,expires_at) VALUES(?1,?2,?3,'claim',?4,'{ \"historical\": \"retained exact receipt\" }','2099-01-01T00:00:00Z')",params![session,Uuid::new_v4().to_string(),epoch,"a".repeat(64)]).unwrap();
    conn.execute("INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,mandatory,claim_token,lease_until,payload_json,created_at,updated_at) VALUES('generic-running','processing',?1,'service','running','generic-revision','local',0,'generic-token','2099-01-01T00:00:00Z','{}','2026-11-02','2026-11-02')", [&valid.meeting]).unwrap();
    let receipts = database(&conn)
        .into_iter()
        .find(|(name, _)| name == "service_worker_job_receipts")
        .unwrap();
    migrations::run_until(&conn, 16).unwrap();
    let (state, token, lease, revision): (String, Option<String>, Option<String>, String) = conn
        .query_row(
            "SELECT state,claim_token,lease_until,revision FROM service_jobs WHERE id=?1",
            [&valid.job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, "interrupted");
    assert_eq!(token, None);
    assert_eq!(lease, None);
    assert_ne!(revision, "old-running-revision");
    assert_eq!(
        conn.query_row("SELECT state FROM service_worker_job_claims", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "retired"
    );
    assert_eq!(
        database(&conn)
            .into_iter()
            .find(|(name, _)| name == "service_worker_job_receipts")
            .unwrap(),
        receipts
    );
    assert_eq!(conn.query_row("SELECT state||':'||revision||':'||claim_token FROM service_jobs WHERE id='generic-running'",[],|r|r.get::<_,String>(0)).unwrap(),"running:generic-revision:generic-token");
    let events:i64=conn.query_row("SELECT COUNT(*) FROM service_changes WHERE entity_kind='job' AND entity_id=?1 AND revision=?2",params![valid.job,revision],|r|r.get(0)).unwrap();
    assert_eq!(events, 1);
}

#[test]
fn late_migration_failure_rolls_back_lineage_schema_retirement_events_and_user_version() {
    let conn = legacy_database();
    let valid = seed_capture(&conn, false);
    conn.execute("UPDATE service_jobs SET state='running',claim_token='old-token',lease_until='2099-01-01T00:00:00Z' WHERE id=?1",[&valid.job]).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_checkpoint_migration BEFORE UPDATE ON service_jobs WHEN new.state='interrupted' BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;").unwrap();
    let before = database(&conn);
    assert!(migrations::run_until(&conn, 16).is_err());
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(database(&conn), before);
    let created:i64=conn.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('service_post_process_anchors','service_post_process_checkpoints')",[],|r|r.get(0)).unwrap();
    assert_eq!(created, 0);
}

#[test]
fn new_capture_finalization_rolls_back_on_anchor_failure_then_initializes_readonly_lineage() {
    let store = Store::open_in_memory().unwrap();
    let epoch = Uuid::new_v4();
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities: vec![WorkerCapability::Capture],
    };
    store
        .service_register_worker(42, epoch, &registration, 60)
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
        title: "Synthetic checkpoint finalization".into(),
        app: None,
        started_at: Utc::now() - Duration::seconds(2),
        participants: vec![],
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: Some("fixture-local".into()),
            language: Some("en".into()),
        },
    };
    let lease = store
        .service_prepare_capture(42, epoch, &proof, &start)
        .unwrap();
    store
        .service_begin_capture(42, epoch, &proof, &lease, &start.fence)
        .unwrap();
    let terminal = CaptureTerminal {
        raw_screen: None,
        soniox_outcome: None,
        retain_local_live: false,
        status: CaptureTerminalStatus::Completed,
        provider: start.provider.clone(),
        duration_ms: 1000,
        ended_at: start.started_at + Duration::seconds(1),
        has_video: false,
        last_confirmed_sequence: 1,
        captions: vec![ConfirmedCaption {
            sequence: 1,
            start_ms: 0,
            end_ms: 1000,
            speaker: None,
            text: "Raw is preserved Орион".into(),
        }],
    };
    store.conn.lock().execute_batch("CREATE TRIGGER reject_capture_anchor BEFORE INSERT ON service_post_process_anchors BEGIN SELECT RAISE(ABORT,'injected anchor failure'); END;").unwrap();
    let before = database(&store.conn.lock());
    assert!(store
        .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
        .is_err());
    assert_eq!(
        database(&store.conn.lock()),
        before,
        "late checkpoint failure rolls back captions/source/job/lifecycle/receipt"
    );
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER reject_capture_anchor;")
        .unwrap();
    let finalized = store
        .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
        .unwrap();
    let job = finalized.job_id.unwrap();
    let conn = store.conn.lock();
    assert_eq!(count(&conn, "service_post_process_anchors"), 1);
    assert_eq!(count(&conn, "service_post_process_checkpoints"), 1);
    let before = database(&conn);
    let candidate = postprocess::candidate_on(&conn, &job).unwrap();
    assert_eq!(candidate.input.lifecycle_revision, finalized.revision);
    let checkpoint: String = conn
        .query_row(
            "SELECT revision FROM service_post_process_checkpoints WHERE job_id=?1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(candidate.input.checkpoint_revision, checkpoint);
    assert_eq!(database(&conn), before);
    conn.execute(
        "UPDATE service_transcript_state SET revision='external-edit' WHERE meeting_id=?1",
        [start.meeting_id.to_string()],
    )
    .unwrap();
    let changed = database(&conn);
    assert!(postprocess::candidate_on(&conn, &job).is_err());
    assert_eq!(
        database(&conn),
        changed,
        "external edits never cause a read-side checkpoint refresh"
    );
}

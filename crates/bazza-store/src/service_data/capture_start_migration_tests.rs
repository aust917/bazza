//! Append-v13 compatibility with live foreign keys and pre-begin-protocol rows.
use super::*;
use rusqlite::{params, types::Value};

fn v12_capture_fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    for sql in &MIGRATIONS[..12] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 12).unwrap();
    conn.execute("INSERT INTO service_worker_sessions(id,peer_pid,secret_hash,library_epoch,capabilities_json,expires_at,created_at) VALUES('old-worker',42,?1,'old-epoch','[\"capture\"]','2099-01-01T00:00:00Z','2026-11-01T00:00:00Z')", ["a".repeat(64)]).unwrap();
    conn.execute("INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,mandatory,payload_json,created_at,updated_at) VALUES('old-job','post_process','old-finalized-meeting','gui_worker','waiting_for_worker','job-revision','old-worker',1,'{ \"original\": true }','2026-11-01T00:00:00Z','2026-11-01T00:00:00Z')", []).unwrap();
    for (id, status) in [
        ("old-capturing-meeting", "recording"),
        ("old-finalized-meeting", "processing"),
    ] {
        conn.execute("INSERT INTO meetings(id,title,started_at,ended_at,duration_ms,status,created_at,participants_json,error) VALUES(?1,'Exact legacy title','2026-11-01T00:00:00Z',NULL,2000,?2,'2026-11-01T00:00:00Z','[\"Анна\", \"Sam\"]',' retained legacy diagnostic ')", params![id,status]).unwrap();
    }
    conn.execute("INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual) VALUES(42,'old-finalized-meeting',5,1000,'Анна',' Original raw текст \n','',1)", []).unwrap();
    conn.execute("INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual) VALUES(77,'old-capturing-meeting',1000,2000,NULL,'Second exact raw',NULL,0)", []).unwrap();
    conn.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,language,raw_json,created_at) VALUES('old-source','old-finalized-meeting','captured','soniox','ru-RU','[ { \"text\": \" Original raw текст \\n\" } ]','2026-11-01T00:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES('old-source','42','',1)", []).unwrap();
    conn.execute("INSERT INTO settings(key,value) VALUES('stt:old-finalized-meeting','{ \"provider\": \"soniox\", \"model\": \"old-model\", \"captions\": [ { \"text\": \" Exact caption \" } ] }')", []).unwrap();
    conn.execute(
        "INSERT INTO settings(key,value) VALUES('private:keep','\" exact unrelated value \"')",
        [],
    )
    .unwrap();
    insert_capture(
        &conn,
        "old-capturing",
        "old-capturing-meeting",
        "capturing",
        None,
    );
    insert_capture(
        &conn,
        "old-finalized",
        "old-finalized-meeting",
        "finalized",
        Some("old-job"),
    );
    conn.execute("UPDATE service_capture_leases SET last_confirmed_sequence=1,finalize_hash='exact-finalize-hash',outcome_json='{ \"entity_id\": \"old-finalized-meeting\", \"revision\": \"receipt-revision\", \"change_sequence\": 17, \"job_id\": \"old-job\", \"dedup_until\": \"2099-01-01T00:00:00Z\" }' WHERE id='old-finalized'", []).unwrap();
    conn.execute("INSERT INTO service_capture_captions(capture_id,sequence,segment_id,caption_hash) VALUES('old-finalized',1,42,'exact-caption-hash')", []).unwrap();
    conn
}

fn insert_capture(conn: &Connection, id: &str, meeting: &str, phase: &str, job: Option<&str>) {
    conn.execute("INSERT INTO service_capture_leases(id,meeting_id,worker_session_id,library_epoch,generation,fence_hash,start_hash,provider_json,state,job_id,created_at) VALUES(?1,?2,'old-worker','old-epoch','exact-generation',?3,?4,'{ \"provider\": \"soniox\", \"model\": \"old-model\", \"language\": \"ru-RU\" }',?5,?6,'2026-11-01T00:00:00Z')", params![id,meeting,"b".repeat(64),"c".repeat(64),phase,job]).unwrap();
}

fn rows(conn: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            (0..row.as_ref().column_count())
                .map(|column| row.get::<_, Value>(column))
                .collect()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

#[test]
fn v13_preserves_old_capture_receipts_captions_and_manual_empty_raw_with_foreign_keys_on() {
    let conn = v12_capture_fixture();
    let tables = [
        "service_worker_sessions",
        "service_capture_leases",
        "service_capture_captions",
        "service_jobs",
        "meetings",
        "segments",
        "settings",
        "service_transcript_sources",
        "service_transcript_overlays",
    ];
    let before: Vec<_> = tables.iter().map(|table| rows(&conn, table)).collect();
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    run(&conn).unwrap();
    run(&conn).unwrap();
    for (table, original) in tables.iter().zip(before) {
        assert_eq!(
            rows(&conn, table),
            original,
            "migration changed persisted rows in {table}"
        );
    }
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SUPPORTED_SCHEMA
    );
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let mut check = conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(check.query([]).unwrap().next().unwrap().is_none());
    assert_eq!(
        conn.query_row(
            "SELECT state FROM service_capture_leases WHERE id='old-capturing'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "capturing"
    );
    let (raw, clean, manual): (String, Option<String>, bool) = conn
        .query_row(
            "SELECT text,clean_text,clean_text_manual FROM segments WHERE id=42",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(raw, " Original raw текст \n");
    assert_eq!(clean.as_deref(), Some(""));
    assert!(manual);
}

#[test]
fn v13_singleton_index_forbids_mixed_reserved_and_capturing_not_just_each_phase() {
    let conn = v12_capture_fixture();
    run(&conn).unwrap();
    let attempt = |id: &str, phase: &str| {
        conn.execute(
        "INSERT INTO service_capture_leases(id,meeting_id,worker_session_id,library_epoch,generation,fence_hash,start_hash,provider_json,state,created_at) VALUES(?1,?1,'old-worker','old-epoch','generation','fence','start','{}',?2,'2026-11-01T00:00:00Z')",
        params![id,phase],
    )
    };
    assert!(attempt("reserved-alongside-old-capture", "reserved").is_err());
    assert!(attempt("second-old-capture", "capturing").is_err());
    conn.execute(
        "UPDATE service_capture_leases SET state='finalized' WHERE id='old-capturing'",
        [],
    )
    .unwrap();
    assert_eq!(attempt("one-reservation", "reserved").unwrap(), 1);
    assert!(attempt("capture-alongside-reservation", "capturing").is_err());
    assert!(attempt("second-reservation", "reserved").is_err());
    conn.execute(
        "UPDATE service_capture_leases SET state='capturing' WHERE id='one-reservation'",
        [],
    )
    .unwrap();
    assert!(attempt("reservation-alongside-capture", "reserved").is_err());
    assert_eq!(attempt("retained-aborted-receipt", "aborted").unwrap(), 1);
    assert_eq!(
        attempt("another-finalized-receipt", "finalized").unwrap(),
        1
    );
}

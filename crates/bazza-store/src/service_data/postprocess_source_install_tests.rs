use super::*;
use crate::service_data::{CaptureSelection, PostProcessSourceFence};
use bazza_domain::local_asr::LocalAsrCaption;
use rusqlite::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

const MODEL: &str = "whisper-actual-fallback";
const CAPTURE_RAW: &str = " original raw Орион ";
const LOCAL_RAW: &str = " localnewtext \0Бюджет🦀\n";

fn capture_fixture() -> (Connection, PostProcessJob) {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    crate::migrations::run(&conn).unwrap();
    let job = PostProcessJob {
        job_id: "source-job".into(),
        meeting_id: Uuid::new_v4(),
        revision: "job-revision".into(),
        stage: crate::service_data::PostProcessStage::Captured,
        input: PostProcessSourceFence {
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            checkpoint_revision: "checkpoint-revision".into(),
            lifecycle_revision: "lifecycle-revision".into(),
            transcript_revision: "capture-transcript".into(),
            source_id: Some("capture-source".into()),
        },
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: Some("selected-live-model".into()),
            language: Some("ru".into()),
        },
    };
    let id = job.meeting_id.to_string();
    let time = Utc::now();
    conn.execute(
        "INSERT INTO meetings(id,title,started_at,created_at,duration_ms,status)
         VALUES(?1,'Synthetic local capture',?2,?2,1000,'processing')",
        params![id, time],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO service_meeting_state VALUES(?1,'recorded','metadata-revision','lifecycle-revision',0)",
        [&id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,created_at,updated_at)
         VALUES(?1,'processing',?2,'gui_worker','running',?3,'fixture',?4,?4)",
        params![job.job_id, id, job.revision, time],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual)
         VALUES(101,?1,0,1000,'Capture label',?2,'',1)",
        params![id, CAPTURE_RAW],
    )
    .unwrap();
    let raw = json!([{
        "id":101,"start_ms":0,"end_ms":1000,"speaker":"Capture label","text":CAPTURE_RAW,
    }]);
    conn.execute(
        "INSERT INTO service_transcript_sources VALUES('capture-source',?1,'captured','local','ru',?2,?3)",
        params![id, raw.to_string(), time],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO service_transcript_state VALUES(?1,'capture-transcript','capture-source','present',0)",
        [&id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO service_transcript_overlays VALUES('capture-source','101','',1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meeting_speakers(meeting_id,label) VALUES(?1,'Capture label')",
        [&id],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO settings VALUES(?1,'{"provider":"local"}'),(?2,'{}'),(?3,'[1]'),('speaker_embeddings:policy:unrelated','[2]')"#,
        params![format!("stt:{id}"), format!("stt_previous:{id}"), format!("speaker_embeddings:policy:{id}")],
    )
    .unwrap();
    // The old raw text has an empty effective overlay, so seed an existing
    // retrieval observation to verify its vector is discarded with the chunk.
    conn.execute("INSERT INTO chunks(id,meeting_id,start_ms,end_ms,text) VALUES(55,?1,0,1000,'old cached text')", [&id]).unwrap();
    conn.execute("INSERT INTO chunk_vectors VALUES(55,1,X'00000000')", [])
        .unwrap();
    search::rebuild_meeting_on(&conn, &id).unwrap();
    (conn, job)
}

fn body() -> LocalAsrBody {
    LocalAsrBody {
        model: MODEL.into(),
        language: Some("ru-RU".into()),
        outcome: LocalAsrOutcome::Complete,
        captions: vec![LocalAsrCaption {
            start_ms: 17,
            end_ms: 17,
            text: LOCAL_RAW.into(),
        }],
    }
}

fn snapshot(conn: &Connection) -> String {
    conn.query_row(
        "SELECT json_array(
          (SELECT json_group_array(json_array(id,text,clean_text)) FROM segments),
          (SELECT json_group_array(json_array(key,value)) FROM settings ORDER BY key),
          (SELECT json_group_array(raw_json) FROM service_transcript_sources ORDER BY id),
          (SELECT json_array(revision,current_source_id) FROM service_transcript_state),
          (SELECT count(*) FROM service_post_process_local_sources),
          (SELECT count(*) FROM service_changes),
          (SELECT count(*) FROM meeting_speakers),
          (SELECT json_group_array(json_array(id,text)) FROM chunks),
          (SELECT count(*) FROM chunk_vectors),
          (SELECT json_group_array(json_array(id,text,revision)) FROM service_search_documents))",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn local_source_is_exact_and_archive_and_unrelated_caches_survive() {
    let (mut conn, job) = capture_fixture();
    let tx = conn.transaction().unwrap();
    let installed = install_on(&tx, &job, &body(), "fixture-worker").unwrap();
    tx.commit().unwrap();
    let id = job.meeting_id.to_string();
    let active: (i64, String, i64, i64) = conn
        .query_row("SELECT id,text,start_ms,end_ms FROM segments", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap();
    assert!(active.0 > 101);
    assert_eq!((active.1.as_str(), active.2, active.3), (LOCAL_RAW, 17, 17));
    let (origin, raw): (String, String) = conn
        .query_row(
            "SELECT origin,raw_json FROM service_transcript_sources WHERE id=?1",
            [&installed.source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, "local_asr");
    let raw: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        raw,
        json!([{"id":active.0,"start_ms":17,"end_ms":17,"speaker":null,"text":LOCAL_RAW}])
    );
    let previous: (String, String, bool) = conn
        .query_row(
            "SELECT s.raw_json,o.clean_text,o.manual FROM service_transcript_sources s
            JOIN service_transcript_overlays o ON o.source_id=s.id WHERE s.id='capture-source'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let previous_raw: Value = serde_json::from_str(&previous.0).unwrap();
    assert_eq!(previous_raw[0]["text"], CAPTURE_RAW);
    assert_eq!((previous.1.as_str(), previous.2), ("", true));
    let stt: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [format!("stt:{id}")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stt).unwrap(),
        json!({
            "provider":"local","model":MODEL,"language":"ru-RU","status":"complete","error":null,
            "captions":[{"start_ms":17,"end_ms":17,"text":LOCAL_RAW,"is_final":true}],
        })
    );
    let counts: (i64, i64, i64, i64, String) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM chunk_vectors),
          (SELECT count(*) FROM service_search_fts WHERE service_search_fts MATCH 'localnewtext'),
          (SELECT count(*) FROM settings WHERE key=?1),
          (SELECT count(*) FROM settings WHERE key='speaker_embeddings:policy:unrelated'),
          (SELECT value FROM settings WHERE key=?2)",
            params![
                format!("speaker_embeddings:policy:{id}"),
                format!("stt_previous:{id}")
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 2, 0, 1, "{}".into()));
    let lineage: (String, String, u32) = conn.query_row(
        "SELECT model,outcome,caption_count FROM service_post_process_local_sources WHERE source_id=?1 AND job_id=?2",
        params![installed.source_id, job.job_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(lineage, (MODEL.into(), "complete".into(), 1));
    assert_eq!(installed.caption_count, 1);
}

#[test]
fn late_index_failure_and_contradictory_stt_projection_preserve_the_old_state() {
    let (mut conn, job) = capture_fixture();
    conn.execute_batch(
        "CREATE TRIGGER reject_local_search BEFORE INSERT ON service_search_documents
         WHEN NEW.source_kind='transcript' BEGIN SELECT RAISE(ABORT,'fixture index failure'); END;",
    )
    .unwrap();
    let before = snapshot(&conn);
    {
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            install_on(&tx, &job, &body(), "fixture-worker"),
            Err(DataError::Sqlite(_))
        ));
    }
    assert_eq!(snapshot(&conn), before);
    conn.execute_batch("DROP TRIGGER reject_local_search")
        .unwrap();
    conn.execute(
        r#"UPDATE settings SET value='{"provider":"soniox"}' WHERE key=?1"#,
        [format!("stt:{}", job.meeting_id)],
    )
    .unwrap();
    let before = snapshot(&conn);
    {
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            install_on(&tx, &job, &body(), "fixture-worker"),
            Err(DataError::Forbidden)
        ));
    }
    assert_eq!(snapshot(&conn), before);
}

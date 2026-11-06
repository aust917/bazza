use super::*;
use crate::{
    service_data::{
        CaptureSelection, DesktopTranscriptRepresentation, DesktopTranscriptRequest,
        PostProcessSourceFence, TranscriptRead,
    },
    Store,
};
use bazza_domain::speaker_result::SpeakerAssignment;
use serde_json::json;
use uuid::Uuid;

const TIME: &str = "2026-11-03T00:00:00Z";
const RAW_TEXT: &str = " Raw\0 Орион 🦀\n";

fn fixture(zero_label: Option<&str>) -> (Store, PostProcessJob) {
    let store = Store::open_in_memory().unwrap();
    let job = PostProcessJob {
        job_id: "speaker-job".into(),
        meeting_id: Uuid::new_v4(),
        revision: "job-rev".into(),
        input: PostProcessSourceFence {
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            checkpoint_revision: "source-checkpoint".into(),
            lifecycle_revision: "life-rev".into(),
            transcript_revision: "source-rev".into(),
            source_id: Some("local-source".into()),
        },
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: None,
            language: None,
        },
        stage: PostProcessStage::LocalAsr {
            outcome: LocalAsrOutcome::Complete,
        },
    };
    let id = job.meeting_id.to_string();
    {
        let conn = store.conn.lock();
        conn.execute(
            "INSERT INTO meetings(id,title,started_at,created_at,duration_ms,status)
             VALUES(?1,'Synthetic diarization',?2,?2,3000,'processing')",
            params![id, TIME],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_meeting_state VALUES(?1,'recorded','meta-rev','life-rev',0)",
            [&id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,created_at,updated_at)
             VALUES(?1,'processing',?2,'gui_worker','running',?3,'fixture',?4,?4)",
            params![job.job_id, id, job.revision, TIME],
        )
        .unwrap();
        let mut raw = Vec::new();
        for (segment, start, end, label, text, clean) in [
            (101, 1000, 1700, None, RAW_TEXT, None),
            (102, 0, 1000, None, "manual text", Some("")),
            (103, 2000, 2000, zero_label, "zero", None),
            (104, 2100, 2500, None, "fourth", None),
        ] {
            conn.execute(
                "INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![segment, id, start, end, label, text, clean, clean.is_some()],
            )
            .unwrap();
            raw.push(
                json!({"id":segment,"start_ms":start,"end_ms":end,"speaker":label,"text":text}),
            );
        }
        conn.execute(
            "INSERT INTO service_transcript_sources VALUES('local-source',?1,'local_asr','local',NULL,?2,?3),
             ('archive-source',?1,'captured','local',NULL,?4,?3)",
            params![id, json!(raw).to_string(), TIME, json!([{"id":88,"start_ms":0,
                "end_ms":700,"speaker":"Original archive label","text":"archive raw"}]).to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_transcript_speaker_overlays VALUES('archive-source','88','Archive overlay')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_post_process_local_sources VALUES('local-source',?1,'actual-model','complete',4)",
            [&job.job_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_transcript_state VALUES(?1,'source-rev','local-source','present',0)",
            [&id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_transcript_overlays VALUES('local-source','102','',1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO speakers(id,name,embedding,dim,created_at) VALUES('voice','Saved identity',X'00000000',1,?1)",
            [TIME],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO voice_samples(id,speaker_id,embedding,dim,model_id,source,created_at)
             VALUES('sample','voice',X'00000000',1,'voice-model','legacy',?1)",
            [TIME],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meeting_speakers(meeting_id,label,suggested_id,suggested_score)
             VALUES(?1,'Stale suggestion','voice',0.8)",
            [&id],
        )
        .unwrap();
        conn.execute(
            r#"INSERT INTO settings VALUES(?1,'{"provider":"local","future":42}'),
             (?2,'[1]'),(?3,'[2]'),('speaker_embeddings:policy:other','[3]')"#,
            params![
                format!("stt:{id}"),
                format!("speaker_embeddings:{id}"),
                format!("speaker_embeddings:policy:{id}")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks(id,meeting_id,start_ms,end_ms,text) VALUES(44,?1,0,2600,'old retrieval text')",
            [&id],
        )
        .unwrap();
        conn.execute("INSERT INTO chunk_vectors VALUES(44,1,X'00000000')", [])
            .unwrap();
        search::rebuild_meeting_on(&conn, &id).unwrap();
    }
    (store, job)
}

fn applied() -> SpeakerResultBody {
    SpeakerResultBody::Applied {
        assignments: [
            (101, Some(900)),
            (102, Some(4)),
            (103, None),
            (104, Some(900)),
        ]
        .into_iter()
        .map(|(segment_id, cluster)| SpeakerAssignment {
            segment_id,
            cluster,
        })
        .collect(),
    }
}

fn install(
    store: &Store,
    job: &PostProcessJob,
    body: &SpeakerResultBody,
) -> DataResult<InstalledSpeakerResult> {
    let mut conn = store.conn.lock();
    let tx = conn.transaction()?;
    let result = install_on(&tx, job, body, "fixture-worker")?;
    tx.commit()?;
    Ok(result)
}

fn desktop(
    job: &PostProcessJob,
    revision: String,
    representation: DesktopTranscriptRepresentation,
) -> DesktopTranscriptRequest {
    DesktopTranscriptRequest {
        meeting_id: job.meeting_id.to_string(),
        source_id: None,
        expected_revision: revision,
        representation,
        cursor: None,
        limit: 100,
        max_bytes: 65024,
    }
}

fn snapshot(conn: &rusqlite::Connection) -> String {
    conn.query_row(
        "SELECT json_array(
         (SELECT json_group_array(json_array(id,speaker,text,clean_text,clean_text_manual)) FROM segments),
         (SELECT json_group_array(raw_json) FROM service_transcript_sources),
         (SELECT json_group_array(json_array(source_id,segment_key,label)) FROM service_transcript_speaker_overlays),
         (SELECT json_group_array(json_array(label,speaker_id,suggested_id,suggested_score)) FROM meeting_speakers),
         (SELECT json_group_array(json_array(key,value)) FROM settings),
         (SELECT revision FROM service_transcript_state),
         (SELECT generation FROM service_search_state),
         (SELECT count(*) FROM service_changes),
         (SELECT json_group_array(json_array(id,text)) FROM chunks),
         (SELECT count(*) FROM chunk_vectors))",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn anonymous_labels_use_asr_order_and_explicit_null_without_changing_text_or_archive() {
    let (store, job) = fixture(Some("Raw zero label"));
    let id = job.meeting_id.to_string();
    let mut old_page = desktop(
        &job,
        job.input.transcript_revision.clone(),
        DesktopTranscriptRepresentation::Raw,
    );
    old_page.limit = 1;
    old_page.cursor = store.service_desktop_transcript(&old_page).unwrap().next;
    assert!(old_page.cursor.is_some());
    let raw_before: String = store
        .conn
        .lock()
        .query_row(
            "SELECT raw_json FROM service_transcript_sources WHERE id='local-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let installed = install(&store, &job, &applied()).unwrap();
    assert_eq!(installed.changed_segments, 4);
    assert_ne!(installed.revision, job.input.transcript_revision);
    assert!(matches!(
        store.service_desktop_transcript(&old_page),
        Err(DataError::StaleCursor)
    ));
    let expected = [
        Some("Speaker 1"),
        Some("Speaker 2"),
        None,
        Some("Speaker 1"),
    ];
    for representation in [
        DesktopTranscriptRepresentation::Raw,
        DesktopTranscriptRepresentation::Effective,
    ] {
        let request = desktop(&job, installed.revision.clone(), representation);
        let page = store.service_desktop_transcript(&request).unwrap();
        let labels: Vec<_> = page
            .items
            .iter()
            .map(|item| item.speaker.as_deref())
            .collect();
        assert_eq!(labels, expected);
        assert_eq!(page.items[0].text, RAW_TEXT);
        assert_eq!(
            page.items[1].text,
            if representation == DesktopTranscriptRepresentation::Raw {
                "manual text"
            } else {
                ""
            }
        );
        assert!(page.items[1].clean_text_manual);
    }
    let page = store
        .service_read_transcript(&id, &TranscriptRead::default())
        .unwrap();
    let labels: Vec<_> = page
        .segments
        .iter()
        .map(|item| item.speaker.as_deref())
        .collect();
    assert_eq!(labels, expected);
    assert_eq!(page.segments[1].clean_text.as_deref(), Some(""));
    let archived = store
        .service_read_transcript(
            &id,
            &TranscriptRead {
                source_id: Some("archive-source".into()),
                ..TranscriptRead::default()
            },
        )
        .unwrap();
    assert_eq!(
        archived.segments[0].speaker.as_deref(),
        Some("Archive overlay")
    );
    assert_eq!(archived.segments[0].text, "archive raw");
    let mut archived = desktop(
        &job,
        installed.revision.clone(),
        DesktopTranscriptRepresentation::Raw,
    );
    archived.source_id = Some("archive-source".into());
    assert_eq!(
        store.service_desktop_transcript(&archived).unwrap().items[0]
            .speaker
            .as_deref(),
        Some("Archive overlay")
    );

    let conn = store.conn.lock();
    let raw_after: String = conn
        .query_row(
            "SELECT raw_json FROM service_transcript_sources WHERE id='local-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_before, raw_after);
    let explicit: Option<String> = conn.query_row(
        "SELECT label FROM service_transcript_speaker_overlays WHERE source_id='local-source' AND segment_key='103'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(explicit, None);
    let projections: (String, i64, i64, i64, String) = conn.query_row(
        "SELECT (SELECT group_concat(label) FROM (SELECT label FROM meeting_speakers
                       WHERE speaker_id IS NULL AND suggested_id IS NULL AND suggested_score IS NULL ORDER BY label)),
                (SELECT count(*) FROM chunk_vectors), (SELECT count(*) FROM voice_samples WHERE speaker_id='voice'),
                (SELECT count(*) FROM settings WHERE key LIKE 'speaker_embeddings:%'),
                (SELECT value FROM settings WHERE key=?1)",
        [format!("stt:{id}")], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).unwrap();
    assert_eq!(
        projections,
        (
            "Speaker 1,Speaker 2".into(),
            0,
            1,
            1,
            r#"{"provider":"local","future":42}"#.into()
        )
    );
    let chunks: String = conn
        .query_row(
            "SELECT text FROM chunks WHERE meeting_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(chunks.contains("Speaker 1: "));
    assert!(chunks.contains("Speaker 2: "));
}

#[test]
fn skipped_noop_keeps_revision_and_vectors_but_drops_stale_unconfirmed_data() {
    let (store, job) = fixture(None);
    let installed = install(
        &store,
        &job,
        &SpeakerResultBody::Skipped {
            reason: SpeakerSkipReason::ModelUnavailable,
        },
    )
    .unwrap();
    assert_eq!(installed.revision, job.input.transcript_revision);
    assert_eq!(installed.changed_segments, 0);
    let counts: (i64, i64, i64) = store
        .conn
        .lock()
        .query_row(
            "SELECT (SELECT count(*) FROM meeting_speakers), (SELECT count(*) FROM chunk_vectors),
                (SELECT count(*) FROM settings WHERE key LIKE 'speaker_embeddings:%')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 1, 1));
}

#[test]
fn invalid_coverage_confirmation_projection_and_late_search_failure_roll_back() {
    let (store, job) = fixture(None);
    let before = snapshot(&store.conn.lock());
    let SpeakerResultBody::Applied { assignments } = applied() else {
        unreachable!()
    };
    for changed in [
        assignments[1..].to_vec(),
        assignments.iter().cloned().rev().collect(),
        assignments
            .iter()
            .cloned()
            .map(|mut item| {
                if item.segment_id == 103 {
                    item.cluster = Some(99);
                }
                item
            })
            .collect(),
    ] {
        assert!(matches!(
            install(
                &store,
                &job,
                &SpeakerResultBody::Applied {
                    assignments: changed
                }
            ),
            Err(DataError::Invalid(_))
        ));
    }
    assert!(matches!(
        install(
            &store,
            &job,
            &SpeakerResultBody::Skipped {
                reason: SpeakerSkipReason::EmptyTranscript
            }
        ),
        Err(DataError::Invalid(_))
    ));
    assert_eq!(snapshot(&store.conn.lock()), before);
    store
        .conn
        .lock()
        .execute_batch(
            "CREATE TRIGGER reject_speaker_search BEFORE INSERT ON service_search_documents
         WHEN NEW.source_kind='transcript' BEGIN SELECT RAISE(ABORT,'fixture index failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        install(&store, &job, &applied()),
        Err(DataError::Sqlite(_))
    ));
    assert_eq!(snapshot(&store.conn.lock()), before);
    {
        let conn = store.conn.lock();
        conn.execute_batch(
            "DROP TRIGGER reject_speaker_search; UPDATE segments SET end_ms=2001 WHERE id=103;",
        )
        .unwrap();
    }
    assert!(matches!(
        install(&store, &job, &applied()),
        Err(DataError::Forbidden)
    ));
    store.conn.lock().execute_batch("UPDATE segments SET end_ms=2000 WHERE id=103; UPDATE meeting_speakers SET speaker_id='voice';").unwrap();
    let confirmed = snapshot(&store.conn.lock());
    assert!(matches!(
        install(&store, &job, &applied()),
        Err(DataError::Forbidden)
    ));
    assert_eq!(snapshot(&store.conn.lock()), confirmed);
}

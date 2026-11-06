use super::*;
use crate::Store;
use chrono::Utc;

fn ctx(key: &str) -> MutationContext {
    MutationContext {
        principal: "agent-a".into(),
        idempotency_key: key.into(),
    }
}
fn text() -> NewTextMeeting {
    NewTextMeeting {
        title: "Орион local".into(),
        started_at: Utc::now(),
        participants: vec!["Анна".into()],
        folder_id: None,
        tag_ids: vec![],
        calendar_uid: None,
        transcript: None,
    }
}
fn note() -> NewNote {
    NewNote {
        title: "Meeting memo".into(),
        markdown: "Decide astronomy".into(),
        meeting_id: None,
    }
}
fn query(q: &str) -> SearchRequest {
    SearchRequest {
        query: q.into(),
        limit: 20,
        ..Default::default()
    }
}

#[test]
fn text_meeting_reads_never_create_recordings_or_change_missing_recorded_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bazza.db");
    let store = Store::open(&path).unwrap();
    let created = store
        .service_create_text_meeting(&ctx("new"), &text())
        .unwrap();
    let m = store.service_meeting(&created.entity_id).unwrap().unwrap();
    assert_eq!(m.content_kind, "text");
    assert_eq!(m.media_state, "none");
    assert_eq!(m.meeting.status, crate::MeetingStatus::Ready);
    assert!(!tmp.path().join("recordings").exists());
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_meeting_state SET content_kind='recorded' WHERE meeting_id=?1",
            [&created.entity_id],
        )
        .unwrap();
    let m = store.service_meeting(&created.entity_id).unwrap().unwrap();
    assert_eq!(m.content_kind, "recorded");
    assert_eq!(m.media_state, "missing");
    assert!(!tmp.path().join("recordings").exists());
}

#[test]
fn lost_response_retry_survives_reopen_without_duplicate_or_content_in_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bazza.db");
    let input = note();
    let store = Store::open(&path).unwrap();
    let first = store.service_create_note(&ctx("retry"), &input).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let again = store.service_create_note(&ctx("retry"), &input).unwrap();
    assert_eq!(first, again);
    assert_eq!(
        store.service_notes(None, false, None, 100).unwrap().len(),
        1
    );
    assert_eq!(store.service_changes(0, 100).unwrap().len(), 1);
    let body: String = store
        .conn
        .lock()
        .query_row("SELECT outcome_json FROM service_idempotency", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!body.contains(&input.markdown));
    assert!(!body.contains(&input.title));
    let different = NewNote {
        markdown: "different".into(),
        ..input
    };
    assert!(matches!(
        store.service_create_note(&ctx("retry"), &different),
        Err(DataError::IdempotencyConflict)
    ));
}

#[test]
fn failed_transaction_rolls_back_content_revision_fts_change_and_dedup() {
    let store = Store::open_in_memory().unwrap();
    store.service_rebuild_search().unwrap();
    store.conn.lock().execute_batch("CREATE TRIGGER inject_failure BEFORE INSERT ON service_changes BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
    let generation = store.service_search_state().unwrap();
    assert!(store.service_create_note(&ctx("same"), &note()).is_err());
    assert!(store
        .service_notes(None, false, None, 100)
        .unwrap()
        .is_empty());
    assert!(store
        .service_search(&query("astronomy"))
        .unwrap()
        .hits
        .is_empty());
    assert_eq!(generation, store.service_search_state().unwrap());
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM service_idempotency", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER inject_failure;")
        .unwrap();
    assert!(store.service_create_note(&ctx("same"), &note()).is_ok());
}

#[test]
fn optimistic_writers_conflict_and_targeted_metadata_preserves_unrelated_state() {
    let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
    let input = text();
    let first = store
        .service_create_text_meeting(&ctx("new"), &input)
        .unwrap();
    let mut threads = vec![];
    for n in 0..2 {
        let store = store.clone();
        let id = first.entity_id.clone();
        let expected = first.revision.clone();
        threads.push(std::thread::spawn(move || {
            store.service_update_meeting(
                &ctx(&format!("writer{n}")),
                &id,
                &expected,
                &MeetingUpdate {
                    title: Some(format!("Title {n}")),
                    ..Default::default()
                },
            )
        }));
    }
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(DataError::Conflict { .. })))
            .count(),
        1
    );
    let m = store.service_meeting(&first.entity_id).unwrap().unwrap();
    assert_eq!(m.meeting.participants, input.participants);
    assert_eq!(m.meeting.status, crate::MeetingStatus::Ready);
    assert_eq!(
        m.lifecycle_revision,
        store
            .conn
            .lock()
            .query_row(
                "SELECT lifecycle_revision FROM service_meeting_state",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap()
    );
}

#[test]
fn dedup_scopes_principals_and_documents_retention_and_retry_before_revision_check() {
    let store = Store::open_in_memory().unwrap();
    let created = store.service_create_note(&ctx("same"), &note()).unwrap();
    let other = MutationContext {
        principal: "agent-b".into(),
        ..ctx("same")
    };
    let second = store.service_create_note(&other, &note()).unwrap();
    assert_ne!(created.entity_id, second.entity_id);
    let patch = NoteUpdate {
        markdown: Some("edited".into()),
        ..Default::default()
    };
    let first = store
        .service_update_note(
            &ctx("update"),
            &created.entity_id,
            &created.revision,
            &patch,
        )
        .unwrap();
    assert_eq!(
        first,
        store
            .service_update_note(
                &ctx("update"),
                &created.entity_id,
                &created.revision,
                &patch
            )
            .unwrap()
    );
    assert!(first.dedup_until > Utc::now() + chrono::Duration::days(6));
    store.conn.lock().execute("UPDATE service_idempotency SET expires_at='2000-01-01T00:00:00Z' WHERE principal='agent-a' AND tool='create_note'",[]).unwrap();
    let third = store.service_create_note(&ctx("same"), &note()).unwrap();
    assert_ne!(third.entity_id, created.entity_id);
}

#[test]
fn note_association_clear_is_explicit_and_bad_relation_rolls_back_title() {
    let store = Store::open_in_memory().unwrap();
    let meeting = store
        .service_create_text_meeting(&ctx("meeting"), &text())
        .unwrap();
    let n = store
        .service_create_note(
            &ctx("note"),
            &NewNote {
                meeting_id: Some(meeting.entity_id),
                ..note()
            },
        )
        .unwrap();
    assert!(matches!(
        store.service_update_note(
            &ctx("bad"),
            &n.entity_id,
            &n.revision,
            &NoteUpdate {
                title: Some("oops".into()),
                association: Some(AssociationPatch::Set("missing".into())),
                ..Default::default()
            }
        ),
        Err(DataError::NotFound)
    ));
    assert_eq!(
        store.service_note(&n.entity_id).unwrap().unwrap().title,
        "Meeting memo"
    );
    let changed = store
        .service_update_note(
            &ctx("clear"),
            &n.entity_id,
            &n.revision,
            &NoteUpdate {
                association: Some(AssociationPatch::Clear),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(store
        .service_note(&n.entity_id)
        .unwrap()
        .unwrap()
        .meeting_id
        .is_none());
    store
        .service_delete_note(&ctx("delete"), &n.entity_id, &changed.revision)
        .unwrap();
    assert!(store.service_note(&n.entity_id).unwrap().is_none());
}

#[test]
fn search_pins_generation_query_fields_and_default_effective_text() {
    let store = Store::open_in_memory().unwrap();
    let created = store
        .service_create_text_meeting(&ctx("meeting"), &text())
        .unwrap();
    let state = store.service_transcript_state(&created.entity_id).unwrap();
    store
        .service_import_transcript(
            &ctx("transcript"),
            &created.entity_id,
            &TranscriptWrite {
                expected_revision: state.revision,
                segments: vec![crate::NewSegment {
                    start_ms: 0,
                    end_ms: 100,
                    speaker: None,
                    text: "secretRawword".into(),
                }],
                source_label: None,
                language: None,
                origin: TranscriptImportOrigin::ExternalAgent,
            },
        )
        .unwrap();
    let state = store.service_transcript_state(&created.entity_id).unwrap();
    let segment = store.segments(&created.entity_id).unwrap()[0].id;
    store
        .service_set_transcript_corrections(
            &ctx("empty"),
            &created.entity_id,
            &TranscriptCorrections {
                expected_revision: state.revision,
                corrections: vec![TranscriptCorrection {
                    segment_id: segment,
                    text: Some(String::new()),
                }],
            },
        )
        .unwrap();
    assert!(matches!(
        store.service_search(&query("secretRawword")),
        Err(DataError::IndexUnavailable)
    ));
    store.service_rebuild_search().unwrap();
    assert!(store
        .service_search(&query("secretRawword"))
        .unwrap()
        .hits
        .is_empty());
    let raw = store
        .service_search(&SearchRequest {
            representation: SearchRepresentation::Raw,
            ..query("secretRawword")
        })
        .unwrap();
    assert_eq!(raw.hits.len(), 1);
    assert_eq!(raw.hits[0].source_kind, "transcript");
    for n in 0..3 {
        store
            .service_create_note(&ctx(&format!("note{n}")), &note())
            .unwrap();
    }
    let request = SearchRequest {
        limit: 1,
        ..query("astronomy")
    };
    let page = store.service_search(&request).unwrap();
    let cursor = page.next.unwrap();
    assert_eq!(
        store
            .service_search(&SearchRequest {
                cursor: Some(cursor.clone()),
                ..request.clone()
            })
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(matches!(
        store.service_search(&SearchRequest {
            query: "other".into(),
            cursor: Some(cursor.clone()),
            ..request.clone()
        }),
        Err(DataError::Invalid(_))
    ));
    store.service_create_note(&ctx("later"), &note()).unwrap();
    assert!(matches!(
        store.service_search(&SearchRequest {
            cursor: Some(cursor),
            ..request
        }),
        Err(DataError::StaleCursor)
    ));
}

#[test]
fn full_backfill_failure_preserves_ready_projection_and_retries() {
    let store = Store::open_in_memory().unwrap();
    store.service_create_note(&ctx("note"), &note()).unwrap();
    store.service_rebuild_search().unwrap();
    let before = store.service_search_state().unwrap();
    store.conn.lock().execute_batch("CREATE TRIGGER inject_index BEFORE INSERT ON service_search_documents BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
    assert!(store.service_rebuild_search().is_err());
    assert_eq!(store.service_search_state().unwrap(), before);
    assert_eq!(
        store
            .service_search(&query("astronomy"))
            .unwrap()
            .hits
            .len(),
        1
    );
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER inject_index;")
        .unwrap();
    store.service_rebuild_search().unwrap();
    assert_eq!(
        store
            .service_search(&query("astronomy"))
            .unwrap()
            .hits
            .len(),
        1
    );
}

#[test]
fn v10_migration_failure_rolls_back_entire_append_and_can_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bazza.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!(
        "../../../bazza-service-test-support/src/schema_v8.sql"
    ))
    .unwrap();
    conn.execute_batch("CREATE INDEX meetings_folder ON meetings(folder_id);CREATE INDEX meeting_tags_tag ON meeting_tags(tag_id);PRAGMA user_version=9;CREATE TABLE service_notes(sentinel TEXT);INSERT INTO service_notes VALUES('preserved');").unwrap();
    assert!(crate::migrations::run(&conn).is_err());
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        conn.query_row("SELECT sentinel FROM service_notes", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "preserved"
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='service_meeting_state'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    conn.execute("DROP TABLE service_notes", []).unwrap();
    crate::migrations::run(&conn).unwrap();
    crate::migrations::run(&conn).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::SUPPORTED_SCHEMA
    );
    let states: i64 = conn
        .query_row("SELECT COUNT(*) FROM service_meeting_state", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(states, 0);
}

#[test]
fn unified_search_covers_four_sources_and_deduplicates_identical_both_text() {
    let store = Store::open_in_memory().unwrap();
    let m = store
        .service_create_text_meeting(
            &ctx("meeting"),
            &NewTextMeeting {
                title: "nebulaTitle".into(),
                ..text()
            },
        )
        .unwrap();
    let state = store.service_transcript_state(&m.entity_id).unwrap();
    store
        .service_import_transcript(
            &ctx("transcript"),
            &m.entity_id,
            &TranscriptWrite {
                expected_revision: state.revision,
                segments: vec![crate::NewSegment {
                    start_ms: 0,
                    end_ms: 100,
                    speaker: None,
                    text: "nebulaTranscript".into(),
                }],
                source_label: None,
                language: None,
                origin: TranscriptImportOrigin::ExternalAgent,
            },
        )
        .unwrap();
    let state = store.service_summary_state(&m.entity_id).unwrap();
    store
        .service_write_summary(
            &ctx("summary"),
            &m.entity_id,
            &SummaryWrite {
                expected_revision: state.revision,
                mode: SummaryWriteMode::Create,
                markdown: "nebulaSummary".into(),
                structured_json: serde_json::json!({}),
                source_label: None,
                author: SummaryAuthor::ExternalAgent,
            },
        )
        .unwrap();
    store
        .service_create_note(
            &ctx("note"),
            &NewNote {
                markdown: "nebulaNote".into(),
                meeting_id: Some(m.entity_id.clone()),
                ..note()
            },
        )
        .unwrap();
    store.service_rebuild_search().unwrap();
    for (token, kind) in [
        ("nebulaTitle", "title"),
        ("nebulaTranscript", "transcript"),
        ("nebulaSummary", "summary"),
        ("nebulaNote", "note"),
    ] {
        let page = store.service_search(&query(token)).unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].source_kind, kind);
        if kind != "transcript" {
            assert!(page.hits[0].start_ms.is_none());
            assert!(page.hits[0].segment_id.is_none());
        }
    }
    let both = store
        .service_search(&SearchRequest {
            representation: SearchRepresentation::Both,
            ..query("nebulaTranscript")
        })
        .unwrap();
    assert_eq!(both.hits.len(), 1);
    assert_eq!(both.hits[0].representation, "both");
    assert!(store
        .service_search(&SearchRequest {
            fields: vec!["note".into()],
            ..query("nebulaSummary")
        })
        .unwrap()
        .hits
        .is_empty());
    assert!(store
        .service_search(&SearchRequest {
            meeting_ids: vec!["other".into()],
            ..query("nebulaTitle")
        })
        .unwrap()
        .hits
        .is_empty());
}

#[test]
fn list_metadata_and_search_excerpt_have_byte_budgets() {
    let store = Store::open_in_memory().unwrap();
    let huge = "я".repeat(100_000);
    store
        .service_create_note(
            &ctx("large"),
            &NewNote {
                title: "quasar".into(),
                markdown: huge,
                ..note()
            },
        )
        .unwrap();
    let json = serde_json::to_value(store.service_notes(None, false, None, 20).unwrap()).unwrap();
    assert!(json[0].get("markdown").is_none());
    store.service_rebuild_search().unwrap();
    let hits = store.service_search(&query("quasar")).unwrap().hits;
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.len() <= 2048);
    assert!(hits[0].snippet.is_char_boundary(hits[0].snippet.len()));
    let large = TranscriptWrite {
        expected_revision: String::new(),
        segments: vec![crate::NewSegment {
            start_ms: 0,
            end_ms: 1,
            speaker: None,
            text: "x".repeat(2_000_000),
        }],
        source_label: None,
        language: None,
        origin: TranscriptImportOrigin::ExternalAgent,
    };
    assert!(matches!(
        consistency::mutate(&store, &ctx("budget"), "test", &large, |_| panic!(
            "must reject before transaction"
        )),
        Err(DataError::Invalid("request budget"))
    ));
}

#[test]
fn search_cursor_cannot_cross_libraries_with_the_same_generation() {
    let a = Store::open_in_memory().unwrap();
    let b = Store::open_in_memory().unwrap();
    for store in [&a, &b] {
        for n in 0..2 {
            store
                .service_create_note(&ctx(&format!("note{n}")), &note())
                .unwrap();
        }
        store.service_rebuild_search().unwrap();
    }
    assert_eq!(
        a.service_search_state().unwrap(),
        b.service_search_state().unwrap()
    );
    let request = SearchRequest {
        limit: 1,
        ..query("astronomy")
    };
    let cursor = a.service_search(&request).unwrap().next.unwrap();
    assert!(matches!(
        b.service_search(&SearchRequest {
            cursor: Some(cursor),
            ..request
        }),
        Err(DataError::StaleCursor)
    ));
}

#[test]
fn creation_with_timed_transcript_is_atomic_and_invalid_intervals_create_nothing() {
    let store = Store::open_in_memory().unwrap();
    let segment = crate::NewSegment {
        start_ms: 0,
        end_ms: 1200,
        speaker: Some("Sam".into()),
        text: "createdTranscript".into(),
    };
    let input = NewTextMeeting {
        transcript: Some(vec![segment.clone()]),
        ..text()
    };
    store.conn.lock().execute_batch("CREATE TRIGGER fail_source BEFORE INSERT ON service_transcript_sources BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
    assert!(store
        .service_create_text_meeting(&ctx("create"), &input)
        .is_err());
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM meetings", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER fail_source;")
        .unwrap();
    let out = store
        .service_create_text_meeting(&ctx("create"), &input)
        .unwrap();
    assert_eq!(
        out,
        store
            .service_create_text_meeting(&ctx("create"), &input)
            .unwrap()
    );
    assert_eq!(
        store
            .service_meeting(&out.entity_id)
            .unwrap()
            .unwrap()
            .meeting
            .duration_ms,
        1200
    );
    assert_eq!(
        store
            .service_transcript_state(&out.entity_id)
            .unwrap()
            .document_state,
        DocumentState::Present
    );
    assert_eq!(
        store.segments(&out.entity_id).unwrap()[0].text,
        "createdTranscript"
    );
    let bad = NewTextMeeting {
        transcript: Some(vec![crate::NewSegment {
            end_ms: 0,
            ..segment
        }]),
        ..text()
    };
    assert!(matches!(
        store.service_create_text_meeting(&ctx("invalid"), &bad),
        Err(DataError::Invalid("transcript timing"))
    ));
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM meetings", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn atomic_large_v8_backfill_is_complete_and_reports_elapsed_time() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("bazza.db");
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!(
        "../../../bazza-service-test-support/src/schema_v8.sql"
    ))
    .unwrap();
    let tx = conn.transaction().unwrap();
    for meeting in 0..500 {
        let id = format!("synthetic-{meeting}");
        tx.execute("INSERT INTO meetings(id,title,started_at,status,created_at) VALUES(?1,'largeFixtureTitle','2026-11-02T00:00:00Z','ready','2026-11-02T00:00:00Z')",[&id]).unwrap();
        tx.execute("INSERT INTO summaries VALUES(?1,'general','largeFixtureSummary','{}','synthetic','2026-11-02T00:00:00Z')",[&id]).unwrap();
        for segment in 0..20 {
            tx.execute("INSERT INTO segments(meeting_id,start_ms,end_ms,text,clean_text,clean_text_manual) VALUES(?1,?2,?3,'largeFixtureRaw',?4,?5)",rusqlite::params![id,segment*1000,(segment+1)*1000,if segment%2==0{Some("largeFixtureEffective")}else{None},segment%2==0]).unwrap();
        }
    }
    tx.commit().unwrap();
    drop(conn);
    let begin = std::time::Instant::now();
    let store = Store::open(&path).unwrap();
    let migration = begin.elapsed();
    let begin = std::time::Instant::now();
    store.service_rebuild_search().unwrap();
    let backfill = begin.elapsed();
    let conn = store.conn.lock();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM service_transcript_sources", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        500
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM service_search_documents", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        21_000
    );
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM service_search_fts WHERE service_search_fts MATCH 'largeFixtureSummary'",[],|r|r.get::<_,i64>(0)).unwrap(),500);
    eprintln!("large synthetic v8:500meetings/10000segments/500summaries/21000searchdocs migration_ms={} backfill_ms={}",migration.as_millis(),backfill.as_millis());
}

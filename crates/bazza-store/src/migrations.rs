use rusqlite::Connection;

#[cfg(test)]
#[path = "service_data/capture_start_migration_tests.rs"]
mod capture_start_tests;

const MIGRATIONS: &[&str] = &[
    // v1
    r#"
CREATE TABLE meetings(
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  app TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  duration_ms INTEGER NOT NULL DEFAULT 0,
  has_video INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL,
  folder_id TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE segments(
  id INTEGER PRIMARY KEY,
  meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  speaker TEXT,
  text TEXT NOT NULL,
  clean_text TEXT
);
CREATE INDEX segments_meeting ON segments(meeting_id, start_ms);
CREATE VIRTUAL TABLE segments_fts USING fts5(text, clean_text, content='segments', content_rowid='id');
CREATE TRIGGER segments_ai AFTER INSERT ON segments BEGIN
  INSERT INTO segments_fts(rowid, text, clean_text) VALUES (new.id, new.text, new.clean_text);
END;
CREATE TRIGGER segments_ad AFTER DELETE ON segments BEGIN
  INSERT INTO segments_fts(segments_fts, rowid, text, clean_text) VALUES ('delete', old.id, old.text, old.clean_text);
END;
CREATE TRIGGER segments_au AFTER UPDATE ON segments BEGIN
  INSERT INTO segments_fts(segments_fts, rowid, text, clean_text) VALUES ('delete', old.id, old.text, old.clean_text);
  INSERT INTO segments_fts(rowid, text, clean_text) VALUES (new.id, new.text, new.clean_text);
END;
CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#,
    // v2: speakers
    r#"
CREATE TABLE speakers(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  embedding BLOB NOT NULL,
  dim INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX speakers_name ON speakers(name COLLATE NOCASE);
CREATE TABLE meeting_speakers(
  meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  speaker_id TEXT,
  suggested_id TEXT,
  suggested_score REAL,
  PRIMARY KEY(meeting_id, label)
);
"#,
    // v3: templates, summaries, chats, retrieval chunks
    r#"
CREATE TABLE templates(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', body TEXT NOT NULL,
  builtin INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL
);
CREATE TABLE summaries(
  meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
  template_id TEXT NOT NULL, markdown TEXT NOT NULL, structured_json TEXT NOT NULL, model TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE chats(
  id TEXT PRIMARY KEY, scope_kind TEXT NOT NULL, scope_id TEXT, title TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE INDEX chats_scope ON chats(scope_kind, scope_id);
CREATE TABLE chat_messages(
  id TEXT PRIMARY KEY, chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  role TEXT NOT NULL, content TEXT NOT NULL, citations_json TEXT NOT NULL DEFAULT '[]', created_at TEXT NOT NULL
);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id, created_at);
CREATE TABLE chunks(
  id INTEGER PRIMARY KEY, meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL, text TEXT NOT NULL
);
CREATE INDEX chunks_meeting ON chunks(meeting_id, start_ms);
CREATE VIRTUAL TABLE chunks_fts USING fts5(text, content='chunks', content_rowid='id');
CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text); END;
CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.id, old.text); END;
"#,
    // v4: folders, tags, shares
    r#"
CREATE TABLE folders(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, parent_id TEXT, default_template_id TEXT, created_at TEXT NOT NULL
);
CREATE TABLE tags(id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE UNIQUE INDEX tags_name ON tags(name COLLATE NOCASE);
CREATE TABLE meeting_tags(
  meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
  PRIMARY KEY(meeting_id, tag_id)
);
CREATE TABLE shares(
  token TEXT PRIMARY KEY, meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  kind TEXT NOT NULL, start_ms INTEGER, end_ms INTEGER, enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL
);
"#,
    // v5: processing errors, auto-titles, attendees, calendar link, chunk embeddings
    r#"
ALTER TABLE meetings ADD COLUMN error TEXT;
ALTER TABLE meetings ADD COLUMN title_auto INTEGER NOT NULL DEFAULT 1;
ALTER TABLE meetings ADD COLUMN participants_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE meetings ADD COLUMN calendar_uid TEXT;
CREATE TABLE chunk_vectors(
  chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
  dim INTEGER NOT NULL, vec BLOB NOT NULL
);
"#,
    // v6: managed voice samples, with an explicit embedding model identity.
    r#"
ALTER TABLE speakers ADD COLUMN embedding_model TEXT NOT NULL DEFAULT 'titanet-small:sha256:ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e';
CREATE TABLE voice_samples(
  id TEXT PRIMARY KEY,
  speaker_id TEXT NOT NULL REFERENCES speakers(id) ON DELETE CASCADE,
  embedding BLOB NOT NULL,
  dim INTEGER NOT NULL,
  model_id TEXT NOT NULL,
  source TEXT NOT NULL CHECK(source IN ('legacy', 'meeting', 'file')),
  source_label TEXT NOT NULL DEFAULT '',
  meeting_id TEXT,
  start_ms INTEGER,
  end_ms INTEGER,
  has_audio INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX voice_samples_speaker ON voice_samples(speaker_id, created_at);
INSERT INTO voice_samples(id, speaker_id, embedding, dim, model_id, source, source_label, created_at)
  SELECT 'legacy-' || id, id, embedding, dim, embedding_model, 'legacy', 'Previously saved voiceprint', created_at
  FROM speakers WHERE dim > 0;
"#,
    // v7: prior nearest-centroid suggestions were not verified against independent
    // samples. Forget only pending guesses; confirmed identities and text remain.
    r#"
UPDATE meeting_speakers SET suggested_id = NULL, suggested_score = NULL WHERE speaker_id IS NULL;
"#,
    // v8: user corrections are a protected cleaned-text overlay, never raw text.
    r#"
ALTER TABLE segments ADD COLUMN clean_text_manual INTEGER NOT NULL DEFAULT 0
  CHECK(clean_text_manual IN (0, 1) AND (clean_text_manual = 0 OR clean_text IS NOT NULL));
"#,
    // v9: sidebar counts look up meetings by folder and memberships by tag.
    r#"
CREATE INDEX meetings_folder ON meetings(folder_id);
CREATE INDEX meeting_tags_tag ON meeting_tags(tag_id);
"#,
    // v10: isolated service domain data; legacy capture writers remain unchanged.
    include_str!("service_data/schema.sql"),
    // v11: registered native workers and durable capture handoff fencing.
    include_str!("service_data/workers.sql"),
    // v12: desktop preference revisions; capture/STT settings are untouched.
    include_str!("service_data/desktop.sql"),
    // v13: mandatory begin and retained, non-destructive reservation abort.
    include_str!("service_data/capture_start.sql"),
    // v14: durable independent folder/tag metadata revisions.
    include_str!("service_data/library_state.sql"),
    // v15: closed template uploads and registered post-process ownership.
    concat!(
        include_str!("service_data/templates.sql"),
        "\n",
        include_str!("service_data/worker_jobs.sql")
    ),
    // v16: explicit post-process input checkpoints; cleanup staging follows.
    concat!(
        include_str!("service_data/postprocess_checkpoint.sql"),
        "\n",
        include_str!("service_data/postprocess_cleanup.sql")
    ),
    // v17: private bounded chat reads with durable passive mutation fences.
    include_str!("service_data/chats.sql"),
    // v18: closed local ASR publication, with immutable capture lineage.
    include_str!("service_data/postprocess_source.sql"),
    // v19: source-owned speaker overlays and a separate Ready checkpoint.
    include_str!("service_data/postprocess_speakers.sql"),
    // v20: bounded generated summary publication and fixture job completion.
    concat!(
        include_str!("service_data/postprocess_summary.sql"),
        "\n",
        include_str!("service_data/postprocess_complete.sql")
    ),
    // v21: persistent whole-library read grants for the external MCP role.
    include_str!("service_data/external_read_grants.sql"),
    // v22: independent evidence for captured raw screen and Soniox terminal status.
    concat!(
        include_str!("service_data/capture_terminal_screen.sql"),
        "\n",
        include_str!("service_data/capture_terminal_stt.sql")
    ),
    // v23: Soniox permission epochs and one-shot capture grants.
    include_str!("service_data/soniox_permission.sql"),
    // v24: recovery of a synced post-process video across file/database commit.
    include_str!("service_data/postprocess_video.sql"),
    // v25: fence speaker results to either a local ASR or captured Soniox source.
    include_str!("service_data/postprocess_speaker_sources.sql"),
];

/// Highest schema understood by this build. Never infer support from a library's
/// user_version or rewrite a previously released migration.
pub const SUPPORTED_SCHEMA: i64 = MIGRATIONS.len() as i64;
/// Last schema required by the ordinary, unenrolled native GUI/recorder.
/// Service migrations require a separate explicit ownership/adoption boundary.
pub(crate) const LEGACY_SCHEMA: i64 = 9;

pub(crate) fn ensure_supported(current: i64) -> crate::Result<()> {
    if current > SUPPORTED_SCHEMA {
        return Err(crate::StoreError::FutureSchema {
            found: current,
            supported: SUPPORTED_SCHEMA,
        });
    }
    Ok(())
}

pub fn run(conn: &Connection) -> crate::Result<()> {
    run_until(conn, SUPPORTED_SCHEMA)
}

pub(crate) fn run_until(conn: &Connection, target: i64) -> crate::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure_supported(current)?;
    if !(1..=SUPPORTED_SCHEMA).contains(&target) {
        return Err(crate::StoreError::Other(
            "invalid private migration target".into(),
        ));
    }
    // A supported newer database is never downgraded or rewritten merely
    // because this caller only needs the legacy subset.
    for (i, sql) in MIGRATIONS.iter().take(target as usize).enumerate() {
        let v = (i + 1) as i64;
        if v > current {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            if v == 16 {
                crate::service_data::postprocess_checkpoint::migrate_on(&tx).map_err(|error| {
                    match error {
                        crate::service_data::DataError::Sqlite(error) => {
                            crate::StoreError::Sqlite(error)
                        }
                        crate::service_data::DataError::Json(error) => {
                            crate::StoreError::Json(error)
                        }
                        crate::service_data::DataError::Io(error) => crate::StoreError::Io(error),
                        _ => crate::StoreError::Other(
                            "post-process checkpoint migration failed".into(),
                        ),
                    }
                })?;
            }
            tx.pragma_update(None, "user_version", v)?;
            tx.commit()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_source_migration_keeps_outcomes_immutable_and_cascades_on_source_erasure() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_until(&conn, 24).unwrap();
        conn.execute_batch(
            r#"INSERT INTO meetings(id,title,started_at,created_at,duration_ms,status)
                VALUES('meeting','Fixture','2026-11-03T00:00:00Z','2026-11-03T00:00:00Z',0,'ready');
             INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,created_at,updated_at)
                VALUES('job','processing','meeting','gui_worker','waiting_for_worker','job-rev',
                'fixture','2026-11-03T00:00:00Z','2026-11-03T00:00:00Z');
             INSERT INTO service_post_process_anchors VALUES('job','meeting','fingerprint','life','old-transcript',NULL,'absent');
             INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,raw_json,created_at)
                VALUES('source','meeting','local_asr','local','[]','2026-11-03T00:00:00Z');
             INSERT INTO service_post_process_local_sources VALUES('source','job','fixture-model','complete',0);
             INSERT INTO service_post_process_speaker_results VALUES('job','{"kind":"applied"}');"#
        ).unwrap();
        run(&conn).unwrap();
        let result: (String, String) = conn.query_row(
            "SELECT source_id,outcome_json FROM service_post_process_speaker_results WHERE job_id='job'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(result, ("source".into(), r#"{"kind":"applied"}"#.into()));
        assert!(conn
            .execute(
                "UPDATE service_post_process_speaker_results SET outcome_json=outcome_json",
                [],
            )
            .is_err());
        conn.execute(
            "DELETE FROM service_transcript_sources WHERE id='source'",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM service_post_process_speaker_results",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn voice_samples_upgrade_preserves_existing_voice_memory() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        let voice: Vec<u8> = [0.6f32, 0.8].iter().flat_map(|v| v.to_le_bytes()).collect();
        conn.execute("INSERT INTO speakers(id, name, embedding, dim, created_at) VALUES ('old', 'Ann', ?1, 2, '2026-01-01')", [voice.clone()]).unwrap();
        run(&conn).unwrap();
        run(&conn).unwrap();
        let saved: (Vec<u8>, String) = conn
            .query_row(
                "SELECT embedding, embedding_model FROM speakers WHERE id = 'old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(saved.0, voice);
        assert_eq!(saved.1, crate::CURRENT_VOICE_MODEL);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM voice_samples WHERE speaker_id = 'old' AND source = 'legacy'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn manual_corrections_upgrade_preserves_v7_text_profiles_samples_and_ids() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute("INSERT INTO meetings(id,title,started_at,status,created_at) VALUES ('meeting','Original','2026-01-01','ready','2026-01-01')", []).unwrap();
        conn.execute("INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text) VALUES (42,'meeting',100,2000,'Ann','original raw','existing AI cleanup')", []).unwrap();
        let embedding: Vec<u8> = [0.6f32, 0.8]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        conn.execute("INSERT INTO speakers(id,name,embedding,dim,created_at) VALUES ('known','Ann',?1,2,'2026-01-01')", [&embedding]).unwrap();
        conn.execute("INSERT INTO voice_samples(id,speaker_id,embedding,dim,model_id,source,source_label,created_at) VALUES ('sample','known',?1,2,?2,'file','fixture.wav','2026-01-01')", rusqlite::params![embedding,crate::CURRENT_VOICE_MODEL]).unwrap();
        run(&conn).unwrap();
        run(&conn).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MIGRATIONS.len() as i64
        );
        let saved: (i64, String, String, bool) = conn
            .query_row(
                "SELECT id,text,clean_text,clean_text_manual FROM segments",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            saved,
            (
                42,
                "original raw".into(),
                "existing AI cleanup".into(),
                false
            )
        );
        let speaker: (String, String, Vec<u8>) = conn
            .query_row("SELECT id,name,embedding FROM speakers", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(speaker, ("known".into(), "Ann".into(), embedding.clone()));
        let sample: (String, String, Vec<u8>) = conn
            .query_row(
                "SELECT id,speaker_id,embedding FROM voice_samples",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(sample, ("sample".into(), "known".into(), embedding));
        assert!(conn
            .execute(
                "UPDATE segments SET clean_text=NULL,clean_text_manual=1",
                []
            )
            .is_err());
    }

    #[test]
    fn verified_matching_upgrade_clears_only_pending_suggestions() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute("INSERT INTO meetings(id, title, started_at, status, created_at) VALUES ('meeting', 'Original meeting', '2026-01-01', 'ready', '2026-01-01')", []).unwrap();
        let voice: Vec<u8> = [0.6f32, 0.8]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        conn.execute("INSERT INTO speakers(id, name, embedding, dim, created_at) VALUES ('known', 'Ann', ?1, 2, '2026-01-01')", [voice.clone()]).unwrap();
        conn.execute("INSERT INTO meeting_speakers(meeting_id, label, suggested_id, suggested_score) VALUES ('meeting', 'Speaker 1', 'known', 0.9)", []).unwrap();
        conn.execute("INSERT INTO meeting_speakers(meeting_id, label, speaker_id, suggested_id, suggested_score) VALUES ('meeting', 'Ann', 'known', 'known', 0.95)", []).unwrap();
        conn.execute("INSERT INTO segments(meeting_id, start_ms, end_ms, speaker, text) VALUES ('meeting', 0, 2000, 'Ann', 'original words')", []).unwrap();
        run(&conn).unwrap();
        run(&conn).unwrap();
        let pending: (String, Option<String>, Option<String>, Option<f64>) = conn.query_row("SELECT label, speaker_id, suggested_id, suggested_score FROM meeting_speakers WHERE label = 'Speaker 1'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
        assert_eq!(pending, ("Speaker 1".into(), None, None, None));
        let confirmed: (String, String, String, f64) = conn.query_row("SELECT label, speaker_id, suggested_id, suggested_score FROM meeting_speakers WHERE label = 'Ann'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
        assert_eq!(
            confirmed,
            ("Ann".into(), "known".into(), "known".into(), 0.95)
        );
        assert_eq!(
            conn.query_row("SELECT speaker, text FROM segments", [], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            )))
            .unwrap(),
            ("Ann".into(), "original words".into())
        );
        assert_eq!(
            conn.query_row(
                "SELECT embedding FROM speakers WHERE id = 'known'",
                [],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
            voice
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MIGRATIONS.len() as i64
        );
    }

    #[test]
    fn sidebar_indexes_upgrade_v8_without_changing_library_data() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..8] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 8).unwrap();
        conn.execute(
            "INSERT INTO folders(id,name,created_at) VALUES ('folder','Projects','2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO tags(id,name) VALUES ('tag','Important')", [])
            .unwrap();
        conn.execute("INSERT INTO meetings(id,title,started_at,status,folder_id,created_at) VALUES ('meeting','Original','2026-01-01','ready','folder','2026-01-01')", []).unwrap();
        conn.execute(
            "INSERT INTO meeting_tags(meeting_id,tag_id) VALUES ('meeting','tag')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO segments(id,meeting_id,start_ms,end_ms,text,clean_text,clean_text_manual) VALUES (42,'meeting',0,1000,'raw words','manual correction',1)", []).unwrap();
        run(&conn).unwrap();
        run(&conn).unwrap();

        for (name, column) in [
            ("meetings_folder", "folder_id"),
            ("meeting_tags_tag", "tag_id"),
        ] {
            let columns: Vec<String> = conn
                .prepare(&format!("PRAGMA index_info({name})"))
                .unwrap()
                .query_map([], |row| row.get(2))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(columns, [column]);
        }
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM meetings WHERE folder_id='folder'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM meeting_tags WHERE tag_id='tag'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let saved: (i64, String, String, bool) = conn
            .query_row(
                "SELECT id,text,clean_text,clean_text_manual FROM segments",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            saved,
            (42, "raw words".into(), "manual correction".into(), true)
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MIGRATIONS.len() as i64
        );
    }

    #[test]
    fn owned_v8_upgrade_is_explicit_and_preserves_raw_manual_and_empty_overlays() {
        use crate::{database_path, LibraryOwnerGuard, MigrationPolicy, Store, StoreError};
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let path = database_path(root);
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..8] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 8).unwrap();
        conn.execute("INSERT INTO meetings(id,title,started_at,status,created_at) VALUES ('meeting','Original','2026-01-01','ready','2026-01-01')", []).unwrap();
        conn.execute("INSERT INTO segments(id,meeting_id,start_ms,end_ms,text,clean_text,clean_text_manual) VALUES (42,'meeting',0,1000,'raw words','manual correction',1),(43,'meeting',1000,2000,'hidden raw words','',1)", []).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
        assert!(matches!(
            Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent),
            Err(StoreError::MigrationRequired {
                found: 8,
                supported: SUPPORTED_SCHEMA
            })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!root.join("bazza.db-wal").exists());
        assert!(!root.join("bazza.db-shm").exists());
        let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
        let store = Store::open_existing_owned(guard, MigrationPolicy::Upgrade).unwrap();
        let segments = store.segments("meeting").unwrap();
        assert_eq!(segments[0].id, 42);
        assert_eq!(segments[0].text, "raw words");
        assert_eq!(segments[0].clean_text.as_deref(), Some("manual correction"));
        assert!(segments[0].clean_text_manual);
        assert_eq!(segments[1].id, 43);
        assert_eq!(segments[1].text, "hidden raw words");
        assert_eq!(segments[1].clean_text.as_deref(), Some(""));
        assert!(segments[1].clean_text_manual);
        drop(store);
        assert_eq!(crate::schema_version(&path).unwrap(), SUPPORTED_SCHEMA);
        let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
        let reopened = Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent).unwrap();
        assert_eq!(reopened.segments("meeting").unwrap(), segments);
    }

    #[test]
    fn direct_migration_runner_rejects_future_schema_without_schema_changes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('untouched');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", SUPPORTED_SCHEMA + 1)
            .unwrap();
        assert!(matches!(
            run(&conn),
            Err(crate::StoreError::FutureSchema { .. })
        ));
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            SUPPORTED_SCHEMA + 1
        );
        assert_eq!(
            conn.query_row("SELECT value FROM sentinel", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "untouched"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

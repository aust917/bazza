use rusqlite::Connection;

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
];

pub fn run(conn: &Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let v = (i + 1) as i64;
        if v > current {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
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
    fn voice_samples_upgrade_preserves_existing_voice_memory() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..5] { conn.execute_batch(sql).unwrap(); }
        conn.pragma_update(None, "user_version", 5).unwrap();
        let voice: Vec<u8> = [0.6f32, 0.8].iter().flat_map(|v| v.to_le_bytes()).collect();
        conn.execute("INSERT INTO speakers(id, name, embedding, dim, created_at) VALUES ('old', 'Ann', ?1, 2, '2026-01-01')", [voice.clone()]).unwrap();
        run(&conn).unwrap();
        run(&conn).unwrap();
        let saved: (Vec<u8>, String) = conn.query_row("SELECT embedding, embedding_model FROM speakers WHERE id = 'old'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(saved.0, voice);
        assert_eq!(saved.1, crate::CURRENT_VOICE_MODEL);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM voice_samples WHERE speaker_id = 'old' AND source = 'legacy'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
}

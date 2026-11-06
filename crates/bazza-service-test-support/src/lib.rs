//! Disposable synthetic libraries for the MCP service migration boundary.
//!
//! This is test support, not a library opener. Callers supply a new directory;
//! neither the user's library nor Bazza's data-directory pointer is discovered.

use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

pub const MARKER_FILE: &str = ".bazza-synthetic-fixture";
pub const MARKER_CONTENT: &str = "bazza-mcp-v8-fixture-v1\n";
pub const READY_MEETING_ID: &str = "00000000-0000-4000-8000-000000000001";
pub const DAMAGED_MEETING_ID: &str = "00000000-0000-4000-8000-000000000002";
pub const ORPHAN_MEETING_ID: &str = "00000000-0000-4000-8000-000000000003";
pub const CORRUPT_MEETING_ID: &str = "00000000-0000-4000-8000-000000000004";
pub const SPEAKER_ID: &str = "00000000-0000-4000-8000-000000000010";
pub const VOICE_SAMPLE_ID: &str = "00000000-0000-4000-8000-000000000011";
pub const MEETING_SHARE_TOKEN: &str = "00000000000040008000000000000001";
pub const CLIP_SHARE_TOKEN: &str = "00000000000040008000000000000002";
pub const DISABLED_SHARE_TOKEN: &str = "00000000000040008000000000000003";

const TIMESTAMP: &str = "2026-11-02T00:00:00Z";
const VOICE_MODEL: &str =
    "titanet-small:sha256:ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e";

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("synthetic fixture I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("synthetic fixture SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug)]
pub struct FixtureInfo {
    pub root: PathBuf,
    pub database_path: PathBuf,
    pub ready_meeting_id: &'static str,
    pub damaged_meeting_id: &'static str,
}

/// Create a deterministic v8 library in a directory that does not exist yet.
///
/// Existing directories, even empty ones, are refused before any writes. The
/// parent must already exist. A partial failure leaves the new directory for
/// inspection and never recursively removes anything.
pub fn create_v8(root: &Path) -> Result<FixtureInfo, FixtureError> {
    std::fs::create_dir(root)?;
    let database_path = root.join("bazza.db");
    let mut connection = Connection::open(&database_path)?;
    connection.execute_batch(include_str!("schema_v8.sql"))?;
    let transaction = connection.transaction()?;

    transaction.execute(
        "INSERT INTO folders(id,name,created_at) VALUES ('fixture-folder','Проекты / Projects',?1)",
        [TIMESTAMP],
    )?;
    transaction.execute(
        "INSERT INTO tags(id,name) VALUES ('fixture-tag','MCP baseline')",
        [],
    )?;
    for (id, title, status, duration, error) in [
        (
            READY_MEETING_ID,
            "Орион / Budget review",
            "ready",
            4000,
            None,
        ),
        (
            DAMAGED_MEETING_ID,
            "Повреждённая запись / Missing media",
            "failed",
            4000,
            Some("synthetic missing mix.wav"),
        ),
        (
            ORPHAN_MEETING_ID,
            "Прерванная запись / Recovery candidate",
            "recording",
            0,
            None,
        ),
        (
            CORRUPT_MEETING_ID,
            "Повреждённый WAV / Corrupt media",
            "failed",
            4000,
            Some("synthetic invalid WAV"),
        ),
    ] {
        transaction.execute(
            "INSERT INTO meetings(id,title,app,started_at,ended_at,duration_ms,status,folder_id,created_at,error,title_auto,participants_json,calendar_uid) VALUES (?1,?2,'Synthetic fixture',?3,?4,?5,?6,'fixture-folder',?3,?7,0,'[\"Анна\",\"Alex\"]','fixture-calendar-uid')",
            params![id, title, TIMESTAMP, (status != "recording").then_some("2026-11-02T00:00:04Z"), duration, status, error],
        )?;
        transaction.execute(
            "INSERT INTO meeting_tags(meeting_id,tag_id) VALUES (?1,'fixture-tag')",
            [id],
        )?;
    }
    for (id, start, end, raw, clean, manual) in [
        (101, 0, 1000, "ээ проект Орион", None, false),
        (
            102,
            1000,
            2000,
            "budget maybe fourteen",
            Some("Бюджет проекта — 14."),
            true,
        ),
        (103, 2000, 3000, "скрытая вручную реплика", Some(""), true),
        (
            104,
            3000,
            4000,
            "uh next week launch",
            Some("Launch next week."),
            false,
        ),
    ] {
        transaction.execute(
            "INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual) VALUES (?1,?2,?3,?4,'Speaker 1',?5,?6,?7)",
            params![id, READY_MEETING_ID, start, end, raw, clean, manual],
        )?;
    }
    transaction.execute(
        "INSERT INTO segments(id,meeting_id,start_ms,end_ms,text) VALUES (201,?1,0,1000,'confirmed raw despite missing audio')",
        [DAMAGED_MEETING_ID],
    )?;
    transaction.execute(
        "INSERT INTO chunks(id,meeting_id,start_ms,end_ms,text) VALUES (301,?1,0,4000,'ээ проект Орион Бюджет проекта — 14. Launch next week.')",
        [READY_MEETING_ID],
    )?;
    let embedding: Vec<u8> = [0.6_f32, 0.8]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    transaction.execute(
        "INSERT INTO chunk_vectors(chunk_id,dim,vec) VALUES (301,2,?1)",
        [&embedding],
    )?;
    transaction.execute(
        "INSERT INTO templates(id,name,description,body,builtin,created_at) VALUES ('fixture-template','Fixture summary','Synthetic only','# Summary',0,?1)",
        [TIMESTAMP],
    )?;
    for (meeting, markdown, model, structured) in [
        (
            READY_MEETING_ID,
            "# Итоги\n\nБюджет: 14. Launch next week.",
            "synthetic-manual",
            "{\"fixture_origin\":\"manual\",\"budget\":14}",
        ),
        (
            DAMAGED_MEETING_ID,
            "# Imported agent summary\n\nMedia is missing.",
            "synthetic-external-agent",
            "{\"fixture_origin\":\"external_agent\"}",
        ),
    ] {
        transaction.execute(
            "INSERT INTO summaries(meeting_id,template_id,markdown,structured_json,model,created_at) VALUES (?1,'fixture-template',?2,?3,?4,?5)",
            params![meeting, markdown, structured, model, TIMESTAMP],
        )?;
    }
    transaction.execute(
        "INSERT INTO chats(id,scope_kind,scope_id,title,created_at) VALUES ('fixture-chat','meeting',?1,'О бюджете / Budget',?2)",
        params![READY_MEETING_ID, TIMESTAMP],
    )?;
    transaction.execute(
        "INSERT INTO chat_messages(id,chat_id,role,content,citations_json,created_at) VALUES ('fixture-message','fixture-chat','assistant','Бюджет — 14.','[{\"meeting_id\":\"00000000-0000-4000-8000-000000000001\",\"start_ms\":1000,\"end_ms\":2000}]',?1)",
        [TIMESTAMP],
    )?;
    for (token, kind, start, end, enabled) in [
        (MEETING_SHARE_TOKEN, "meeting", None, None, true),
        (CLIP_SHARE_TOKEN, "clip", Some(1000), Some(2000), true),
        (DISABLED_SHARE_TOKEN, "meeting", None, None, false),
    ] {
        transaction.execute(
            "INSERT INTO shares(token,meeting_id,kind,start_ms,end_ms,enabled,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![token, READY_MEETING_ID, kind, start, end, enabled, TIMESTAMP],
        )?;
    }
    transaction.execute(
        "INSERT INTO speakers(id,name,embedding,dim,created_at,embedding_model) VALUES (?1,'Анна',?2,2,?3,?4)",
        params![SPEAKER_ID, embedding, TIMESTAMP, VOICE_MODEL],
    )?;
    transaction.execute(
        "INSERT INTO meeting_speakers(meeting_id,label,speaker_id) VALUES (?1,'Speaker 1',?2)",
        params![READY_MEETING_ID, SPEAKER_ID],
    )?;
    transaction.execute(
        "INSERT INTO voice_samples(id,speaker_id,embedding,dim,model_id,source,source_label,meeting_id,start_ms,end_ms,has_audio,created_at) VALUES (?1,?2,?3,2,?4,'meeting','Орион / Budget review',?5,0,1000,1,?6)",
        params![VOICE_SAMPLE_ID, SPEAKER_ID, embedding, VOICE_MODEL, READY_MEETING_ID, TIMESTAMP],
    )?;
    transaction.execute(
        "INSERT INTO settings(key,value) VALUES (?1,?2)",
        params![format!("stt:{READY_MEETING_ID}"), r#"{"provider":"soniox","model":"synthetic-model","language":"ru","status":"complete","error":null,"captions":[{"start_ms":0,"end_ms":1000,"text":"ээ проект Орион","is_final":true}]}"#],
    )?;
    transaction.commit()?;
    drop(connection);

    for (meeting, audio) in [
        (READY_MEETING_ID, synthetic_wav(4000)),
        (ORPHAN_MEETING_ID, synthetic_wav(1000)),
        (
            CORRUPT_MEETING_ID,
            b"synthetic deliberately invalid WAV\n".to_vec(),
        ),
    ] {
        let directory = root.join("recordings").join(meeting);
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join("mix.wav"), audio)?;
    }
    let voices = root.join("voice-samples");
    std::fs::create_dir(&voices)?;
    std::fs::write(
        voices.join(format!("{VOICE_SAMPLE_ID}.wav")),
        synthetic_wav(1000),
    )?;
    let clips = root.join("clips");
    std::fs::create_dir(&clips)?;
    // A corrupt rendered artifact exercises file lifecycle, not MP3 playback.
    std::fs::write(
        clips.join(format!("{CLIP_SHARE_TOKEN}.mp3")),
        b"synthetic corrupt clip sentinel\n",
    )?;
    // Publish the marker only after the database and artifacts are complete.
    std::fs::write(root.join(MARKER_FILE), MARKER_CONTENT)?;

    Ok(FixtureInfo {
        root: root.to_path_buf(),
        database_path,
        ready_meeting_id: READY_MEETING_ID,
        damaged_meeting_id: DAMAGED_MEETING_ID,
    })
}

fn synthetic_wav(duration_ms: u32) -> Vec<u8> {
    let sample_count = duration_ms * 16;
    let data_size = sample_count * 2;
    let mut bytes = Vec::with_capacity((44 + data_size) as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&32_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for sample in 0..sample_count {
        bytes.extend_from_slice(&((sample % 101) as i16 - 50).to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_store::{MeetingStatus, Store};

    fn logical_snapshot(database: &Path) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let connection = Connection::open(database).unwrap();
        let frozen_schema = Connection::open_in_memory().unwrap();
        frozen_schema
            .execute_batch(include_str!("schema_v8.sql"))
            .unwrap();
        let tables = [
            "meetings",
            "segments",
            "settings",
            "speakers",
            "meeting_speakers",
            "templates",
            "summaries",
            "chats",
            "chat_messages",
            "chunks",
            "folders",
            "tags",
            "meeting_tags",
            "shares",
            "chunk_vectors",
            "voice_samples",
        ];
        tables
            .into_iter()
            .map(|table| {
                let columns = frozen_schema
                    .prepare(&format!("PRAGMA table_info({table})"))
                    .unwrap()
                    .query_map([], |row| row.get::<_, String>(1))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                let selection = columns
                    .iter()
                    .map(|column| format!("\"{column}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                let mut statement = connection
                    .prepare(&format!("SELECT {selection} FROM {table} ORDER BY rowid"))
                    .unwrap();
                let column_count = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        (0..column_count)
                            .map(|column| row.get(column))
                            .collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (table.to_owned(), rows)
            })
            .collect()
    }

    #[test]
    fn frozen_v8_fixture_contains_raw_overlay_missing_and_corrupt_cases() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temporary.path().join("library")).unwrap();
        let connection = Connection::open(&fixture.database_path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, 8,
            "fixture must not follow future production schema"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root.join(MARKER_FILE)).unwrap(),
            MARKER_CONTENT
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM meetings", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            4
        );
        assert!(connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none());
        let overlays: Vec<(i64, Option<String>, bool)> = connection.prepare("SELECT id,clean_text,clean_text_manual FROM segments WHERE meeting_id=?1 ORDER BY id").unwrap()
            .query_map([READY_MEETING_ID], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(
            overlays,
            [
                (101, None, false),
                (102, Some("Бюджет проекта — 14.".into()), true),
                (103, Some("".into()), true),
                (104, Some("Launch next week.".into()), false)
            ]
        );
        assert!(!fixture
            .root
            .join("recordings")
            .join(DAMAGED_MEETING_ID)
            .join("mix.wav")
            .exists());
        assert_eq!(
            std::fs::read(
                fixture
                    .root
                    .join("recordings")
                    .join(CORRUPT_MEETING_ID)
                    .join("mix.wav")
            )
            .unwrap(),
            b"synthetic deliberately invalid WAV\n"
        );
    }

    #[test]
    fn creation_refuses_any_existing_directory_without_modifying_its_contents() {
        let temporary = tempfile::tempdir().unwrap();
        let existing = temporary.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::write(existing.join("sentinel"), "preserve me").unwrap();
        assert!(create_v8(&existing).is_err());
        assert_eq!(
            std::fs::read_to_string(existing.join("sentinel")).unwrap(),
            "preserve me"
        );
        assert!(!existing.join("bazza.db").exists());
        let fixture = create_v8(&temporary.path().join("fixture")).unwrap();
        let original = std::fs::read(&fixture.database_path).unwrap();
        assert!(create_v8(&fixture.root).is_err());
        assert_eq!(std::fs::read(&fixture.database_path).unwrap(), original);
    }

    #[test]
    fn production_upgrade_and_reopen_preserve_fixture_content_and_media_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temporary.path().join("library")).unwrap();
        let media_paths = [
            fixture
                .root
                .join("recordings")
                .join(READY_MEETING_ID)
                .join("mix.wav"),
            fixture
                .root
                .join("recordings")
                .join(ORPHAN_MEETING_ID)
                .join("mix.wav"),
            fixture
                .root
                .join("recordings")
                .join(CORRUPT_MEETING_ID)
                .join("mix.wav"),
            fixture
                .root
                .join("voice-samples")
                .join(format!("{VOICE_SAMPLE_ID}.wav")),
            fixture
                .root
                .join("clips")
                .join(format!("{CLIP_SHARE_TOKEN}.mp3")),
        ];
        let before: Vec<Vec<u8>> = media_paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        let before_rows = logical_snapshot(&fixture.database_path);
        let store = Store::open(&fixture.database_path).unwrap();
        let segments = store.segments(READY_MEETING_ID).unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            [
                "ээ проект Орион",
                "budget maybe fourteen",
                "скрытая вручную реплика",
                "uh next week launch"
            ]
        );
        assert_eq!(segments[0].clean_text, None);
        assert_eq!(
            segments[1].clean_text.as_deref(),
            Some("Бюджет проекта — 14.")
        );
        assert_eq!(segments[2].clean_text.as_deref(), Some(""));
        assert!(segments[1].clean_text_manual && segments[2].clean_text_manual);
        assert!(!segments[3].clean_text_manual);
        let ready = store.get_meeting(READY_MEETING_ID).unwrap().unwrap();
        assert_eq!(ready.status, MeetingStatus::Ready);
        assert_eq!(ready.participants, ["Анна", "Alex"]);
        assert_eq!(
            store
                .get_meeting(DAMAGED_MEETING_ID)
                .unwrap()
                .unwrap()
                .status,
            MeetingStatus::Failed
        );
        assert_eq!(
            store
                .get_meeting(ORPHAN_MEETING_ID)
                .unwrap()
                .unwrap()
                .status,
            MeetingStatus::Recording
        );
        let summary = store.get_summary(READY_MEETING_ID).unwrap().unwrap();
        assert_eq!(summary.model, "synthetic-manual");
        assert_eq!(
            summary.structured,
            serde_json::json!({"fixture_origin":"manual","budget":14})
        );
        assert_eq!(
            store.list_voice_samples(SPEAKER_ID).unwrap()[0].id,
            VOICE_SAMPLE_ID
        );
        assert_eq!(store.list_shares().unwrap().len(), 3);
        assert_eq!(
            store.meeting_tags(READY_MEETING_ID).unwrap()[0].name,
            "MCP baseline"
        );
        let provenance = store
            .get_setting::<serde_json::Value>(&format!("stt:{READY_MEETING_ID}"))
            .unwrap()
            .unwrap();
        assert_eq!(provenance["provider"], "soniox");
        assert_eq!(provenance["captions"][0]["text"], "ээ проект Орион");
        drop(store);
        let reopened = Store::open(&fixture.database_path).unwrap();
        assert_eq!(reopened.segments(READY_MEETING_ID).unwrap(), segments);
        assert_eq!(
            reopened.get_summary(READY_MEETING_ID).unwrap().unwrap(),
            summary
        );
        drop(reopened);
        assert_eq!(
            logical_snapshot(&fixture.database_path),
            before_rows,
            "upgrade/reopen changed existing table values"
        );
        for (path, expected) in media_paths.iter().zip(before) {
            assert_eq!(
                std::fs::read(path).unwrap(),
                expected,
                "media changed: {}",
                path.display()
            );
        }
        assert!(!fixture.root.join("recordings").join(DAMAGED_MEETING_ID).exists(), "missing recorded media must not silently become a text-only meeting or create a directory");
    }
}

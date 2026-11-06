//! Normal native startup must not adopt service schema/projections implicitly.
//! Every database/media path here is a disposable synthetic fixture.
use bazza_store::{
    database_path, schema_version, LibraryOwnerGuard, Meeting, MeetingStatus, MigrationPolicy,
    NewSegment, Store, StoreError, SERVICE_IDENTITY_FILE, SERVICE_OWNER_LOCK_FILE,
    SUPPORTED_SCHEMA,
};
use rusqlite::{Connection, OptionalExtension};
use std::{fs, path::Path};

const LEGACY_SCHEMA: i64 = 9;
const V8_SQL: &str = include_str!("../../bazza-service-test-support/src/schema_v8.sql");
const SERVICE_SQL: &[&str] = &[
    include_str!("../src/service_data/schema.sql"),
    include_str!("../src/service_data/workers.sql"),
    include_str!("../src/service_data/desktop.sql"),
    include_str!("../src/service_data/capture_start.sql"),
    include_str!("../src/service_data/library_state.sql"),
    concat!(
        include_str!("../src/service_data/templates.sql"),
        "\n",
        include_str!("../src/service_data/worker_jobs.sql")
    ),
    concat!(
        include_str!("../src/service_data/postprocess_checkpoint.sql"),
        "\n",
        include_str!("../src/service_data/postprocess_cleanup.sql")
    ),
    include_str!("../src/service_data/chats.sql"),
    include_str!("../src/service_data/postprocess_source.sql"),
    include_str!("../src/service_data/postprocess_speakers.sql"),
    concat!(
        include_str!("../src/service_data/postprocess_summary.sql"),
        "\n",
        include_str!("../src/service_data/postprocess_complete.sql")
    ),
    include_str!("../src/service_data/external_read_grants.sql"),
    concat!(
        include_str!("../src/service_data/capture_terminal_screen.sql"),
        "\n",
        include_str!("../src/service_data/capture_terminal_stt.sql")
    ),
    include_str!("../src/service_data/soniox_permission.sql"),
    include_str!("../src/service_data/postprocess_video.sql"),
    include_str!("../src/service_data/postprocess_speaker_sources.sql"),
];

fn v8(root: &Path) {
    let conn = Connection::open(database_path(root)).unwrap();
    conn.execute_batch(V8_SQL).unwrap();
    conn.execute_batch(r#"
      INSERT INTO meetings(id,title,started_at,status,created_at) VALUES('legacy','Exact legacy title','2026-11-02T00:00:00Z','ready','2026-11-02T00:00:00Z');
      INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual) VALUES
        (41,'legacy',0,1000,'Анна','original providerquasar',NULL,0),
        (42,'legacy',1000,2000,'Анна','original second','manualneptune',1),
        (43,'legacy',2000,3000,NULL,'hidden original','',1);
      INSERT INTO settings(key,value) VALUES('settings','{"capture_system":false,"language":"ja","video_fps":15,"video_max_height":1080}'),
        ('stt:legacy','{"provider":"soniox","model":"synthetic-provider","captions":[]}'),
        ('private:soniox_api_key','"synthetic-private-key"');
      INSERT INTO speakers(id,name,embedding,dim,created_at) VALUES('profile','Saved profile',X'0000803F00000000',2,'2026-11-02T00:00:00Z');
      INSERT INTO voice_samples(id,speaker_id,embedding,dim,model_id,source,source_label,has_audio,created_at)
        SELECT 'sample',id,embedding,dim,embedding_model,'legacy','Synthetic saved sample',0,'2026-11-02T00:00:00Z' FROM speakers;
      INSERT INTO templates(id,name,description,body,builtin,created_at) VALUES('general','General','Existing template','Existing body',1,'2026-11-02T00:00:00Z');
      INSERT INTO summaries(meeting_id,template_id,markdown,structured_json,model,created_at) VALUES('legacy','general','# Existing summary','{"preserve":true}','synthetic-model','2026-11-02T00:00:00Z');
      INSERT INTO folders(id,name,created_at) VALUES('folder','Existing folder','2026-11-02T00:00:00Z');
      INSERT INTO tags(id,name) VALUES('tag','Existing tag');
      INSERT INTO meeting_tags VALUES('legacy','tag');
      INSERT INTO shares(token,meeting_id,kind,enabled,created_at) VALUES('existing-share','legacy','meeting',1,'2026-11-02T00:00:00Z');
    "#).unwrap();
    fs::create_dir_all(root.join("recordings/legacy")).unwrap();
    fs::write(
        root.join("recordings/legacy/mix.wav"),
        b"synthetic media bytes: never decoded or captured",
    )
    .unwrap();
}

fn logical_snapshot(root: &Path) -> Vec<String> {
    let conn = Connection::open(database_path(root)).unwrap();
    [
        "SELECT json_group_array(json_array(id,meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual)) FROM (SELECT * FROM segments ORDER BY id)",
        "SELECT json_group_array(json_array(key,value)) FROM (SELECT * FROM settings ORDER BY key)",
        "SELECT json_group_array(json_array(id,name,hex(embedding),dim,embedding_model)) FROM (SELECT * FROM speakers ORDER BY id)",
        "SELECT json_group_array(json_array(id,speaker_id,hex(embedding),dim,model_id,source,source_label,has_audio)) FROM (SELECT * FROM voice_samples ORDER BY id)",
        "SELECT json_group_array(json_array(meeting_id,template_id,markdown,structured_json,model,created_at)) FROM (SELECT * FROM summaries ORDER BY meeting_id)",
        "SELECT json_group_array(json_array(token,meeting_id,kind,enabled)) FROM (SELECT * FROM shares ORDER BY token)",
    ].into_iter().map(|query| conn.query_row(query,[],|row| row.get(0)).unwrap()).collect()
}
fn schema_snapshot(root: &Path) -> Vec<(String, Option<String>)> {
    let conn = Connection::open(database_path(root)).unwrap();
    let mut statement = conn
        .prepare("SELECT name,sql FROM sqlite_schema ORDER BY name")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    rows.collect::<rusqlite::Result<_>>().unwrap()
}
fn service_snapshot(root: &Path) -> Vec<String> {
    let conn = Connection::open(database_path(root)).unwrap();
    [
        ("service_meeting_state","SELECT json_group_array(json_array(meeting_id,metadata_revision,lifecycle_revision,tombstoned)) FROM (SELECT * FROM service_meeting_state ORDER BY meeting_id)"),
        ("service_transcript_sources","SELECT json_group_array(json_array(id,meeting_id,raw_json)) FROM (SELECT * FROM service_transcript_sources ORDER BY id)"),
        ("service_transcript_overlays","SELECT json_group_array(json_array(source_id,segment_key,clean_text,manual)) FROM (SELECT * FROM service_transcript_overlays ORDER BY source_id,segment_key)"),
        ("service_search_state","SELECT json_group_array(json_array(generation,state,cursor_scope)) FROM service_search_state"),
        ("service_folder_state","SELECT json_group_array(json_array(folder_id,revision)) FROM (SELECT * FROM service_folder_state ORDER BY folder_id)"),
    ].into_iter().filter_map(|(table,sql)| {
        let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",[table],|row|row.get(0)).unwrap();
        exists.then(||conn.query_row(sql,[],|row|row.get(0)).unwrap())
    }).collect()
}
fn service_table_count(root: &Path) -> i64 {
    Connection::open(database_path(root))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND substr(name,1,8)='service_'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn ordinary_new_database_stops_at_v9_without_service_ddl_and_keeps_legacy_storage_operations() {
    let root = tempfile::tempdir().unwrap();
    let path = database_path(root.path());
    let store = Store::open_legacy(&path).unwrap();
    assert_eq!(schema_version(&path).unwrap(), LEGACY_SCHEMA);
    assert_eq!(service_table_count(root.path()), 0);
    assert_eq!(store.root(), root.path());
    assert!(!root.path().join(SERVICE_IDENTITY_FILE).exists());
    assert!(matches!(
        LibraryOwnerGuard::acquire_existing(root.path()),
        Err(StoreError::OwnerBusy)
    ));
    let mut meeting = Meeting::new_recording("Synthetic storage-only recording", None);
    store.create_meeting(&meeting).unwrap();
    store
        .replace_segments(
            &meeting.id,
            &[NewSegment {
                start_ms: 0,
                end_ms: 1000,
                speaker: None,
                text: "preserved raw words".into(),
            }],
        )
        .unwrap();
    meeting.status = MeetingStatus::Ready;
    store.update_processing_state(&meeting).unwrap();
    let segment = store.segments(&meeting.id).unwrap().remove(0);
    let corrected = store
        .set_transcript_correction(&meeting.id, segment.id, Some(""))
        .unwrap();
    assert_eq!(corrected.text, "preserved raw words");
    assert_eq!(corrected.clean_text.as_deref(), Some(""));
    store
        .create_share(&meeting.id, "meeting", None, None)
        .unwrap();
    store.delete_meeting(&meeting.id).unwrap();
    assert!(store.get_meeting(&meeting.id).unwrap().is_none());
    drop(store);
    drop(LibraryOwnerGuard::acquire_existing(root.path()).unwrap());
    assert_eq!(schema_version(&path).unwrap(), LEGACY_SCHEMA);
    assert_eq!(service_table_count(root.path()), 0);
}

#[test]
fn legacy_v8_upgrade_preserves_raw_manual_empty_settings_profiles_media_and_fts() {
    let root = tempfile::tempdir().unwrap();
    v8(root.path());
    let before = logical_snapshot(root.path());
    let media = fs::read(root.path().join("recordings/legacy/mix.wav")).unwrap();
    let store = Store::open_legacy(&database_path(root.path())).unwrap();
    assert_eq!(
        schema_version(&database_path(root.path())).unwrap(),
        LEGACY_SCHEMA
    );
    let segments = store.segments("legacy").unwrap();
    assert_eq!(segments[0].text, "original providerquasar");
    assert_eq!(segments[1].clean_text.as_deref(), Some("manualneptune"));
    assert!(segments[1].clean_text_manual);
    assert_eq!(segments[2].clean_text.as_deref(), Some(""));
    assert!(segments[2].clean_text_manual);
    assert_eq!(
        store.search("providerquasar", 10).unwrap()[0].segment_id,
        41
    );
    assert_eq!(store.search("manualneptune", 10).unwrap()[0].segment_id, 42);
    assert_eq!(
        store.list_voice_profiles().unwrap()[0].name,
        "Saved profile"
    );
    assert_eq!(
        store.list_voice_samples("profile").unwrap()[0].source_label,
        "Synthetic saved sample"
    );
    drop(store);
    assert_eq!(logical_snapshot(root.path()), before);
    assert_eq!(service_table_count(root.path()), 0);
    assert_eq!(
        fs::read(root.path().join("recordings/legacy/mix.wav")).unwrap(),
        media
    );
    let conn = Connection::open(database_path(root.path())).unwrap();
    for name in ["meetings_folder", "meeting_tags_tag"] {
        assert!(conn
            .query_row("SELECT 1 FROM sqlite_schema WHERE name=?1", [name], |row| {
                row.get::<_, i64>(0)
            })
            .optional()
            .unwrap()
            .is_some());
    }
}

#[test]
fn supported_unmarked_v9_through_latest_are_not_downgraded_or_given_later_ddl() {
    for version in LEGACY_SCHEMA..=SUPPORTED_SCHEMA {
        let root = tempfile::tempdir().unwrap();
        v8(root.path());
        drop(Store::open_legacy(&database_path(root.path())).unwrap());
        let conn = Connection::open(database_path(root.path())).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (offset, sql) in SERVICE_SQL
            .iter()
            .take((version - LEGACY_SCHEMA) as usize)
            .enumerate()
        {
            let tx = conn.unchecked_transaction().unwrap();
            tx.execute_batch(sql).unwrap();
            tx.pragma_update(None, "user_version", LEGACY_SCHEMA + 1 + offset as i64)
                .unwrap();
            tx.commit().unwrap();
        }
        drop(conn);
        let schema = schema_snapshot(root.path());
        let logical = logical_snapshot(root.path());
        let service = service_snapshot(root.path());
        let store = Store::open_legacy(&database_path(root.path())).unwrap();
        assert_eq!(
            schema_version(&database_path(root.path())).unwrap(),
            version
        );
        assert_eq!(schema_snapshot(root.path()), schema);
        assert_eq!(logical_snapshot(root.path()), logical);
        assert_eq!(service_snapshot(root.path()), service);
        drop(store);
        let guard = LibraryOwnerGuard::acquire_existing(root.path()).unwrap();
        if version < SUPPORTED_SCHEMA {
            assert!(matches!(
                Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent),
                Err(StoreError::MigrationRequired { .. })
            ));
        } else {
            drop(Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent).unwrap());
        }
        let guard = LibraryOwnerGuard::acquire_existing(root.path()).unwrap();
        drop(Store::open_existing_owned(guard, MigrationPolicy::Upgrade).unwrap());
        assert_eq!(
            schema_version(&database_path(root.path())).unwrap(),
            SUPPORTED_SCHEMA
        );
        assert_eq!(logical_snapshot(root.path()), logical);
    }
}

#[test]
fn legacy_migration_failure_rolls_back_its_ddl_and_releases_owner_for_retry() {
    let root = tempfile::tempdir().unwrap();
    v8(root.path());
    let before = logical_snapshot(root.path());
    let conn = Connection::open(database_path(root.path())).unwrap();
    // v9 creates meetings_folder first, then this intentionally conflicting
    // second name. Failure must roll the first CREATE and user_version back.
    conn.execute_batch("CREATE INDEX meeting_tags_tag ON meeting_tags(meeting_id)")
        .unwrap();
    drop(conn);
    assert!(matches!(
        Store::open_legacy(&database_path(root.path())),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(schema_version(&database_path(root.path())).unwrap(), 8);
    let conn = Connection::open(database_path(root.path())).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='meetings_folder'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(logical_snapshot(root.path()), before);
    drop(LibraryOwnerGuard::acquire_existing(root.path()).unwrap());
    conn.execute_batch("DROP INDEX meeting_tags_tag").unwrap();
    drop(conn);
    drop(Store::open_legacy(&database_path(root.path())).unwrap());
    assert_eq!(
        schema_version(&database_path(root.path())).unwrap(),
        LEGACY_SCHEMA
    );
}

#[test]
fn legacy_marker_and_future_schema_refusals_precede_sqlite_and_new_lock_sidecars() {
    let root = tempfile::tempdir().unwrap();
    v8(root.path());
    fs::write(
        root.path().join(SERVICE_IDENTITY_FILE),
        b"malformed service identity",
    )
    .unwrap();
    let before = fs::read(database_path(root.path())).unwrap();
    assert!(matches!(
        Store::open_legacy(&database_path(root.path())),
        Err(StoreError::ServiceRequired)
    ));
    assert_eq!(fs::read(database_path(root.path())).unwrap(), before);
    assert!(!root.path().join(SERVICE_OWNER_LOCK_FILE).exists());
    let future = tempfile::tempdir().unwrap();
    let conn = Connection::open(database_path(future.path())).unwrap();
    conn.execute_batch("CREATE TABLE sentinel(value TEXT)")
        .unwrap();
    conn.pragma_update(None, "user_version", SUPPORTED_SCHEMA + 1)
        .unwrap();
    drop(conn);
    let before = fs::read(database_path(future.path())).unwrap();
    assert!(matches!(
        Store::open_legacy(&database_path(future.path())),
        Err(StoreError::FutureSchema { .. })
    ));
    assert_eq!(fs::read(database_path(future.path())).unwrap(), before);
    for name in [SERVICE_OWNER_LOCK_FILE, "bazza.db-wal", "bazza.db-shm"] {
        assert!(!future.path().join(name).exists());
    }
}

#[test]
fn developer_open_and_owned_create_still_target_latest_schema() {
    let developer = tempfile::tempdir().unwrap();
    drop(Store::open(&database_path(developer.path())).unwrap());
    assert_eq!(
        schema_version(&database_path(developer.path())).unwrap(),
        SUPPORTED_SCHEMA
    );
    let owned = tempfile::tempdir().unwrap();
    let guard = LibraryOwnerGuard::acquire_for_create(owned.path()).unwrap();
    drop(Store::create_owned(guard).unwrap());
    assert_eq!(
        schema_version(&database_path(owned.path())).unwrap(),
        SUPPORTED_SCHEMA
    );
}

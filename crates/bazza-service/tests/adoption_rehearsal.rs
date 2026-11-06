#![cfg(any(windows, target_os = "macos"))]
//! Exercise the CLI against disposable legacy-shaped libraries.

use bazza_core::{rehearse_adoption, CoreError, IDENTITY_FILE};
use bazza_service_api::rpc::STORAGE_SCHEMA_MAX;
use bazza_service_test_support::{create_v8, FixtureInfo, MARKER_FILE, READY_MEETING_ID};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn legacy(root: &Path) -> FixtureInfo {
    let fixture = create_v8(root).unwrap();
    fs::remove_file(root.join(MARKER_FILE)).unwrap();
    fixture
}

fn run_rehearsal(source: &Path, destination: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-service"));
    command
        .args(["rehearse-adoption", "--library"])
        .arg(source)
        .arg("--rehearsal")
        .arg(destination);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.output().unwrap()
}

fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

#[test]
fn rehearsal_preserves_source_and_publishes_rollback_and_migrated_copy() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = legacy(&temporary.path().join("existing library"));
    let destination = temporary.path().join("rehearsal Орион");
    let database_before = fs::read(&fixture.database_path).unwrap();
    let audio = fixture
        .root
        .join("recordings")
        .join(READY_MEETING_ID)
        .join("mix.wav");
    let audio_before = fs::read(&audio).unwrap();
    let output = run_rehearsal(&fixture.root, &destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_schema"], 8);
    assert_eq!(report["candidate_schema"], STORAGE_SCHEMA_MAX);
    assert_eq!(fs::read(&fixture.database_path).unwrap(), database_before);
    assert_eq!(fs::read(audio).unwrap(), audio_before);
    assert!(!fixture.root.join(IDENTITY_FILE).exists());
    assert!(!fixture.root.join(MARKER_FILE).exists());
    assert!(!fixture.root.join(".bazza-service-endpoint.json").exists());

    let rollback = destination.join("snapshot/rollback");
    let candidate = destination.join("snapshot/candidate");
    assert!(candidate.join(IDENTITY_FILE).is_file());
    assert!(!candidate.join(MARKER_FILE).exists());
    assert!(!candidate.join("recordings").exists());
    assert!(!rollback.join(IDENTITY_FILE).exists());
    for (root, version) in [
        (rollback.as_path(), 8),
        (candidate.as_path(), STORAGE_SCHEMA_MAX),
    ] {
        let db = read_only(&root.join("bazza.db"));
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            version
        );
        let (raw, cleaned, manual): (String, Option<String>, bool) = db
            .query_row(
                "SELECT text,clean_text,clean_text_manual FROM segments WHERE id=103",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(raw, "скрытая вручную реплика");
        assert_eq!(cleaned.as_deref(), Some(""));
        assert!(manual);
    }
    let db = read_only(&candidate.join("bazza.db"));
    assert_eq!(
        db.query_row("SELECT state FROM service_search_state", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "ready"
    );
    assert!(bazza_core::require_synthetic_fixture(&candidate).is_err());
}

#[test]
fn rehearsal_rejects_invalid_destinations() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = legacy(&temporary.path().join("source"));
    let parent = temporary.path().canonicalize().unwrap();
    let destination = parent.join("rehearsal");
    let nested = fixture.root.join("rehearsal");
    for rejected in [&nested, &fixture.root, &parent] {
        assert!(matches!(
            rehearse_adoption(&fixture.root, rejected),
            Err(CoreError::InvalidRehearsalPath)
        ));
    }
    assert!(!nested.exists());
    fs::create_dir(&destination).unwrap();
    let sentinel = destination.join("keep");
    fs::write(&sentinel, b"existing destination").unwrap();
    assert!(rehearse_adoption(&fixture.root, &destination).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"existing destination");
}

#[test]
fn failed_candidate_migration_discards_staging_and_preserves_source() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = legacy(&temporary.path().join("source"));
    let db = Connection::open(&fixture.database_path).unwrap();
    db.execute("CREATE TABLE service_meeting_state(collision INTEGER)", [])
        .unwrap();
    drop(db);
    let before = fs::read(&fixture.database_path).unwrap();
    let destination = temporary.path().join("failed rehearsal");
    let output = run_rehearsal(&fixture.root, &destination);
    assert!(!output.status.success());
    assert!(!destination.exists());
    assert_eq!(fs::read(&fixture.database_path).unwrap(), before);
    assert!(!fixture.root.join(IDENTITY_FILE).exists());
}

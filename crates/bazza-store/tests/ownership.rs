//! Current standard legacy opens and service opens share OS ownership. Every
//! path is disposable; historical binaries remain outside this protocol.

use bazza_store::{
    database_path, schema_version, LibraryOwnerGuard, MigrationPolicy, Store, StoreError,
    SUPPORTED_SCHEMA,
};
use rusqlite::Connection;
use std::{collections::BTreeMap, fs, path::Path};

type SqliteFiles = BTreeMap<&'static str, Option<Vec<u8>>>;

fn sqlite_files(root: &Path) -> SqliteFiles {
    [
        "bazza.db",
        "bazza.db-wal",
        "bazza.db-shm",
        "bazza.db-journal",
    ]
    .into_iter()
    .map(|name| {
        let bytes = match fs::read(root.join(name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("could not snapshot {name}: {error}"),
        };
        (name, bytes)
    })
    .collect()
}

fn characterize_coordination(label: &str, before: &SqliteFiles, after: &SqliteFiles) {
    // WAL is the committed source, whereas SHM is SQLite's coordination state.
    // Comparing every file records changes without promising that a read-only
    // SQLite connection cannot create/change its required coordination files.
    for name in before.keys() {
        if before[name] != after[name] {
            eprintln!(
                "OWNERSHIP_WAL_EVIDENCE {label}: {name} {:?} -> {:?} bytes",
                before[name].as_ref().map(Vec::len),
                after[name].as_ref().map(Vec::len)
            );
        }
    }
    assert_eq!(
        before["bazza.db"], after["bazza.db"],
        "{label}: future refusal changed database bytes"
    );
    if before["bazza.db-wal"].is_some() {
        assert_eq!(
            before["bazza.db-wal"], after["bazza.db-wal"],
            "{label}: future refusal changed existing committed WAL"
        );
    }
}

fn header_version(root: &Path) -> i64 {
    let bytes = fs::read(database_path(root)).unwrap();
    // SQLite format stores user_version as a big-endian 32-bit value at byte 60.
    i32::from_be_bytes(bytes[60..64].try_into().unwrap()) as i64
}

fn future_database(root: &Path) {
    let conn = Connection::open(database_path(root)).unwrap();
    conn.execute_batch("CREATE TABLE sentinel(raw TEXT, manual TEXT); INSERT INTO sentinel VALUES ('original raw', '');").unwrap();
    conn.pragma_update(None, "user_version", SUPPORTED_SCHEMA + 1)
        .unwrap();
}

fn assert_no_sqlite_sidecars(root: &Path) {
    for name in ["bazza.db-wal", "bazza.db-shm", "bazza.db-journal"] {
        assert!(!root.join(name).exists(), "unexpected sidecar: {name}");
    }
}

#[test]
fn existing_open_and_schema_inspection_never_create_missing_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("missing/library");
    assert!(matches!(
        LibraryOwnerGuard::acquire_existing(&missing),
        Err(StoreError::LibraryMissing)
    ));
    assert!(matches!(
        LibraryOwnerGuard::acquire_for_create(&missing),
        Err(StoreError::LibraryMissing)
    ));
    assert!(matches!(
        schema_version(&database_path(&missing)),
        Err(StoreError::LibraryMissing)
    ));
    assert!(!temporary.path().join("missing").exists());

    let empty = temporary.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert!(matches!(
        LibraryOwnerGuard::acquire_existing(&empty),
        Err(StoreError::LibraryMissing)
    ));
    assert!(matches!(
        schema_version(&database_path(&empty)),
        Err(StoreError::LibraryMissing)
    ));
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
}

#[test]
fn owned_creation_holds_canonical_ownership_until_store_closes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir(&root).unwrap();
    let guard = LibraryOwnerGuard::acquire_for_create(&root).unwrap();
    assert_eq!(guard.root(), root.canonicalize().unwrap());
    let store = Store::create_owned(guard).unwrap();
    store
        .set_setting("synthetic-marker", &"persisted through owner close")
        .unwrap();
    let alias = root.join("..").join("library").join(".");
    assert!(matches!(
        LibraryOwnerGuard::acquire_existing(&alias),
        Err(StoreError::OwnerBusy)
    ));
    assert!(matches!(
        LibraryOwnerGuard::acquire_for_create(&root),
        Err(StoreError::LibraryExists)
    ));
    drop(store);

    // The lock pathname persists; only the OS lock is released. SQLite closes
    // first, so its final WAL contents are readable under the next owner.
    assert!(root.join(".bazza-service-owner.lock").is_file());
    assert_no_sqlite_sidecars(&root);
    let guard = LibraryOwnerGuard::acquire_existing(&alias).unwrap();
    let reopened = Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent).unwrap();
    assert_eq!(
        schema_version(&database_path(&root)).unwrap(),
        SUPPORTED_SCHEMA
    );
    assert_eq!(
        reopened
            .get_setting::<String>("synthetic-marker")
            .unwrap()
            .as_deref(),
        Some("persisted through owner close")
    );
}

#[test]
fn create_rechecks_database_absence_and_never_overwrites_a_late_file() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let guard = LibraryOwnerGuard::acquire_for_create(root).unwrap();
    // Simulate an unsupported legacy writer that does not honor the new guard.
    let sentinel = b"late unrelated database file";
    fs::write(database_path(root), sentinel).unwrap();
    assert!(matches!(
        Store::create_owned(guard),
        Err(StoreError::LibraryExists)
    ));
    assert_eq!(fs::read(database_path(root)).unwrap(), sentinel);
    assert_no_sqlite_sidecars(root);
}

#[test]
fn future_schema_is_rejected_before_legacy_or_owned_wal_and_migrations() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    future_database(root);
    let database = database_path(root);
    let before = fs::read(&database).unwrap();
    assert_eq!(schema_version(&database).unwrap(), SUPPORTED_SCHEMA + 1);
    assert!(
        matches!(Store::open(&database), Err(StoreError::FutureSchema { found, supported })
        if found == SUPPORTED_SCHEMA + 1 && supported == SUPPORTED_SCHEMA)
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_no_sqlite_sidecars(root);

    for policy in [MigrationPolicy::RequireCurrent, MigrationPolicy::Upgrade] {
        let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
        assert!(matches!(
            Store::open_existing_owned(guard, policy),
            Err(StoreError::FutureSchema { .. })
        ));
        assert_eq!(fs::read(&database).unwrap(), before);
        assert_no_sqlite_sidecars(root);
        // Failure releases the guard too; a later administrative fix is possible.
        drop(LibraryOwnerGuard::acquire_existing(root).unwrap());
    }
}

#[test]
fn closed_future_wal_header_is_rejected_without_creating_sqlite_sidecars() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let connection = Connection::open(database_path(root)).unwrap();
    connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE sentinel(raw TEXT, manual TEXT); INSERT INTO sentinel VALUES ('original raw', '');").unwrap();
    connection
        .pragma_update(None, "user_version", SUPPORTED_SCHEMA + 1)
        .unwrap();
    let while_creator_open = sqlite_files(root);
    assert!(while_creator_open["bazza.db-wal"].is_some());
    assert!(while_creator_open["bazza.db-shm"].is_some());
    drop(connection);
    let before_legacy = sqlite_files(root);
    assert_eq!(header_version(root), SUPPORTED_SCHEMA + 1);
    assert_no_sqlite_sidecars(root);
    eprintln!("OWNERSHIP_WAL_EVIDENCE creator-close: committed WAL checkpointed to the future-version database; WAL/SHM removed.");

    assert_eq!(
        schema_version(&database_path(root)).unwrap(),
        SUPPORTED_SCHEMA + 1
    );
    assert_eq!(
        sqlite_files(root),
        before_legacy,
        "future header inspection must not open SQLite or create coordination files"
    );

    assert!(matches!(
        Store::open(&database_path(root)),
        Err(StoreError::FutureSchema { .. })
    ));
    let after_legacy = sqlite_files(root);
    characterize_coordination("closed-WAL legacy refusal", &before_legacy, &after_legacy);
    assert_eq!(
        after_legacy, before_legacy,
        "closed future WAL refusal must not create sidecars"
    );
    assert_no_sqlite_sidecars(root);
    assert_eq!(header_version(root), SUPPORTED_SCHEMA + 1);

    let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
    let before_owned = sqlite_files(root);
    assert!(matches!(
        Store::open_existing_owned(guard, MigrationPolicy::Upgrade),
        Err(StoreError::FutureSchema { .. })
    ));
    let after_owned = sqlite_files(root);
    characterize_coordination("closed-WAL owned refusal", &before_owned, &after_owned);
    assert_eq!(
        after_owned, before_owned,
        "owned future WAL refusal must not create sidecars"
    );
    assert_no_sqlite_sidecars(root);
    drop(LibraryOwnerGuard::acquire_existing(root).unwrap());
}

#[test]
fn active_wal_future_version_is_honored_even_when_database_header_is_supported() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let writer = Connection::open(database_path(root)).unwrap();
    writer.execute_batch("CREATE TABLE sentinel(raw TEXT, manual TEXT); INSERT INTO sentinel VALUES ('original raw', '');").unwrap();
    writer
        .pragma_update(None, "user_version", SUPPORTED_SCHEMA)
        .unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    assert_eq!(header_version(root), SUPPORTED_SCHEMA);
    writer
        .pragma_update(None, "user_version", SUPPORTED_SCHEMA + 1)
        .unwrap();
    assert_eq!(
        header_version(root),
        SUPPORTED_SCHEMA,
        "fixture must leave the newer user_version only in committed WAL"
    );
    let before = sqlite_files(root);
    assert!(before["bazza.db-wal"].is_some());
    assert!(before["bazza.db-shm"].is_some());

    // An immutable/header-only opener would read the supported header and miss
    // this committed version. Inspection and both constructor policies must read
    // SQLite's effective state while leaving the writer and its WAL intact.
    assert_eq!(
        schema_version(&database_path(root)).unwrap(),
        SUPPORTED_SCHEMA + 1
    );
    assert!(matches!(
        Store::open(&database_path(root)),
        Err(StoreError::FutureSchema { .. })
    ));
    for policy in [MigrationPolicy::RequireCurrent, MigrationPolicy::Upgrade] {
        let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
        assert!(matches!(
            Store::open_existing_owned(guard, policy),
            Err(StoreError::FutureSchema { .. })
        ));
    }
    let after = sqlite_files(root);
    characterize_coordination("active-WAL future refusal", &before, &after);
    assert_eq!(header_version(root), SUPPORTED_SCHEMA);
    assert_eq!(
        writer
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SUPPORTED_SCHEMA + 1
    );
    assert_eq!(
        writer
            .query_row("SELECT raw, manual FROM sentinel", [], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            )))
            .unwrap(),
        ("original raw".into(), "".into())
    );
    drop(writer);
    assert_eq!(header_version(root), SUPPORTED_SCHEMA + 1);
    assert_no_sqlite_sidecars(root);
    eprintln!("OWNERSHIP_WAL_EVIDENCE active-writer-close: original writer remained usable and performed its normal final checkpoint.");
}

#[test]
fn malformed_database_is_preserved_without_wal_or_implicit_repair() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let database = database_path(root);
    let sentinel = b"not a SQLite database; preserve these synthetic bytes";
    fs::write(&database, sentinel).unwrap();
    assert!(matches!(Store::open(&database), Err(StoreError::Sqlite(_))));
    assert_eq!(fs::read(&database).unwrap(), sentinel);
    assert_no_sqlite_sidecars(root);
    let guard = LibraryOwnerGuard::acquire_existing(root).unwrap();
    assert!(matches!(
        Store::open_existing_owned(guard, MigrationPolicy::Upgrade),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(fs::read(&database).unwrap(), sentinel);
    assert_no_sqlite_sidecars(root);
}

#[test]
fn non_sqlite_magic_does_not_interpret_arbitrary_header_bytes_as_a_schema() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let database = database_path(root);
    let mut bytes = vec![0x7f; 128];
    bytes[60..64].copy_from_slice(&((SUPPORTED_SCHEMA + 1) as i32).to_be_bytes());
    fs::write(&database, &bytes).unwrap();
    assert!(matches!(
        schema_version(&database),
        Err(StoreError::Sqlite(_))
    ));
    assert!(matches!(Store::open(&database), Err(StoreError::Sqlite(_))));
    assert_eq!(fs::read(&database).unwrap(), bytes);
    assert_no_sqlite_sidecars(root);
}

#[cfg(any(windows, unix))]
#[test]
fn owned_database_and_owner_lock_reject_links_without_touching_the_target() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as link_file;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as link_file;

    for name in ["bazza.db", ".bazza-service-owner.lock"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir(&root).unwrap();
        let database = database_path(&root);
        drop(Store::open(&database).unwrap());
        let before_database = fs::read(&database).unwrap();
        let target = temporary.path().join("unrelated-target");
        let before_target = if name == "bazza.db" {
            before_database.clone()
        } else {
            b"unrelated lock bytes".to_vec()
        };
        fs::write(&target, &before_target).unwrap();
        let link = root.join(name);
        // Store has closed. Replace only this test's own regular database/lock
        // fixture; live production lock pathnames are never removed.
        fs::remove_file(&link).unwrap();
        if let Err(error) = link_file(&target, &link) {
            #[cfg(windows)]
            if error.raw_os_error() == Some(1314) {
                eprintln!(
                    "OWNERSHIP_EVIDENCE: Windows symlink case not exercised; privilege error 1314."
                );
                continue;
            }
            panic!("could not create owned test symlink: {error}");
        }
        assert!(matches!(
            LibraryOwnerGuard::acquire_existing(&root),
            Err(StoreError::InvalidLibraryPath)
        ));
        assert_eq!(fs::read(&target).unwrap(), before_target);
        assert_eq!(fs::read(&database).unwrap(), before_database);
        assert_no_sqlite_sidecars(&root);
    }
}

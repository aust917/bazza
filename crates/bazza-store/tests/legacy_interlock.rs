//! Current-build interlock only. Historical binaries do not honor this protocol.
use bazza_store::{
    database_path, LibraryOwnerGuard, MigrationPolicy, Store, StoreError, SERVICE_IDENTITY_FILE,
    SUPPORTED_SCHEMA,
};
use std::{collections::BTreeMap, fs, path::Path};

fn sqlite_files(root: &Path) -> BTreeMap<&'static str, Option<Vec<u8>>> {
    [
        "bazza.db",
        "bazza.db-wal",
        "bazza.db-shm",
        "bazza.db-journal",
        "snapshot.db",
    ]
    .into_iter()
    .map(|name| (name, fs::read(root.join(name)).ok()))
    .collect()
}
fn create(root: &Path) {
    let store = Store::open(&database_path(root)).unwrap();
    store
        .set_setting("interlock-fixture", &"original raw marker")
        .unwrap();
}

#[test]
fn service_identity_any_regular_contents_or_directory_blocks_all_legacy_names_before_sqlite() {
    for contents in [
        b"".as_slice(),
        b"malformed".as_slice(),
        br#"{"version":1,"library_id":"00000000-0000-4000-8000-000000000001"}"#.as_slice(),
    ] {
        let root = tempfile::tempdir().unwrap();
        create(root.path());
        fs::write(root.path().join(SERVICE_IDENTITY_FILE), contents).unwrap();
        let before = sqlite_files(root.path());
        for name in ["bazza.db", "snapshot.db"] {
            assert!(matches!(
                Store::open(&root.path().join(name)),
                Err(StoreError::ServiceRequired)
            ));
            assert_eq!(sqlite_files(root.path()), before);
        }
    }
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(SERVICE_IDENTITY_FILE)).unwrap();
    assert!(matches!(
        Store::open(&database_path(root.path())),
        Err(StoreError::ServiceRequired)
    ));
    assert!(!database_path(root.path()).exists());
    assert!(!root.path().join(".bazza-service-owner.lock").exists());
}

#[cfg(any(windows, unix))]
#[test]
fn dangling_service_identity_link_is_presence_and_does_not_touch_its_target() {
    let root = tempfile::tempdir().unwrap();
    create(root.path());
    let missing = root.path().join("missing-identity-target");
    let marker = root.path().join(SERVICE_IDENTITY_FILE);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(&missing, &marker);
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(&missing, &marker);
    if let Err(error) = result {
        #[cfg(windows)]
        if error.raw_os_error() == Some(1314) {
            eprintln!(
                "INTERLOCK_EVIDENCE: dangling-link case unavailable (Windows privilege 1314)"
            );
            return;
        }
        panic!("could not create owned marker link: {error}");
    }
    let before = sqlite_files(root.path());
    assert!(matches!(
        Store::open(&database_path(root.path())),
        Err(StoreError::ServiceRequired)
    ));
    assert_eq!(sqlite_files(root.path()), before);
    assert!(!missing.exists());
}

#[test]
fn legacy_first_holds_service_owner_and_duplicate_legacy_until_connection_closes() {
    let root = tempfile::tempdir().unwrap();
    let path = database_path(root.path());
    let store = Store::open(&path).unwrap();
    store.set_setting("interlock-fixture", &true).unwrap();
    assert_eq!(
        store.root(),
        root.path(),
        "legacy capture path spelling changed"
    );
    let alias = root.path().join(".");
    assert!(matches!(
        LibraryOwnerGuard::acquire_existing(&alias),
        Err(StoreError::OwnerBusy)
    ));
    assert!(matches!(Store::open(&path), Err(StoreError::OwnerBusy)));
    drop(store);
    let guard = LibraryOwnerGuard::acquire_existing(&alias).unwrap();
    let service = Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent).unwrap();
    assert_eq!(
        service.get_setting::<bool>("interlock-fixture").unwrap(),
        Some(true)
    );
    drop(service);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.get_setting::<bool>("interlock-fixture").unwrap(),
        Some(true)
    );
}

#[test]
fn service_first_blocks_legacy_while_active_and_identity_remains_a_barrier_after_close() {
    let root = tempfile::tempdir().unwrap();
    create(root.path());
    let guard = LibraryOwnerGuard::acquire_existing(root.path()).unwrap();
    // Before marker publication the common OS lock closes the service-first gap.
    assert!(matches!(
        Store::open(&database_path(root.path())),
        Err(StoreError::OwnerBusy)
    ));
    fs::write(
        root.path().join(SERVICE_IDENTITY_FILE),
        b"service-owned fixture",
    )
    .unwrap();
    let before = sqlite_files(root.path());
    assert!(matches!(
        Store::open(&database_path(root.path())),
        Err(StoreError::ServiceRequired)
    ));
    assert_eq!(sqlite_files(root.path()), before);
    let service = Store::open_existing_owned(guard, MigrationPolicy::RequireCurrent).unwrap();
    assert_eq!(
        service
            .get_setting::<String>("interlock-fixture")
            .unwrap()
            .as_deref(),
        Some("original raw marker")
    );
    drop(service);
    let before = sqlite_files(root.path());
    assert!(matches!(
        Store::open(&database_path(root.path())),
        Err(StoreError::ServiceRequired)
    ));
    assert_eq!(sqlite_files(root.path()), before);
}

#[test]
fn nonstandard_unmarked_snapshot_paths_retain_existing_parallel_open_behavior() {
    let root = tempfile::tempdir().unwrap();
    let normal = Store::open(&database_path(root.path())).unwrap();
    normal.set_setting("snapshot-fixture", &"original").unwrap();
    let path = root.path().join("snapshot.db");
    normal.snapshot_to(&path).unwrap();
    let first = Store::open(&path).unwrap();
    let second = Store::open(&path).unwrap();
    assert_eq!(
        first
            .get_setting::<String>("snapshot-fixture")
            .unwrap()
            .as_deref(),
        Some("original")
    );
    second
        .set_setting("snapshot-fixture", &"snapshot only")
        .unwrap();
    assert_eq!(
        normal
            .get_setting::<String>("snapshot-fixture")
            .unwrap()
            .as_deref(),
        Some("original")
    );
    assert!(matches!(
        LibraryOwnerGuard::acquire_existing(root.path()),
        Err(StoreError::OwnerBusy)
    ));
    drop(first);
    drop(second);
    drop(normal);
    drop(LibraryOwnerGuard::acquire_existing(root.path()).unwrap());
}

#[test]
fn future_header_refusal_creates_neither_owner_lock_nor_sqlite_sidecars() {
    let root = tempfile::tempdir().unwrap();
    let path = database_path(root.path());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('raw preserved');",
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", SUPPORTED_SCHEMA + 1)
        .unwrap();
    drop(connection);
    let before = sqlite_files(root.path());
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::FutureSchema { .. })
    ));
    assert_eq!(sqlite_files(root.path()), before);
    assert!(!root.path().join(".bazza-service-owner.lock").exists());
}

use crate::{migrations, owner, LibraryOwnerGuard, Result, StoreError, SUPPORTED_SCHEMA};
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Default location: `%APPDATA%\Bazza` on Windows,
/// `~/Library/Application Support/Bazza` on macOS, `~/.local/share/bazza` elsewhere.
pub fn default_data_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "linux") {
        base.join("bazza")
    } else {
        base.join("Bazza")
    }
}

const POINTER_FILE: &str = "data-dir.txt";

fn redirected_dir(root: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(root.join(POINTER_FILE))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
}

fn resolve_data_dir(default: &Path) -> PathBuf {
    redirected_dir(default).unwrap_or_else(|| default.to_path_buf())
}

/// Root of all Bazza data. The Bazza location may hold a `data-dir.txt` pointer
/// to a user-chosen directory. An explicit process-local BAZZA_DATA_DIR bypasses
/// that pointer for isolated development runs. Created on first call.
pub fn data_dir() -> PathBuf {
    let override_dir = std::env::var_os("BAZZA_DATA_DIR");
    let explicit = explicit_data_dir(override_dir.as_deref()).expect(
        "BAZZA_DATA_DIR must be a nonempty absolute path; refusing to open the default library",
    );
    // Do not even consult the normal library or its pointer in an isolated run.
    if let Some(dir) = explicit {
        std::fs::create_dir_all(&dir).expect("create BAZZA_DATA_DIR");
        return dir;
    }
    let default = default_data_dir();
    let dir = resolve_data_dir(&default);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn explicit_data_dir(
    value: Option<&std::ffi::OsStr>,
) -> std::result::Result<Option<PathBuf>, &'static str> {
    match value {
        None => Ok(None),
        Some(value) if !value.is_empty() && Path::new(value).is_absolute() => {
            Ok(Some(PathBuf::from(value)))
        }
        Some(_) => Err("expected a nonempty absolute path"),
    }
}

/// The Bazza database within a data directory.
pub fn database_path(root: &Path) -> PathBuf {
    root.join("bazza.db")
}

/// Inspect an existing database without running migrations or changing PRAGMAs.
/// A valid future-version SQLite header is returned before opening SQLite, so a
/// closed future WAL library does not acquire new coordination sidecars. Lower
/// headers still use SQLite's effective state, including committed WAL; read-only
/// SQLite may update required SHM coordination. Downgrades are unsupported.
pub fn schema_version(path: &Path) -> Result<i64> {
    let metadata = std::fs::metadata(path).map_err(owner::missing_as_library)?;
    if !metadata.is_file() {
        return Err(StoreError::InvalidLibraryPath);
    }
    if let Some(current) = header_schema_version(path)? {
        if current > SUPPORTED_SCHEMA {
            return Ok(current);
        }
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Pure header preflight: no SQLite open, PRAGMAs or owner-lock creation. A
/// lower header is not authoritative when a committed WAL may contain a newer
/// version; constructors still inspect SQLite's effective version under lock.
fn header_schema_version(path: &Path) -> Result<Option<i64>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut header = [0_u8; 100];
    let complete = match file.read_exact(&mut header) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(error) => return Err(error.into()),
    };
    Ok(
        (complete && header.starts_with(b"SQLite format 3\0")).then(|| {
            i64::from(i32::from_be_bytes([
                header[60], header[61], header[62], header[63],
            ]))
        }),
    )
}

fn standard_database_name(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    #[cfg(windows)]
    {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("bazza.db"))
    }
    #[cfg(not(windows))]
    {
        name == std::ffi::OsStr::new("bazza.db")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationPolicy {
    RequireCurrent,
    Upgrade,
}

#[derive(Clone, Copy)]
enum SchemaTarget {
    Latest,
    Legacy,
}

impl SchemaTarget {
    fn version(self) -> i64 {
        match self {
            Self::Latest => SUPPORTED_SCHEMA,
            Self::Legacy => migrations::LEGACY_SCHEMA,
        }
    }
}

fn check_schema(current: i64, policy: MigrationPolicy) -> Result<()> {
    migrations::ensure_supported(current)?;
    if policy == MigrationPolicy::RequireCurrent && current != SUPPORTED_SCHEMA {
        return Err(StoreError::MigrationRequired {
            found: current,
            supported: SUPPORTED_SCHEMA,
        });
    }
    Ok(())
}

/// Point future launches at `dir` (pass the default dir to clear the override).
pub fn write_data_dir_pointer(dir: &Path) -> std::io::Result<()> {
    let default = default_data_dir();
    std::fs::create_dir_all(&default)?;
    if dir == default {
        let _ = std::fs::remove_file(default.join(POINTER_FILE));
        Ok(())
    } else {
        std::fs::write(default.join(POINTER_FILE), dir.to_string_lossy().as_bytes())
    }
}

pub struct Store {
    // Fields drop in declaration order: SQLite must close before ownership ends.
    pub(crate) conn: Mutex<Connection>,
    root: PathBuf,
    _owner: Option<LibraryOwnerGuard>,
}

impl Store {
    /// Full-schema developer open/create. Any service identity entry refuses
    /// access before SQLite. Normal bazza.db holds canonical ownership until
    /// close; ordinary native startup must use open_legacy before service cutover.
    /// Nonstandard unmarked database/snapshot paths retain their path behavior.
    pub fn open(path: &Path) -> Result<Store> {
        Self::open_target(path, SchemaTarget::Latest)
    }

    /// Ordinary native GUI/recorder open before service cutover. Uses the same
    /// marker/ownership/path interlock as open, but only upgrades older data to
    /// the last legacy schema. Supported newer unmarked data stays unchanged;
    /// this does not repair or reconcile existing service projections.
    pub fn open_legacy(path: &Path) -> Result<Store> {
        Self::open_target(path, SchemaTarget::Legacy)
    }

    fn open_target(path: &Path, target: SchemaTarget) -> Result<Store> {
        let root = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let admission_root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        owner::refuse_service_identity(admission_root)?;
        if standard_database_name(path) {
            match std::fs::symlink_metadata(path) {
                Ok(_) => {
                    owner::validate_regular_file(path)?;
                    if let Some(version) = header_schema_version(path)? {
                        migrations::ensure_supported(version)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let guard = LibraryOwnerGuard::acquire_legacy_root(admission_root)?;
            let mut store = match std::fs::symlink_metadata(database_path(guard.root())) {
                Ok(_) => Self::open_existing_owned_target(guard, MigrationPolicy::Upgrade, target)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Self::create_owned_target(guard, target)?
                }
                Err(error) => return Err(error.into()),
            };
            // Keep existing recorder/FFmpeg/UI path spelling. Ownership alone
            // uses a canonical path; introducing Windows extended prefixes into
            // capture defaults is outside this startup interlock change.
            store.root = root;
            return Ok(store);
        }
        if path.try_exists()? {
            check_schema(schema_version(path)?, MigrationPolicy::Upgrade)?;
        }
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn, root, None, MigrationPolicy::Upgrade, target)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(
            Connection::open_in_memory()?,
            std::env::temp_dir().join("bazza-test"),
            None,
            MigrationPolicy::Upgrade,
            SchemaTarget::Latest,
        )
    }

    /// Service-aware existing open. No directory/database creation and no
    /// implicit upgrade under RequireCurrent. The guard stays inside the Store.
    pub fn open_existing_owned(guard: LibraryOwnerGuard, policy: MigrationPolicy) -> Result<Store> {
        Self::open_existing_owned_target(guard, policy, SchemaTarget::Latest)
    }

    fn open_existing_owned_target(
        guard: LibraryOwnerGuard,
        policy: MigrationPolicy,
        target: SchemaTarget,
    ) -> Result<Store> {
        owner::validate_existing_database(guard.root())?;
        let path = database_path(guard.root());
        check_schema(schema_version(&path)?, policy)?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let root = guard.root().to_path_buf();
        Self::init(conn, root, Some(guard), policy, target)
    }

    /// Explicit creation in a guarded existing directory. Reserve a new file
    /// atomically and never overwrite an existing library. A failed initialization
    /// leaves its newly created database for inspection instead of deleting data.
    pub fn create_owned(guard: LibraryOwnerGuard) -> Result<Store> {
        Self::create_owned_target(guard, SchemaTarget::Latest)
    }

    fn create_owned_target(guard: LibraryOwnerGuard, target: SchemaTarget) -> Result<Store> {
        owner::validate_database_absent(guard.root())?;
        let path = database_path(guard.root());
        let reservation = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StoreError::LibraryExists
                } else {
                    error.into()
                }
            })?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let root = guard.root().to_path_buf();
        drop(reservation);
        Self::init(conn, root, Some(guard), MigrationPolicy::Upgrade, target)
    }

    fn init(
        conn: Connection,
        root: PathBuf,
        owner: Option<LibraryOwnerGuard>,
        policy: MigrationPolicy,
        target: SchemaTarget,
    ) -> Result<Store> {
        // Construct the lifetime owner before fallible initialization so errors
        // also close SQLite before releasing the OS ownership lock.
        let mut store = Store {
            conn: Mutex::new(conn),
            root,
            _owner: owner,
        };
        let conn = store.conn.get_mut();
        let current = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        check_schema(current, policy)?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )?;
        if policy == MigrationPolicy::Upgrade {
            match target {
                SchemaTarget::Latest => migrations::run(conn)?,
                SchemaTarget::Legacy => migrations::run_until(conn, target.version())?,
            }
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Fold the WAL into the main database file (before copying it elsewhere).
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .lock()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Write a consistent database snapshot, including committed WAL contents.
    /// The destination's parent must exist and its file must be new. The source
    /// stays open and is never renamed, even when another reader prevents a checkpoint.
    pub fn snapshot_to(&self, path: &Path) -> Result<()> {
        write_snapshot(path, |destination| {
            self.conn.lock().execute("VACUUM INTO ?1", [destination])
        })
    }

    /// Snapshot an unenrolled legacy library while its exclusive owner guard
    /// remains held. The source connection is read-only and runs no migrations.
    /// SQLite may update WAL coordination files while reading.
    pub fn snapshot_legacy_owned(guard: &LibraryOwnerGuard, path: &Path) -> Result<i64> {
        owner::refuse_service_identity(guard.root())?;
        owner::validate_existing_database(guard.root())?;
        let source = database_path(guard.root());
        let connection = Connection::open_with_flags(
            &source,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let version = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if !(1..=migrations::LEGACY_SCHEMA).contains(&version) {
            return Err(StoreError::Other(
                "adoption rehearsal requires a supported legacy schema".into(),
            ));
        }
        write_snapshot(path, |destination| {
            connection.execute("VACUUM INTO ?1", [destination])
        })?;
        Ok(version)
    }

    pub fn models_dir(&self) -> PathBuf {
        let d = self.root.join("models");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// `<root>/recordings/<meeting_id>`, created if missing.
    pub fn recordings_dir(&self, meeting_id: &str) -> PathBuf {
        let d = self.root.join("recordings").join(meeting_id);
        let _ = std::fs::create_dir_all(&d);
        d
    }
}

fn write_snapshot(path: &Path, copy: impl FnOnce(&str) -> rusqlite::Result<usize>) -> Result<()> {
    let destination = path
        .to_str()
        .ok_or_else(|| StoreError::Other("database snapshot path must be valid UTF-8".into()))?;
    // Reserve a fresh file. VACUUM INTO accepts the empty output and snapshots
    // the committed SQLite state, including WAL contents.
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Err(error) = copy(destination) {
        drop(output);
        let _ = std::fs::remove_file(path);
        return Err(error.into());
    }
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_library_is_process_local_and_invalid_values_fail_closed() {
        use std::ffi::OsStr;
        assert_eq!(explicit_data_dir(None), Ok(None));
        for invalid in ["", " ", "relative/library", ".", ".."] {
            assert!(explicit_data_dir(Some(OsStr::new(invalid))).is_err());
        }
        let base = tempfile::tempdir().unwrap();
        let absolute = base.path().join("new-isolated-library");
        assert_eq!(
            explicit_data_dir(Some(absolute.as_os_str())).unwrap(),
            Some(absolute.clone())
        );
        assert!(
            !absolute.exists(),
            "the pure selector must not create or open a library"
        );
        assert!(!base.path().join(POINTER_FILE).exists());
    }

    #[test]
    fn new_library_uses_bazza_database_and_reopens_saved_settings() {
        let base = tempfile::tempdir().unwrap();
        let current = base.path().join("Bazza");
        assert_eq!(resolve_data_dir(&current), current);
        assert_eq!(database_path(&current), current.join("bazza.db"));

        let store = Store::open(&database_path(&current)).unwrap();
        store.set_setting("library-marker", &true).unwrap();
        assert!(matches!(
            Store::open(&database_path(&current)),
            Err(StoreError::OwnerBusy)
        ));
        drop(store);
        let reopened = Store::open(&database_path(&resolve_data_dir(&current))).unwrap();
        assert_eq!(
            reopened.get_setting::<bool>("library-marker").unwrap(),
            Some(true)
        );
        assert!(current.join("bazza.db").is_file());
    }

    #[test]
    fn bazza_pointer_selects_existing_directory_and_invalid_pointer_uses_default() {
        let base = tempfile::tempdir().unwrap();
        let current = base.path().join("Bazza");
        let redirected = base.path().join("custom-library");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&redirected).unwrap();
        assert_eq!(resolve_data_dir(&current), current);
        std::fs::write(
            current.join(POINTER_FILE),
            format!("{}\n", redirected.display()),
        )
        .unwrap();
        assert_eq!(resolve_data_dir(&current), redirected);
        std::fs::write(
            current.join(POINTER_FILE),
            base.path().join("missing").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(resolve_data_dir(&current), current);
        std::fs::write(current.join(POINTER_FILE), " \n").unwrap();
        assert_eq!(resolve_data_dir(&current), current);
    }

    #[test]
    fn snapshot_includes_committed_wal_while_another_reader_blocks_checkpoint() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("source/bazza.db");
        let store = Store::open(&path).unwrap();
        store.set_setting("snapshot-marker", &"initial").unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let original: String = reader
            .query_row(
                "SELECT value FROM settings WHERE key='snapshot-marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        store
            .set_setting("snapshot-marker", &"latest committed value")
            .unwrap();
        {
            let connection = store.conn.lock();
            connection.busy_timeout(std::time::Duration::ZERO).unwrap();
            let busy: i64 = connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
                .unwrap();
            assert_eq!(busy, 1, "the reader must keep committed data in the WAL");
        }

        std::fs::create_dir(base.path().join("target")).unwrap();
        let snapshot = base.path().join("target/bazza.db");
        store.snapshot_to(&snapshot).unwrap();
        let copied = Store::open(&snapshot).unwrap();
        assert_eq!(
            copied
                .get_setting::<String>("snapshot-marker")
                .unwrap()
                .as_deref(),
            Some("latest committed value")
        );
        assert_eq!(
            store
                .get_setting::<String>("snapshot-marker")
                .unwrap()
                .as_deref(),
            Some("latest committed value")
        );
        let still_original: String = reader
            .query_row(
                "SELECT value FROM settings WHERE key='snapshot-marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            still_original, original,
            "snapshot must not disturb the other reader"
        );
        reader.execute_batch("ROLLBACK").unwrap();
        assert!(path.is_file());
        assert!(
            store.snapshot_to(&snapshot).is_err(),
            "an existing destination is never overwritten"
        );
        assert_eq!(
            copied
                .get_setting::<String>("snapshot-marker")
                .unwrap()
                .as_deref(),
            Some("latest committed value")
        );
    }
}

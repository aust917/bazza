//! Phase-0 lifecycle experiment. This is not a production service or MCP server.
//! Only an explicitly marked synthetic v8 fixture can be opened, read-only.

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub const MARKER_FILE: &str = ".bazza-synthetic-fixture";
pub const MARKER_CONTENT: &str = "bazza-mcp-v8-fixture-v1\n";
pub const STATUS_FILE: &str = ".bazza-service-probe.json";
pub const STOP_FILE: &str = ".bazza-service-probe-stop";
pub const MAX_LIFETIME_MS: u64 = 300_000;

fn persist_atomic(file: tempfile::NamedTempFile, target: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        persist_atomic_windows(file, target, |_| {}).map_err(Into::into)
    }
    #[cfg(not(windows))]
    {
        file.persist(target)?;
        Ok(())
    }
}

#[cfg(windows)]
const REPLACE_RETRY_BUDGET: Duration = Duration::from_millis(250);

/// Retain the same completed temporary file on transient Windows replacement
/// errors. The destination is never removed, opened for writing or truncated.
/// The observer is private and lets tests synchronize on an actual failed
/// replacement instead of guessing when a publishing thread has started.
#[cfg(windows)]
fn persist_atomic_windows(
    mut file: tempfile::NamedTempFile,
    target: &Path,
    mut observe_retry: impl FnMut(&std::io::Error),
) -> std::io::Result<()> {
    let deadline = Instant::now() + REPLACE_RETRY_BUDGET;
    loop {
        match file.persist(target) {
            Ok(_) => return Ok(()),
            Err(error) => {
                // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
                if !matches!(error.error.raw_os_error(), Some(5 | 32 | 33))
                    || Instant::now() >= deadline
                {
                    return Err(error.error);
                }
                observe_retry(&error.error);
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error.error);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
                if Instant::now() >= deadline {
                    return Err(error.error);
                }
                // PersistError returns ownership after failure. Reuse it rather
                // than rewriting bytes or creating another temporary file.
                file = error.file;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub probe_version: u32,
    pub process_id: u32,
    pub instance_id: String,
    pub schema_version: i64,
    pub heartbeat: u64,
    pub meeting_id: Option<String>,
    pub meeting_title: Option<String>,
    pub finished: bool,
}

/// Holds the canonical fixture lock and read-only connection for the whole run.
/// Production Store constructors deliberately remain untouched by this experiment.
pub struct FixtureOwner {
    root: PathBuf,
    conn: Connection,
    // Fields drop in declaration order: close SQLite before releasing ownership.
    _lock: File,
}

fn regular_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("FIXTURE_REGULAR_FILE_REQUIRED");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("FIXTURE_REPARSE_POINT_FORBIDDEN");
        }
    }
    Ok(())
}

fn bounded_text(path: &Path, max: u64) -> Result<String> {
    regular_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("FIXTURE_FILE_TOO_LARGE");
    }
    Ok(String::from_utf8(bytes)?)
}

pub fn fixture_root(root: &Path) -> Result<PathBuf> {
    // Do not call Store::data_dir/open: they create directories and run migrations.
    let root = root.canonicalize().context("FIXTURE_UNAVAILABLE")?;
    if !root.is_dir()
        || bounded_text(&root.join(MARKER_FILE), 128).ok().as_deref() != Some(MARKER_CONTENT)
    {
        bail!("SYNTHETIC_FIXTURE_REQUIRED");
    }
    regular_file(&root.join("bazza.db"))?;
    Ok(root)
}

impl FixtureOwner {
    pub fn open(root: &Path) -> Result<Self> {
        let root = fixture_root(root)?;
        let lock_path = root.join(".bazza-service-probe.lock");
        if lock_path.symlink_metadata().is_ok() {
            regular_file(&lock_path)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock().context("FIXTURE_OWNER_BUSY")?;
        let conn = Connection::open_with_flags(
            root.join("bazza.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let schema: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if schema != 8 {
            bail!("FIXTURE_SCHEMA_MISMATCH: this probe requires frozen v8");
        }
        conn.execute_batch("PRAGMA query_only=ON;")?;
        Ok(Self {
            root,
            _lock: lock,
            conn,
        })
    }

    fn status(&self, instance_id: &str, heartbeat: u64, finished: bool) -> Result<Status> {
        use rusqlite::OptionalExtension;
        let meeting: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT substr(id,1,128), substr(title,1,256) FROM meetings ORDER BY id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (meeting_id, meeting_title) = match meeting {
            Some((id, title)) => (Some(id), Some(title)),
            None => (None, None),
        };
        Ok(Status {
            probe_version: 1,
            process_id: std::process::id(),
            instance_id: instance_id.into(),
            schema_version: 8,
            heartbeat,
            meeting_id,
            meeting_title,
            finished,
        })
    }

    fn publish(&self, status: &Status) -> Result<()> {
        // Same-directory atomic replacement: readers never observe partial JSON.
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(&mut file, status)?;
        file.flush()?;
        file.as_file().sync_all()?;
        persist_atomic(file, &self.root.join(STATUS_FILE))?;
        Ok(())
    }
}

pub fn read_status(root: &Path) -> Result<Status> {
    let root = fixture_root(root)?;
    regular_file(&root.join(STATUS_FILE))?;
    let file = File::open(root.join(STATUS_FILE))?;
    // A status snapshot is evidence only; it is not authenticated discovery.
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        bail!("STATUS_TOO_LARGE");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// An instance-bound stop cannot accidentally stop a later restarted probe.
pub fn request_stop(root: &Path) -> Result<()> {
    let root = fixture_root(root)?;
    let status = read_status(&root)?;
    if status.finished {
        return Ok(());
    }
    if root.join(STOP_FILE).symlink_metadata().is_ok() {
        regular_file(&root.join(STOP_FILE))?;
    }
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    file.write_all(status.instance_id.as_bytes())?;
    file.flush()?;
    persist_atomic(file, &root.join(STOP_FILE))?;
    Ok(())
}

pub fn run(root: &Path, lifetime: Duration) -> Result<()> {
    run_with_instance(root, lifetime, uuid::Uuid::new_v4())
}

/// The broker experiment pins a fresh public nonce to its exact task action.
/// This distinguishes its process from another owner of the same fixture.
pub fn run_with_instance(root: &Path, lifetime: Duration, instance: uuid::Uuid) -> Result<()> {
    if lifetime.is_zero() || lifetime.as_millis() > MAX_LIFETIME_MS as u128 {
        bail!("INVALID_LIFETIME");
    }
    let owner = FixtureOwner::open(root)?;
    let instance_id = instance.to_string();
    let started = Instant::now();
    let mut heartbeat = 0;
    loop {
        let stopped = bounded_text(&owner.root.join(STOP_FILE), 128)
            .ok()
            .is_some_and(|intent| intent == instance_id);
        let finished = started.elapsed() >= lifetime || stopped;
        owner.publish(&owner.status(&instance_id, heartbeat, finished)?)?;
        if finished {
            return Ok(());
        }
        heartbeat += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fail closed if the host disallows breakaway. Do not silently substitute a
/// normal child process and claim it has an independent service lifetime.
pub fn launch(executable: &Path, root: &Path, lifetime: Duration) -> Result<Child> {
    if lifetime.is_zero() || lifetime.as_millis() > MAX_LIFETIME_MS as u128 {
        bail!("INVALID_LIFETIME");
    }
    let root = fixture_root(root)?;
    let mut command = Command::new(executable);
    command
        .arg("run")
        .arg("--fixture")
        .arg(root)
        .arg("--lifetime-ms")
        .arg(lifetime.as_millis().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_detached(&mut command)?;
    command.spawn().context("DETACHED_LAUNCH_UNAVAILABLE")
}

#[cfg(windows)]
fn configure_detached(command: &mut Command) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, DETACHED_PROCESS};
    // DETACHED_PROCESS has no console. CREATE_NO_WINDOW is ignored with it.
    command.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
    Ok(())
}

#[cfg(unix)]
fn configure_detached(command: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // SAFETY: the child hook calls only the async-signal-safe setsid syscall.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(any(windows, unix)))]
fn configure_detached(_: &mut Command) -> Result<()> {
    bail!("DETACHED_LAUNCH_UNSUPPORTED")
}

#[cfg(all(test, windows))]
#[path = "publish_tests.rs"]
mod publish_tests;

//! Disposable phase-0 experiments. A passing tree-kill experiment classifies this
//! host's behavior; it does not certify every MCP host or a production service.

use bazza_service_probe::{
    fixture_root, read_status, request_stop, run, FixtureOwner, Status, MARKER_FILE, STATUS_FILE,
};
use bazza_service_test_support::{create_v8, READY_MEETING_ID};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const CHILD_LIFETIME_MS: u64 = 2_000;

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    instance: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("synthetic-library");
        create_v8(&root).unwrap();
        Self {
            _directory: directory,
            root,
            instance: None,
        }
    }

    fn remember(&mut self, status: &Status) {
        self.instance = Some(status.instance_id.clone());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The fixture and instance came from this test, never from production
        // discovery. Do not signal a replaced instance or act on a stale PID.
        if let Some(instance) = &self.instance {
            if read_status(&self.root)
                .ok()
                .is_some_and(|status| &status.instance_id == instance)
            {
                let _ = request_stop(&self.root);
            }
            // Every probe has a two-second hard lifetime. Wait for the lock before
            // TempDir cleanup so Windows has time to release open SQLite handles.
            let _ = wait_for(Duration::from_secs(3), || {
                FixtureOwner::open(&self.root).is_ok()
            });
        }
    }
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_bazza-service-probe"))
}

fn wait_for(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_launcher(fixture: &mut Fixture, hold_parent: bool) -> Option<(OwnedChild, Status)> {
    let mut command = Command::new(binary());
    command
        .args(["launch", "--fixture"])
        .arg(&fixture.root)
        .args(["--lifetime-ms", &CHILD_LIFETIME_MS.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if hold_parent {
        command.arg("--hold-parent");
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = sender.send(result);
    });
    // CLI startup itself has a five-second deadline. A failed startup emits no
    // status; inspect the actual child error rather than treating it as survival.
    let line = receiver
        .recv_timeout(Duration::from_secs(6))
        .expect("launcher output deadline")
        .unwrap();
    reader.join().unwrap();
    if line.is_empty() {
        let output = child.0.stderr.take().unwrap();
        let mut diagnostic = String::new();
        use std::io::Read;
        BufReader::new(output)
            .read_to_string(&mut diagnostic)
            .unwrap();
        assert!(wait_for(Duration::from_secs(1), || child
            .0
            .try_wait()
            .unwrap()
            .is_some()));
        assert!(!child.0.try_wait().unwrap().unwrap().success());
        #[cfg(windows)]
        if diagnostic.contains("DETACHED_LAUNCH_UNAVAILABLE") && diagnostic.contains("os error 5") {
            eprintln!("LIFECYCLE_EVIDENCE: this Windows host refused breakaway; a user-session broker is required. {diagnostic}");
            assert!(!fixture.root.join(STATUS_FILE).exists());
            return None;
        }
        panic!("detached launcher failed: {diagnostic}");
    }
    let status: Status = serde_json::from_str(&line).unwrap();
    assert!(!status.finished);
    assert_ne!(status.process_id, child.0.id());
    assert_eq!(status.meeting_id.as_deref(), Some(READY_MEETING_ID));
    fixture.remember(&status);
    Some((child, status))
}

#[test]
fn missing_unmarked_and_future_libraries_fail_without_creation_or_migration() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");
    assert!(fixture_root(&missing).is_err());
    assert!(!missing.exists());

    let fixture = Fixture::new();
    let database = fixture.root.join("bazza.db");
    let before = fs::read(&database).unwrap();
    fs::remove_file(fixture.root.join(MARKER_FILE)).unwrap();
    assert!(FixtureOwner::open(&fixture.root).is_err());
    assert_eq!(fs::read(&database).unwrap(), before);
    assert!(!fixture.root.join(".bazza-service-probe.lock").exists());

    let future = Fixture::new();
    let database = future.root.join("bazza.db");
    let connection = Connection::open(&database).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    let error = FixtureOwner::open(&future.root).err().unwrap();
    assert!(format!("{error:#}").contains("FIXTURE_SCHEMA_MISMATCH"));
    assert_eq!(fs::read(&database).unwrap(), before);
    assert!(!future.root.join(STATUS_FILE).exists());
}

#[test]
fn cli_requires_an_explicit_fixture_and_rejects_unbounded_lifetimes() {
    let output = Command::new(binary())
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("explicit --fixture is required"));
    let fixture = Fixture::new();
    assert!(run(&fixture.root, Duration::ZERO).is_err());
    assert!(run(&fixture.root, Duration::from_secs(301)).is_err());
    assert!(!fixture.root.join(STATUS_FILE).exists());
    fs::write(fixture.root.join(MARKER_FILE), vec![b'x'; 1_024]).unwrap();
    assert!(fixture_root(&fixture.root).is_err());
}

#[cfg(any(windows, unix))]
#[test]
fn fixture_marker_and_database_links_are_refused_without_following_targets() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as link_file;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as link_file;

    for name in [MARKER_FILE, "bazza.db"] {
        let fixture = Fixture::new();
        let original = fixture.root.join(name);
        let target = fixture._directory.path().join("unrelated-target");
        fs::rename(&original, &target).unwrap();
        let before = fs::read(&target).unwrap();
        if let Err(error) = link_file(&target, &original) {
            #[cfg(windows)]
            if error.raw_os_error() == Some(1314) {
                eprintln!("LIFECYCLE_EVIDENCE: Windows symlink fixture requires developer mode or privilege; this symlink case was not exercised.");
                continue;
            }
            panic!("could not create owned symlink fixture: {error}");
        }
        assert!(FixtureOwner::open(&fixture.root).is_err());
        assert_eq!(fs::read(&target).unwrap(), before);
        assert!(!fixture.root.join(STATUS_FILE).exists());
    }
}

#[test]
fn canonical_alias_has_one_owner_and_lock_is_released_on_close() {
    let fixture = Fixture::new();
    let owner = FixtureOwner::open(&fixture.root).unwrap();
    let alias = fixture.root.join("..").join("synthetic-library").join(".");
    assert_eq!(
        fixture_root(&alias).unwrap(),
        fixture.root.canonicalize().unwrap()
    );
    let error = FixtureOwner::open(&alias).err().unwrap();
    assert!(format!("{error:#}").contains("FIXTURE_OWNER_BUSY"));
    drop(owner);
    drop(FixtureOwner::open(&alias).unwrap());
}

#[test]
fn read_only_run_preserves_database_bytes_and_synthetic_payloads() {
    let fixture = Fixture::new();
    let database = fixture.root.join("bazza.db");
    let audio = fixture
        .root
        .join("recordings")
        .join(READY_MEETING_ID)
        .join("mix.wav");
    let before_database = fs::read(&database).unwrap();
    let before_audio = fs::read(&audio).unwrap();
    run(&fixture.root, Duration::from_millis(100)).unwrap();
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(fs::read(&audio).unwrap(), before_audio);
    let status = read_status(&fixture.root).unwrap();
    assert!(status.finished);
    assert_eq!(status.schema_version, 8);
    assert_eq!(status.meeting_id.as_deref(), Some(READY_MEETING_ID));
    assert_eq!(
        status.meeting_title.as_deref(),
        Some("Орион / Budget review")
    );
    let connection =
        Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
}

#[test]
fn direct_run_publishes_complete_json_while_readers_poll() {
    let fixture = Fixture::new();
    let mut child = OwnedChild(
        Command::new(binary())
            .args(["run", "--fixture"])
            .arg(&fixture.root)
            .args(["--lifetime-ms", "400"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert!(wait_for(Duration::from_secs(2), || fixture
        .root
        .join(STATUS_FILE)
        .exists()));
    let mut snapshots = 0;
    while child.0.try_wait().unwrap().is_none() {
        let status =
            read_status(&fixture.root).expect("atomic heartbeat must always be complete JSON");
        assert_eq!(status.process_id, child.0.id());
        snapshots += 1;
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(snapshots > 0);
    let exit = child.0.try_wait().unwrap().unwrap();
    if !exit.success() {
        use std::io::Read;
        let mut detail = String::new();
        if let Some(stderr) = child.0.stderr.take() {
            stderr.take(16_384).read_to_string(&mut detail).unwrap();
        }
        panic!("owned probe exited unsuccessfully ({exit}): {detail}");
    }
    assert!(read_status(&fixture.root).unwrap().finished);
}

#[test]
fn detached_launch_classifies_host_policy_and_when_available_checks_restart() {
    let mut fixture = Fixture::new();
    let Some((mut launcher, first)) = spawn_launcher(&mut fixture, false) else {
        return;
    };
    assert!(wait_for(Duration::from_secs(1), || launcher
        .0
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(launcher.0.try_wait().unwrap().unwrap().success());
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(|status| status.instance_id == first.instance_id
        && status.heartbeat > first.heartbeat
        && !status.finished)));
    request_stop(&fixture.root).unwrap();
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(|status| status.finished)));
    assert!(wait_for(Duration::from_secs(1), || FixtureOwner::open(
        &fixture.root
    )
    .is_ok()));

    // A stop request for the old instance must not terminate the next instance.
    let Some((mut next_launcher, next)) = spawn_launcher(&mut fixture, false) else {
        return;
    };
    assert_ne!(next.instance_id, first.instance_id);
    assert!(wait_for(Duration::from_secs(1), || next_launcher
        .0
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(|status| status.instance_id == next.instance_id
        && status.heartbeat > next.heartbeat
        && !status.finished)));
    request_stop(&fixture.root).unwrap();
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(|status| status.finished)));
    eprintln!("LIFECYCLE_EVIDENCE: detached probe survived normal launcher exit; explicit stop and instance-safe restart passed.");
}

#[test]
fn direct_child_stop_and_restart_ignore_an_old_instance_stop_intent() {
    let mut fixture = Fixture::new();
    let spawn = |root: &Path| {
        OwnedChild(
            Command::new(binary())
                .args(["run", "--fixture"])
                .arg(root)
                .args(["--lifetime-ms", "2000"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut first_child = spawn(&fixture.root);
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(
        |status| status.process_id == first_child.0.id() && !status.finished
    )));
    let first = read_status(&fixture.root).unwrap();
    fixture.remember(&first);
    request_stop(&fixture.root).unwrap();
    assert!(wait_for(Duration::from_secs(1), || first_child
        .0
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(first_child.0.try_wait().unwrap().unwrap().success());
    assert!(read_status(&fixture.root).unwrap().finished);

    let mut second_child = spawn(&fixture.root);
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(
        |status| status.instance_id != first.instance_id && !status.finished
    )));
    let second = read_status(&fixture.root).unwrap();
    fixture.remember(&second);
    assert!(wait_for(Duration::from_secs(1), || read_status(
        &fixture.root
    )
    .ok()
    .is_some_and(|status| status.instance_id == second.instance_id
        && status.heartbeat > second.heartbeat
        && !status.finished)));
    request_stop(&fixture.root).unwrap();
    assert!(wait_for(Duration::from_secs(1), || second_child
        .0
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(second_child.0.try_wait().unwrap().unwrap().success());
}

#[cfg(windows)]
#[test]
fn windows_owned_launcher_tree_kill_classifies_parentage_limitation() {
    use std::os::windows::process::CommandExt;

    let mut fixture = Fixture::new();
    let Some((mut launcher, first)) = spawn_launcher(&mut fixture, true) else {
        return;
    };
    // A running launcher owns this exact child relationship. Never enumerate or
    // kill by image name, another test's PID, or a status file from a user library.
    assert!(launcher.0.try_wait().unwrap().is_none());
    let started = Instant::now();
    let taskkill = Command::new("taskkill.exe")
        .args(["/PID", &launcher.0.id().to_string(), "/T", "/F"])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        taskkill.status.success(),
        "owned launcher taskkill failed: {}",
        String::from_utf8_lossy(&taskkill.stderr)
    );
    assert!(wait_for(Duration::from_secs(1), || launcher
        .0
        .try_wait()
        .unwrap()
        .is_some()));

    // Compare against a snapshot taken after taskkill returns: a final heartbeat
    // written while taskkill was still traversing the tree is not survival.
    let after_kill = read_status(&fixture.root).unwrap();
    let advanced = wait_for(Duration::from_millis(200), || {
        read_status(&fixture.root).ok().is_some_and(|status| {
            status.instance_id == first.instance_id
                && status.heartbeat > after_kill.heartbeat
                && !status.finished
        })
    });
    if advanced {
        eprintln!("LIFECYCLE_EVIDENCE: this Windows host's taskkill /T left the detached probe alive. This is host-specific evidence, not a universal process-tree guarantee.");
        request_stop(&fixture.root).unwrap();
    } else {
        assert!(wait_for(Duration::from_secs(3), || FixtureOwner::open(
            &fixture.root
        )
        .is_ok()));
        let after = read_status(&fixture.root).unwrap();
        if started.elapsed() < Duration::from_millis(CHILD_LIFETIME_MS) && !after.finished {
            eprintln!("LIFECYCLE_EVIDENCE: Windows taskkill /T stopped the detached probe through parentage; breakaway flags alone are insufficient and a user-session broker is required.");
        } else {
            eprintln!("LIFECYCLE_EVIDENCE: tree-kill experiment outlasted the bounded probe lifetime; no survival claim can be made from this run.");
        }
    }
    assert!(wait_for(Duration::from_secs(3), || FixtureOwner::open(
        &fixture.root
    )
    .is_ok()));
    // The disposable library remains usable even if taskkill killed its owner.
    run(&fixture.root, Duration::from_millis(50)).unwrap();
    assert!(read_status(&fixture.root).unwrap().finished);
}

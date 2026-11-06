use super::*;
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library Орион");
        fs::create_dir(&root).unwrap();
        Self {
            _temporary: temporary,
            root,
        }
    }
    fn gate(&self, id: &str) -> PathBuf {
        self.root.join(GATE_DIRECTORY).join(gate_name(id).unwrap())
    }
}

fn busy<T>(value: Result<T>) {
    assert!(matches!(value, Err(MediaGateError::Busy)));
}

#[test]
fn shared_readers_hold_exclusive_purge_until_the_last_reader_closes() {
    let fixture = Fixture::new();
    let first = MediaReadGuard::acquire(&fixture.root, "meeting").unwrap();
    let second = MediaReadGuard::acquire(&fixture.root, "meeting").unwrap();
    busy(MediaPurgeGuard::acquire(&fixture.root, "meeting"));
    busy(MediaPurgeGuard::acquire(
        &fixture.root.canonicalize().unwrap(),
        "meeting",
    ));
    drop(first);
    busy(MediaPurgeGuard::acquire(&fixture.root, "meeting"));
    drop(second);
    drop(MediaPurgeGuard::acquire(&fixture.root, "meeting").unwrap());
    assert!(fixture.gate("meeting").is_file());
    assert_eq!(fs::metadata(fixture.gate("meeting")).unwrap().len(), 0);
}

#[test]
fn exclusive_purge_blocks_readers_and_duplicate_purge_but_not_another_id() {
    let fixture = Fixture::new();
    let purge = MediaPurgeGuard::acquire(&fixture.root, "meeting").unwrap();
    busy(MediaReadGuard::acquire(&fixture.root, "meeting"));
    busy(MediaPurgeGuard::acquire(&fixture.root, "meeting"));
    drop(MediaReadGuard::acquire(&fixture.root, "different").unwrap());
    drop(MediaPurgeGuard::acquire(&fixture.root, "third").unwrap());
    drop(purge);
    drop(MediaReadGuard::acquire(&fixture.root, "meeting").unwrap());
}

#[test]
fn cloned_read_file_retains_the_gate_after_the_guard_is_dropped() {
    let fixture = Fixture::new();
    let guard = MediaReadGuard::acquire(&fixture.root, "meeting").unwrap();
    let file = guard.try_clone_file().unwrap();
    drop(guard);
    busy(MediaPurgeGuard::acquire(&fixture.root, "meeting"));
    drop(file);
    drop(MediaPurgeGuard::acquire(&fixture.root, "meeting").unwrap());
}

#[test]
fn a_read_clone_cannot_change_the_empty_gate_format() {
    let fixture = Fixture::new();
    let guard = MediaReadGuard::acquire(&fixture.root, "meeting").unwrap();
    let mut file = guard.try_clone_file().unwrap();
    assert!(file.write_all(b"not writable").is_err());
    assert_eq!(file.metadata().unwrap().len(), 0);
    drop(file);
    drop(guard);
    drop(MediaPurgeGuard::acquire(&fixture.root, "meeting").unwrap());
}

#[test]
fn simultaneous_exclusive_attempts_have_exactly_one_owner() {
    let fixture = Fixture::new();
    drop(MediaPurgeGuard::acquire(&fixture.root, "meeting").unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let children: Vec<_> = (0..2)
        .map(|_| {
            let root = fixture.root.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                let result = MediaPurgeGuard::acquire(&root, "meeting");
                // The winner stays open until both attempts have completed.
                barrier.wait();
                match result {
                    Ok(_) => true,
                    Err(MediaGateError::Busy) => false,
                    other => panic!("unexpected competing gate result: {other:?}"),
                }
            })
        })
        .collect();
    barrier.wait();
    barrier.wait();
    let owners = children
        .into_iter()
        .map(|child| usize::from(child.join().unwrap()))
        .sum::<usize>();
    assert_eq!(owners, 1);
    drop(MediaReadGuard::acquire(&fixture.root, "meeting").unwrap());
}

#[test]
fn exact_legacy_unicode_ids_are_hashed_without_path_or_normalization_semantics() {
    let fixture = Fixture::new();
    for (id, hash) in [
        (
            "abc",
            "4c30ebddc7e914cf362e0df2487c4c83b5b360def15bdfa4358c67d66f5ed770",
        ),
        (
            "legacy/id:Имя ?",
            "3f28c8527e7668f6fdaab3051b29164f9a78d20fd35d072d3e980043c99b1477",
        ),
        (
            "meeting-Орион",
            "2efdce6ab44947c8bde5f46240658836714ccbb4229595afc3c3eb6904eeccef",
        ),
        (
            "é",
            "52dc870081e665e4a737246e462bc6a15cfc4809b308a5dce9bc4122d9feabd9",
        ),
        (
            "e\u{301}",
            "3eedc607fb65016d2ef2217212174f0676f37f75fcc760fdf958d5e4d927929c",
        ),
    ] {
        assert_eq!(gate_name(id).unwrap(), format!("{hash}.gate"));
        drop(MediaReadGuard::acquire(&fixture.root, id).unwrap());
        assert!(fixture
            .root
            .join(GATE_DIRECTORY)
            .join(format!("{hash}.gate"))
            .is_file());
    }
    let parent = MediaPurgeGuard::acquire(&fixture.root, "..").unwrap();
    drop(MediaReadGuard::acquire(&fixture.root, ".").unwrap());
    drop(parent);
    assert!(!fixture.root.join("legacy").exists());
    assert_eq!(
        fs::read_dir(fixture.root.join(GATE_DIRECTORY))
            .unwrap()
            .count(),
        7
    );
}

#[test]
fn invalid_ids_create_nothing_and_the_bound_counts_utf8_bytes() {
    let fixture = Fixture::new();
    for id in [
        String::new(),
        "x".repeat(129),
        "я".repeat(65),
        "bad\0id".into(),
    ] {
        assert!(matches!(
            MediaReadGuard::acquire(&fixture.root, &id),
            Err(MediaGateError::Invalid(_))
        ));
    }
    assert!(!fixture.root.join(GATE_DIRECTORY).exists());
    drop(MediaReadGuard::acquire(&fixture.root, &"x".repeat(128)).unwrap());
    drop(MediaReadGuard::acquire(&fixture.root, &"я".repeat(64)).unwrap());
}

#[test]
fn absent_relative_or_non_directory_roots_are_not_created_or_changed() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("not created");
    assert!(MediaReadGuard::acquire(&missing, "meeting").is_err());
    assert!(!missing.exists());
    assert!(matches!(
        MediaReadGuard::acquire(Path::new("relative library"), "meeting"),
        Err(MediaGateError::Invalid(_))
    ));
    let file = fixture.root.join("root-file");
    fs::write(&file, b"retained root bytes").unwrap();
    assert!(matches!(
        MediaPurgeGuard::acquire(&file, "meeting"),
        Err(MediaGateError::Invalid(_))
    ));
    assert_eq!(fs::read(&file).unwrap(), b"retained root bytes");
    assert!(!fixture.root.join(GATE_DIRECTORY).exists());
}

#[test]
fn existing_gate_directory_and_nonempty_gate_entries_are_rejected_without_mutation() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join(GATE_DIRECTORY),
        b"retained directory obstruction",
    )
    .unwrap();
    assert!(matches!(
        MediaReadGuard::acquire(&fixture.root, "meeting"),
        Err(MediaGateError::Invalid(_))
    ));
    assert_eq!(
        fs::read(fixture.root.join(GATE_DIRECTORY)).unwrap(),
        b"retained directory obstruction"
    );

    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join(GATE_DIRECTORY)).unwrap();
    let path = fixture.gate("meeting");
    fs::write(&path, b"retained malformed gate bytes").unwrap();
    for result in [
        MediaReadGuard::acquire(&fixture.root, "meeting").map(drop),
        MediaPurgeGuard::acquire(&fixture.root, "meeting").map(drop),
    ] {
        assert!(matches!(result, Err(MediaGateError::Invalid(_))));
    }
    assert_eq!(fs::read(&path).unwrap(), b"retained malformed gate bytes");
    fs::create_dir(fixture.gate("directory-entry")).unwrap();
    assert!(matches!(
        MediaReadGuard::acquire(&fixture.root, "directory-entry"),
        Err(MediaGateError::Invalid(_))
    ));
    assert!(fixture.gate("directory-entry").is_dir());
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path, _directory: bool) -> bool {
    std::os::unix::fs::symlink(target, link).unwrap();
    true
}
#[cfg(windows)]
fn symlink(target: &Path, link: &Path, directory: bool) -> bool {
    let result = if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match result {
        Ok(()) => true,
        Err(error) if matches!(error.raw_os_error(), Some(5 | 1314)) => {
            eprintln!("synthetic symlink subcase unavailable without Windows symlink privilege");
            false
        }
        Err(error) => panic!("create owned synthetic symlink: {error}"),
    }
}

#[test]
fn root_directory_and_leaf_links_are_rejected_when_symlinks_are_available() {
    let fixture = Fixture::new();
    let root_link = fixture._temporary.path().join("linked-library");
    if !symlink(&fixture.root, &root_link, true) {
        return;
    }
    assert!(matches!(
        MediaReadGuard::acquire(&root_link, "meeting"),
        Err(MediaGateError::Invalid(_))
    ));
    assert!(!fixture.root.join(GATE_DIRECTORY).exists());
    let other = Fixture::new();
    assert!(symlink(
        &other.root,
        &fixture.root.join(GATE_DIRECTORY),
        true
    ));
    assert!(matches!(
        MediaPurgeGuard::acquire(&fixture.root, "meeting"),
        Err(MediaGateError::Invalid(_))
    ));
    assert_eq!(fs::read_dir(&other.root).unwrap().count(), 0);

    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join(GATE_DIRECTORY)).unwrap();
    let target = fixture.root.join("ordinary-file");
    fs::write(&target, []).unwrap();
    assert!(symlink(&target, &fixture.gate("meeting"), false));
    assert!(matches!(
        MediaReadGuard::acquire(&fixture.root, "meeting"),
        Err(MediaGateError::Invalid(_))
    ));
    assert!(fs::symlink_metadata(fixture.gate("meeting"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::metadata(target).unwrap().len(), 0);
}

struct OwnedChild(Child);
impl OwnedChild {
    fn stderr(&mut self) -> String {
        let mut bytes = Vec::new();
        if let Some(stderr) = self.0.stderr.take() {
            stderr.take(8192).read_to_end(&mut bytes).unwrap();
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "owned synthetic helper; invoked by inherited_read_file_holds_gate_in_owned_child"]
fn owned_child_entry() {
    let Some(ready) = std::env::var_os("BAZZA_MEDIA_GATE_TEST_READY") else {
        return;
    };
    let release = std::env::var_os("BAZZA_MEDIA_GATE_TEST_RELEASE").unwrap();
    #[cfg(windows)]
    let gate = {
        use std::os::windows::io::AsHandle;
        File::from(std::io::stdin().as_handle().try_clone_to_owned().unwrap())
    };
    #[cfg(unix)]
    let gate = {
        use std::os::fd::AsFd;
        File::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap())
    };
    assert!(gate.metadata().unwrap().is_file());
    assert_eq!(gate.metadata().unwrap().len(), 0);
    fs::write(ready, b"ready").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !Path::new(&release).exists() {
        assert!(
            Instant::now() < until,
            "owned read-gate child release timeout"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(gate);
}

#[test]
fn inherited_read_file_holds_gate_in_owned_child_after_parent_guard_drop() {
    let fixture = Fixture::new();
    let ready = fixture.root.join("child-ready");
    let release = fixture.root.join("child-release");
    let guard = MediaReadGuard::acquire(&fixture.root, "meeting").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::owned_child_entry",
            "--ignored",
            "--nocapture",
        ])
        .env("BAZZA_MEDIA_GATE_TEST_READY", &ready)
        .env("BAZZA_MEDIA_GATE_TEST_RELEASE", &release)
        .stdin(Stdio::from(guard.try_clone_file().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    // Command's configured Stdio may itself retain a parent handle. Remove it
    // before proving that only the running child's inherited reference pins it.
    drop(command);
    let until = Instant::now() + Duration::from_secs(3);
    while !ready.exists() {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!(
                "owned gate child exited before readiness: {status}; {}",
                child.stderr()
            );
        }
        assert!(Instant::now() < until, "owned gate child readiness timeout");
        thread::sleep(Duration::from_millis(10));
    }
    drop(guard);
    busy(MediaPurgeGuard::acquire(&fixture.root, "meeting"));
    drop(MediaReadGuard::acquire(&fixture.root, "meeting").unwrap());
    fs::write(release, b"release").unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "owned gate child failed: {status}; {}",
                child.stderr()
            );
            break;
        }
        assert!(Instant::now() < until, "owned gate child exit timeout");
        thread::sleep(Duration::from_millis(10));
    }
    child.0.wait().unwrap();
    drop(MediaPurgeGuard::acquire(&fixture.root, "meeting").unwrap());
}

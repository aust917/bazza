//! Deterministic Windows replacement failures. This controlled handle proves
//! retry behavior; it does not establish the cause of the lifecycle flake.
use super::*;
use std::{
    os::windows::fs::OpenOptionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

fn snapshot(heartbeat: u64) -> Status {
    Status {
        probe_version: 1,
        process_id: 42,
        instance_id: "synthetic-publish-instance".into(),
        schema_version: 8,
        heartbeat,
        meeting_id: Some("synthetic-meeting".into()),
        meeting_title: Some("Полный атомарный JSON ".repeat(12)),
        finished: false,
    }
}

fn completed_file(root: &Path, status: &Status) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new_in(root).unwrap();
    serde_json::to_writer(&mut file, status).unwrap();
    file.flush().unwrap();
    file.as_file().sync_all().unwrap();
    file
}

fn deny_delete(path: &Path) -> File {
    // FILE_SHARE_READ | FILE_SHARE_WRITE, deliberately without FILE_SHARE_DELETE.
    OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(path)
        .unwrap()
}

#[test]
fn atomic_publish_retries_observed_replacement_failure_then_succeeds_after_owned_release() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join(STATUS_FILE);
    let old = serde_json::to_vec(&snapshot(1)).unwrap();
    std::fs::write(&target, &old).unwrap();
    let blocked = deny_delete(&target);
    let next = snapshot(2);
    let expected = serde_json::to_vec(&next).unwrap();
    let file = completed_file(temp.path(), &next);
    let done = Arc::new(AtomicBool::new(false));
    let (observed_tx, observed_rx) = mpsc::sync_channel(1);
    let mut observed = false;
    std::thread::scope(|scope| {
        let release_done = done.clone();
        let target_ref = &target;
        let old_ref = &old;
        let release = scope.spawn(move || {
            let code = observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(code, Some(5 | 32 | 33)));
            assert!(!release_done.load(Ordering::SeqCst));
            assert_eq!(std::fs::read(target_ref).unwrap(), *old_ref);
            // This thread owns the sole no-delete handle and releases it only
            // after the publisher has reported a real bounded retry condition.
            drop(blocked);
        });
        persist_atomic_windows(file, &target, |error| {
            if !observed {
                observed = true;
                observed_tx.send(error.raw_os_error()).unwrap();
            }
        })
        .unwrap();
        done.store(true, Ordering::SeqCst);
        release.join().unwrap();
    });
    assert!(observed);
    let saved = std::fs::read(&target).unwrap();
    assert_eq!(saved, expected);
    assert_eq!(
        serde_json::from_slice::<Status>(&saved).unwrap().heartbeat,
        2
    );
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn persistent_replacement_lock_is_bounded_and_leaves_the_complete_old_json_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join(STATUS_FILE);
    let old = serde_json::to_vec(&snapshot(1)).unwrap();
    std::fs::write(&target, &old).unwrap();
    let blocked = deny_delete(&target);
    let file = completed_file(temp.path(), &snapshot(2));
    let started = Instant::now();
    let mut retries = 0;
    let error = persist_atomic_windows(file, &target, |_| retries += 1).unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(error.raw_os_error(), Some(5 | 32 | 33)));
    assert!(retries > 0);
    assert!(elapsed >= Duration::from_millis(200));
    // The retry algorithm's deadline is 250 ms; allow scheduler delay without
    // turning heavily parallel test runs into another timing-sensitive flake.
    assert!(
        elapsed < Duration::from_secs(2),
        "replacement took {elapsed:?}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), old);
    assert_eq!(
        serde_json::from_slice::<Status>(&std::fs::read(&target).unwrap())
            .unwrap()
            .heartbeat,
        1
    );
    drop(blocked);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

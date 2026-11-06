//! Controlled Windows replacement failures, not attribution of a past OS error.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::windows::fs::OpenOptionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

fn json(revision: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "revision": revision,
        "complete": "Полный атомарный JSON / fixture 🙂 ".repeat(50),
    }))
    .unwrap()
}

fn complete_file(root: &Path, bytes: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::new_in(root).unwrap();
    file.write_all(bytes).unwrap();
    file.flush().unwrap();
    file.as_file().sync_all().unwrap();
    file
}

fn no_delete_handle(target: &Path) -> File {
    // FILE_SHARE_READ | FILE_SHARE_WRITE, deliberately excluding DELETE.
    OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(target)
        .unwrap()
}

#[test]
fn observed_replacement_failure_retries_same_file_then_publishes_complete_json() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("discovery.json");
    let old = json(1);
    let next = json(2);
    fs::write(&target, &old).unwrap();
    let blocked = no_delete_handle(&target);
    let file = complete_file(temporary.path(), &next);
    let original_temporary_path = file.path().to_path_buf();
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
            assert_eq!(&fs::read(target_ref).unwrap(), old_ref);
            // Release this owned blocking handle only after the actual atomic
            // replacement has failed, without relying on a timing guess.
            drop(blocked);
        });
        persist_windows(file, &target, |error| {
            assert_eq!(fs::read(&original_temporary_path).unwrap(), next);
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
    assert_eq!(fs::read(&target).unwrap(), next);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&target).unwrap()).unwrap()
            ["revision"],
        2
    );
    assert!(!original_temporary_path.exists());
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

#[test]
fn persistent_replacement_lock_returns_bounded_error_and_preserves_old_json_and_temp_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("discovery.json");
    let old = json(1);
    fs::write(&target, &old).unwrap();
    let blocked = no_delete_handle(&target);
    let file = complete_file(temporary.path(), &json(2));
    let original_temporary_path = file.path().to_path_buf();
    let start = Instant::now();
    let mut retries = 0;
    let error = persist_windows(file, &target, |_| retries += 1).unwrap_err();
    let elapsed = start.elapsed();
    assert!(matches!(error.raw_os_error(), Some(5 | 32 | 33)));
    assert!(retries > 0);
    assert!(elapsed >= Duration::from_millis(200));
    assert!(elapsed < Duration::from_secs(2), "retry took {elapsed:?}");
    assert_eq!(fs::read(&target).unwrap(), old);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&target).unwrap()).unwrap()
            ["revision"],
        1
    );
    assert!(!original_temporary_path.exists());
    drop(blocked);
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

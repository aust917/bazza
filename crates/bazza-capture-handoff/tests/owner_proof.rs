#![cfg(any(windows, target_os = "macos"))]

use bazza_capture_handoff::{ArtifactGuard, CaptureIdentity, LOCK_FILE, OWNER_FILE};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, PathBuf, CaptureIdentity) {
    let root = tempfile::tempdir().unwrap();
    let identity = CaptureIdentity {
        library_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        worker_session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
    };
    let directory = root.path().join(identity.meeting_id.to_string());
    fs::create_dir(&directory).unwrap();
    (root, directory, identity)
}
fn metadata(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join(OWNER_FILE)).unwrap()).unwrap()
}
fn replace(directory: &Path, value: &Value) {
    fs::write(
        directory.join(OWNER_FILE),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn seal_failed(guard: &mut ArtifactGuard) {
    use bazza_capture_handoff::{
        CaptureTerminalStatus, ProviderProvenance, SttProvider, TerminalManifest, MANIFEST_VERSION,
    };
    guard
        .publish_terminal(&TerminalManifest {
            version: MANIFEST_VERSION,
            identity: guard.identity().clone(),
            provenance: ProviderProvenance {
                provider: SttProvider::Local,
                model_id: None,
            },
            status: CaptureTerminalStatus::Failed,
            duration_ms: 0,
            ended_at: "2026-11-02T00:00:00Z".parse().unwrap(),
            has_audio: false,
            has_video: false,
            last_confirmed_sequence: 0,
            error_code: Some("CAPTURE_FAILED".into()),
        })
        .unwrap();
}

#[test]
fn begin_admission_is_unsealed_only_but_control_proof_survives_a_terminal_seal() {
    let (_root, directory, identity) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    ArtifactGuard::verify_begin_owner(&directory, &identity, std::process::id()).unwrap();
    seal_failed(&mut guard);
    assert!(ArtifactGuard::verify_begin_owner(&directory, &identity, std::process::id()).is_err());
    ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
    drop(guard);
    let _next = ArtifactGuard::acquire(&directory, &identity).unwrap();
    assert!(ArtifactGuard::verify_begin_owner(&directory, &identity, std::process::id()).is_err());
    ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
}

#[test]
fn any_existing_terminal_entry_denies_begin_without_mutating_capture_artifacts() {
    for directory_entry in [false, true] {
        let (_root, directory, identity) = fixture();
        let _guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
        let path = directory.join(bazza_capture_handoff::TERMINAL_FILE);
        if directory_entry {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"malformed terminal").unwrap();
        }
        assert!(
            ArtifactGuard::verify_begin_owner(&directory, &identity, std::process::id()).is_err()
        );
        ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
        if directory_entry {
            assert!(path.is_dir());
        } else {
            assert_eq!(fs::read(path).unwrap(), b"malformed terminal");
        }
    }
}

#[test]
fn proof_request_cannot_claim_that_a_sealed_capture_is_unsealed() {
    let (_root, directory, identity) = fixture();
    runtime().block_on(async {
        let mut guard=ArtifactGuard::acquire(&directory,&identity).unwrap();
        seal_failed(&mut guard);
        let owner=metadata(&directory);
        let endpoint=owner["proof_endpoint"].as_str().unwrap();
        let mut stream=bazza_service_transport::connect(endpoint,std::process::id()).await.unwrap();
        let forged=serde_json::json!({"version":1,"identity":identity.clone(),"nonce":owner["proof_nonce"],"challenge":Uuid::new_v4(),"capture_unsealed":true});
        bazza_service_transport::write_frame(&mut stream,&forged).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(500),bazza_service_transport::read_frame::<Value>(&mut stream)).await.unwrap().is_err());
        drop(stream);
        assert!(ArtifactGuard::verify_begin_owner(&directory,&identity,std::process::id()).is_err());
        ArtifactGuard::verify_owner(&directory,&identity,std::process::id()).unwrap();
    });
}

#[test]
fn kernel_pid_proof_ignores_forged_diagnostic_pid_and_checks_exact_identity() {
    let (_root, directory, identity) = fixture();
    let _guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
    let mut owner = metadata(&directory);
    let wrong_pid = std::process::id().wrapping_add(1).max(1);
    owner["owner_pid"] = wrong_pid.into();
    replace(&directory, &owner);
    assert!(ArtifactGuard::verify_owner(&directory, &identity, wrong_pid).is_err());
    // The diagnostic file cannot replace the kernel's process identity.
    ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
    let mut wrong_identity = identity.clone();
    wrong_identity.generation = Uuid::new_v4();
    assert!(ArtifactGuard::verify_owner(&directory, &wrong_identity, std::process::id()).is_err());
    let mut names: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    let mut expected = vec![
        std::ffi::OsString::from(LOCK_FILE),
        std::ffi::OsString::from(OWNER_FILE),
    ];
    expected.sort();
    assert_eq!(
        names, expected,
        "proof rendezvous must not create raw capture files"
    );
}

#[test]
fn stale_and_foreign_rendezvous_cannot_authorize_a_later_guard() {
    let (_root, directory, identity) = fixture();
    let guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    let stale = metadata(&directory);
    drop(guard);
    assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
    let _next = ArtifactGuard::acquire(&directory, &identity).unwrap();
    let current = metadata(&directory);
    assert_ne!(stale["proof_nonce"], current["proof_nonce"]);
    assert_ne!(stale["proof_endpoint"], current["proof_endpoint"]);
    replace(&directory, &stale);
    assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
    let (_other_root, other, other_identity) = fixture();
    let _other_guard = ArtifactGuard::acquire(&other, &other_identity).unwrap();
    let mut foreign = current.clone();
    foreign["proof_endpoint"] = metadata(&other)["proof_endpoint"].clone();
    replace(&directory, &foreign);
    assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
    replace(&directory, &current);
    ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
}

#[test]
fn busy_lock_before_owner_publication_does_not_reuse_old_metadata() {
    let (_root, directory, identity) = fixture();
    let guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    drop(guard);
    // Reproduce the acquisition/publication gap: the OS lock is busy, but the
    // old metadata/proof nonce is still present and its old server is stopped.
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join(LOCK_FILE))
        .unwrap();
    owner.try_lock().unwrap();
    assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
}

#[test]
fn oversized_and_partial_frames_are_bounded_and_drop_joins_before_unlock() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_root, directory, identity) = fixture();
    runtime().block_on(async {
        // Acquire/verify/drop remain usable inside an entered async runtime.
        let guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
        ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).unwrap();
        let endpoint = metadata(&directory)["proof_endpoint"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut oversized = bazza_service_transport::connect(&endpoint, std::process::id())
            .await
            .unwrap();
        oversized.write_u32(2049).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(500), oversized.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        drop(oversized);
        let mut partial = bazza_service_transport::connect(&endpoint, std::process::id())
            .await
            .unwrap();
        partial.write_u32(128).await.unwrap();
        partial.write_all(b"{").await.unwrap();
        let start = Instant::now();
        drop(guard);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "Drop waited on an incomplete peer frame"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(500), partial.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
        let _next = ArtifactGuard::acquire(&directory, &identity).unwrap();
    });
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn separate_process_owner_cannot_be_impersonated_by_editing_owner_json() {
    let (root, directory, identity) = fixture();
    let spec = root.path().join("child-spec.json");
    let ready = root.path().join("ready");
    fs::write(&spec, serde_json::to_vec(&identity).unwrap()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW for this owned helper.
    }
    let child = command
        .args(["--exact", "owner_process_helper", "--nocapture"])
        .env("BAZZA_HANDOFF_PROOF_TEST_SPEC", &spec)
        .env("BAZZA_HANDOFF_PROOF_TEST_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let child = OwnedChild(child);
    let until = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        ready.exists(),
        "owned proof test child did not become ready"
    );
    ArtifactGuard::verify_owner(&directory, &identity, child.0.id()).unwrap();
    let mut forged = metadata(&directory);
    forged["owner_pid"] = std::process::id().into();
    replace(&directory, &forged);
    assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
    ArtifactGuard::verify_owner(&directory, &identity, child.0.id()).unwrap();
    drop(child);
    assert!(ArtifactGuard::verify_owner(&directory, &identity, std::process::id()).is_err());
}

#[test]
fn owner_process_helper() {
    let Some(spec) = std::env::var_os("BAZZA_HANDOFF_PROOF_TEST_SPEC") else {
        return;
    };
    let spec = PathBuf::from(spec);
    let identity: CaptureIdentity = serde_json::from_slice(&fs::read(&spec).unwrap()).unwrap();
    let directory = spec.parent().unwrap().join(identity.meeting_id.to_string());
    let _guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    fs::write(
        std::env::var_os("BAZZA_HANDOFF_PROOF_TEST_READY").unwrap(),
        b"ready",
    )
    .unwrap();
    let mut input = [0u8; 1];
    let _ = std::io::stdin().read(&mut input);
}

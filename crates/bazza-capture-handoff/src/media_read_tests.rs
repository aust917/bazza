use super::*;
use crate::{
    read_terminal_snapshot, CaptureIdentity, CaptureTerminalStatus, ProviderProvenance,
    SttProvider, TerminalManifest, JOURNAL_FILE, MANIFEST_VERSION, MAX_MANIFEST_BYTES, OWNER_FILE,
    TERMINAL_FILE,
};
use bazza_media_gate::MediaPurgeGuard;
use std::time::Instant;

fn identity() -> MediaReadIdentity {
    MediaReadIdentity {
        library_id: Uuid::new_v4(),
        library_epoch: Uuid::new_v4(),
        worker_session_id: Uuid::new_v4(),
        claim_id: Uuid::new_v4(),
        job_id: "job-identity".into(),
        meeting_id: Uuid::new_v4(),
        capture_id: Uuid::new_v4(),
        capture_generation: Uuid::new_v4(),
        checkpoint_revision: "checkpoint-original".into(),
        lifecycle_revision: "life:original".into(),
        transcript_revision: "transcript.original".into(),
        source_id: Some("source-original".into()),
    }
}
fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
fn path(root: &Path, nonce: Uuid) -> PathBuf {
    metadata_path(&root.canonicalize().unwrap().join(GATE_DIRECTORY), nonce)
}
fn variants(original: &MediaReadIdentity) -> Vec<MediaReadIdentity> {
    (0..12)
        .map(|index| {
            let mut changed = original.clone();
            match index {
                0 => changed.library_id = Uuid::new_v4(),
                1 => changed.library_epoch = Uuid::new_v4(),
                2 => changed.worker_session_id = Uuid::new_v4(),
                3 => changed.claim_id = Uuid::new_v4(),
                4 => changed.job_id = "other-job".into(),
                5 => changed.meeting_id = Uuid::new_v4(),
                6 => changed.capture_id = Uuid::new_v4(),
                7 => changed.capture_generation = Uuid::new_v4(),
                8 => changed.lifecycle_revision = "other-life".into(),
                9 => changed.transcript_revision = "other-transcript".into(),
                10 => changed.source_id = None,
                11 => changed.checkpoint_revision = "other-checkpoint".into(),
                _ => unreachable!(),
            }
            changed
        })
        .collect()
}

#[test]
fn live_owner_proves_real_pid_and_identity_without_creating_recordings() {
    let root = root();
    let identity = identity();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    assert_eq!(owner.identity(), &identity);
    assert!(!owner.nonce().is_nil());
    verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).unwrap();
    let files = fs::read_dir(root.path())
        .unwrap()
        .collect::<io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name(), GATE_DIRECTORY);
    assert!(!root.path().join("recordings").exists());
    assert!(matches!(
        MediaPurgeGuard::acquire(root.path(), &identity.meeting_id.to_string()),
        Err(MediaGateError::Busy)
    ));
    let metadata = path(root.path(), owner.nonce());
    assert!(metadata.is_file());
    drop(owner);
    assert!(!metadata.exists());
    drop(MediaPurgeGuard::acquire(root.path(), &identity.meeting_id.to_string()).unwrap());
}

#[test]
fn checkpoint_is_required_and_legacy_media_proof_version_is_rejected() {
    let identity = identity();
    let mut legacy = serde_json::to_value(&identity).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("checkpoint_revision");
    assert!(serde_json::from_value::<MediaReadIdentity>(legacy).is_err());
    let root = root();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    let metadata_path = path(root.path(), owner.nonce());
    let original = fs::read(&metadata_path).unwrap();
    let mut metadata: Metadata = serde_json::from_slice(&original).unwrap();
    metadata.version = 1;
    fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).is_err());
    fs::write(metadata_path, original).unwrap();
    verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).unwrap();
}

#[test]
fn unchanged_checkpoint_reuses_owner_but_checkpoint_advance_requires_a_new_owner() {
    let root = root();
    let original = identity();
    let owner = MediaReadOwner::acquire(root.path(), &original).unwrap();
    // Job renewal keeps this identity unchanged; no job revision is carried.
    verify_owner(root.path(), &original, owner.nonce(), std::process::id()).unwrap();
    let mut next = original.clone();
    next.checkpoint_revision = "checkpoint-next".into();
    assert!(verify_owner(root.path(), &next, owner.nonce(), std::process::id()).is_err());
    let next_owner = MediaReadOwner::acquire(root.path(), &next).unwrap();
    assert_ne!(next_owner.nonce(), owner.nonce());
    verify_owner(root.path(), &next, next_owner.nonce(), std::process::id()).unwrap();
    verify_owner(root.path(), &original, owner.nonce(), std::process::id()).unwrap();
}

#[test]
fn invalid_identity_and_readonly_missing_verification_create_nothing() {
    let root = root();
    let original = identity();
    assert!(verify_owner(root.path(), &original, Uuid::new_v4(), std::process::id()).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    for index in 0..15 {
        let mut invalid = original.clone();
        match index {
            0 => invalid.library_id = Uuid::nil(),
            1 => invalid.library_epoch = Uuid::nil(),
            2 => invalid.worker_session_id = Uuid::nil(),
            3 => invalid.claim_id = Uuid::nil(),
            4 => invalid.meeting_id = Uuid::nil(),
            5 => invalid.capture_id = Uuid::nil(),
            6 => invalid.capture_generation = Uuid::nil(),
            7 => invalid.job_id = "../job".into(),
            8 => invalid.lifecycle_revision = "bad/revision".into(),
            9 => invalid.transcript_revision = "x".repeat(129),
            10 => invalid.source_id = Some("bad source".into()),
            11 => invalid.job_id = "job\0id".into(),
            12 => invalid.checkpoint_revision = String::new(),
            13 => invalid.checkpoint_revision = "bad/checkpoint".into(),
            14 => invalid.checkpoint_revision = "x".repeat(129),
            _ => unreachable!(),
        }
        assert!(MediaReadOwner::acquire(root.path(), &invalid).is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
    let missing = root.path().join("missing-library");
    assert!(MediaReadOwner::acquire(&missing, &original).is_err());
    assert!(!missing.exists());
}

#[test]
fn wrong_pid_nonce_and_every_identity_pin_fail_against_actual_server() {
    let root = root();
    let identity = identity();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    assert!(verify_owner(root.path(), &identity, owner.nonce(), 0).is_err());
    assert!(verify_owner(root.path(), &identity, owner.nonce(), u32::MAX).is_err());
    assert!(verify_owner(root.path(), &identity, Uuid::nil(), std::process::id()).is_err());
    assert!(verify_owner(root.path(), &identity, Uuid::new_v4(), std::process::id()).is_err());
    let metadata_path = path(root.path(), owner.nonce());
    let original = fs::read(&metadata_path).unwrap();
    // Make metadata agree with the forged request. Only the actual server's
    // complete identity comparison can reject these otherwise coherent frames.
    for changed in variants(&identity) {
        let mut metadata: Metadata = serde_json::from_slice(&original).unwrap();
        metadata.identity = changed.clone();
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert!(verify_owner(root.path(), &changed, owner.nonce(), std::process::id()).is_err());
    }
    fs::write(&metadata_path, original).unwrap();
    verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).unwrap();
}

#[test]
fn forged_endpoint_unknown_pid_field_and_oversized_metadata_are_rejected() {
    let root = root();
    let identity = identity();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    let metadata_path = path(root.path(), owner.nonce());
    let original = fs::read(&metadata_path).unwrap();
    let mut metadata: Metadata = serde_json::from_slice(&original).unwrap();
    metadata.endpoint = endpoint(&root.path().canonicalize().unwrap(), Uuid::new_v4()).unwrap();
    fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).is_err());
    let mut metadata: serde_json::Value = serde_json::from_slice(&original).unwrap();
    metadata["owner_pid"] = serde_json::json!(std::process::id());
    fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).is_err());
    fs::write(&metadata_path, vec![b'x'; FRAME_LIMIT + 1]).unwrap();
    assert!(matches!(
        verify_owner(root.path(), &identity, owner.nonce(), std::process::id()),
        Err(HandoffError::TooLarge)
    ));
    fs::write(&metadata_path, original).unwrap();
    verify_owner(root.path(), &identity, owner.nonce(), std::process::id()).unwrap();
}

#[test]
fn dropped_proof_cannot_be_resurrected_by_stale_metadata_while_clone_retains_gate() {
    let root = root();
    let identity = identity();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    let nonce = owner.nonce();
    let metadata_path = path(root.path(), nonce);
    let metadata = fs::read(&metadata_path).unwrap();
    let clone = owner.try_clone_file().unwrap();
    drop(owner);
    assert!(!metadata_path.exists());
    fs::write(&metadata_path, &metadata).unwrap();
    assert!(verify_owner(root.path(), &identity, nonce, std::process::id()).is_err());
    assert!(matches!(
        MediaPurgeGuard::acquire(root.path(), &identity.meeting_id.to_string()),
        Err(MediaGateError::Busy)
    ));
    let next = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    assert_ne!(next.nonce(), nonce);
    verify_owner(root.path(), &identity, next.nonce(), std::process::id()).unwrap();
    drop(next);
    assert_eq!(
        fs::read(&metadata_path).unwrap(),
        metadata,
        "another nonce's stale file is not removed"
    );
    drop(clone);
    drop(MediaPurgeGuard::acquire(root.path(), &identity.meeting_id.to_string()).unwrap());
}

#[test]
fn cleanup_does_not_unlink_another_file_replacing_its_nonce_path() {
    let root = root();
    let identity = identity();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    let metadata = path(root.path(), owner.nonce());
    let moved = metadata.with_extension("retained");
    fs::rename(&metadata, &moved).unwrap();
    fs::write(&metadata, b"retained replacement bytes").unwrap();
    drop(owner);
    assert_eq!(fs::read(&metadata).unwrap(), b"retained replacement bytes");
    assert!(moved.is_file());
    drop(MediaPurgeGuard::acquire(root.path(), &identity.meeting_id.to_string()).unwrap());
}

#[test]
fn oversized_and_partial_frames_are_bounded_and_drop_joins_before_gate_release() {
    let root = root();
    let identity = identity();
    let owner = MediaReadOwner::acquire(root.path(), &identity).unwrap();
    let nonce = owner.nonce();
    let derived = endpoint(&root.path().canonicalize().unwrap(), nonce).unwrap();
    runtime().unwrap().block_on(async {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(1),
            bazza_service_transport::connect(&derived, std::process::id()),
        )
        .await
        .unwrap()
        .unwrap();
        stream.write_u32(FRAME_LIMIT as u32 + 1).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), stream.read_u8())
                .await
                .unwrap()
                .is_err()
        );
    });
    runtime().unwrap().block_on(async {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(1),
            bazza_service_transport::connect(&derived, std::process::id()),
        )
        .await
        .unwrap()
        .unwrap();
        stream.write_u32(1024).await.unwrap();
        stream.write_all(b"{").await.unwrap();
        let before = Instant::now();
        drop(owner);
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), stream.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        drop(stream);
        let late = tokio::time::timeout(
            Duration::from_millis(300),
            bazza_service_transport::connect(&derived, std::process::id()),
        )
        .await;
        assert!(
            !matches!(late, Ok(Ok(_))),
            "proof endpoint must stop before owner Drop returns"
        );
    });
    assert!(!path(root.path(), nonce).exists());
    drop(MediaPurgeGuard::acquire(root.path(), &identity.meeting_id.to_string()).unwrap());
}

fn capture_identity() -> CaptureIdentity {
    CaptureIdentity {
        library_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        worker_session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
    }
}
fn terminal(identity: &CaptureIdentity) -> TerminalManifest {
    TerminalManifest {
        version: MANIFEST_VERSION,
        identity: identity.clone(),
        provenance: ProviderProvenance {
            provider: SttProvider::Local,
            model_id: Some("fixture-local".into()),
        },
        duration_ms: 1000,
        ended_at: "2026-11-02T00:00:02Z".parse().unwrap(),
        status: CaptureTerminalStatus::Completed,
        error_code: None,
        has_audio: true,
        has_video: false,
        last_confirmed_sequence: 7,
    }
}
fn directory_bytes(directory: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut entries = fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn terminal_snapshot_is_bounded_readonly_and_does_not_parse_journal_or_publish_owner() {
    let root = root();
    let identity = capture_identity();
    let directory = root.path().join(identity.meeting_id.to_string());
    fs::create_dir(&directory).unwrap();
    let terminal = terminal(&identity);
    fs::write(
        directory.join(TERMINAL_FILE),
        serde_json::to_vec(&terminal).unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join(JOURNAL_FILE),
        b"deliberately unreadable journal",
    )
    .unwrap();
    fs::write(
        directory.join(OWNER_FILE),
        b"retained stale capture owner bytes",
    )
    .unwrap();
    let before = directory_bytes(&directory);
    assert_eq!(
        read_terminal_snapshot(&directory, &identity).unwrap(),
        terminal
    );
    assert_eq!(directory_bytes(&directory), before);
    assert!(!directory.join(crate::LOCK_FILE).exists());
    assert!(
        !directory.join("mix.wav").exists(),
        "snapshot validates terminal shape, not media integrity"
    );
}

#[test]
fn missing_terminal_snapshot_never_creates_directory_or_artifacts() {
    let root = root();
    let identity = capture_identity();
    let directory = root.path().join(identity.meeting_id.to_string());
    assert!(read_terminal_snapshot(&directory, &identity).is_err());
    assert!(!directory.exists());
    fs::create_dir(&directory).unwrap();
    assert!(
        matches!(read_terminal_snapshot(&directory, &identity), Err(HandoffError::Io(error)) if error.kind()==io::ErrorKind::NotFound)
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
}

#[test]
fn malformed_terminal_snapshot_shapes_fail_without_rewriting_bytes() {
    let root = root();
    let identity = capture_identity();
    let directory = root.path().join(identity.meeting_id.to_string());
    fs::create_dir(&directory).unwrap();
    for index in 0..10 {
        let mut terminal = terminal(&identity);
        match index {
            0 => terminal.version += 1,
            1 => terminal.identity.generation = Uuid::new_v4(),
            2 => terminal.provenance.model_id = Some("../secret".into()),
            3 => terminal.duration_ms = crate::MAX_TIME_MS + 1,
            4 => terminal.has_audio = false,
            5 => terminal.error_code = Some("CAPTURE_FAILED".into()),
            6 => {
                terminal.status = CaptureTerminalStatus::Failed;
                terminal.error_code = Some("bad/path".into());
            }
            7 => {
                terminal.status = CaptureTerminalStatus::Failed;
                terminal.error_code = Some(String::new());
            }
            8 => terminal.ended_at = "1969-12-31T23:59:59Z".parse().unwrap(),
            9 => terminal.last_confirmed_sequence = crate::MAX_RECORDS as u64 + 1,
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&terminal).unwrap();
        fs::write(directory.join(TERMINAL_FILE), &bytes).unwrap();
        assert!(read_terminal_snapshot(&directory, &identity).is_err());
        assert_eq!(fs::read(directory.join(TERMINAL_FILE)).unwrap(), bytes);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }
    for bytes in [b"{malformed".to_vec(), vec![b'x'; MAX_MANIFEST_BYTES + 1]] {
        fs::write(directory.join(TERMINAL_FILE), &bytes).unwrap();
        assert!(read_terminal_snapshot(&directory, &identity).is_err());
        assert_eq!(fs::read(directory.join(TERMINAL_FILE)).unwrap(), bytes);
    }
}

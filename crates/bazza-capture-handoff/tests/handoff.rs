use bazza_capture_handoff::*;
use std::{fs, io::Write, path::PathBuf};
use uuid::Uuid;

fn identity() -> CaptureIdentity {
    CaptureIdentity {
        library_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        worker_session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
    }
}
fn provenance() -> ProviderProvenance {
    ProviderProvenance {
        provider: SttProvider::Soniox,
        model_id: Some("stt-rt-v4".into()),
    }
}
fn caption(sequence: u64) -> ConfirmedCaption {
    ConfirmedCaption {
        sequence,
        start_ms: (sequence - 1) * 1000,
        end_ms: sequence * 1000,
        speaker: Some("Анна".into()),
        text: format!("Подтверждённая реплика {sequence}"),
        provenance: provenance(),
    }
}
fn fixture() -> (tempfile::TempDir, PathBuf, CaptureIdentity) {
    let temp = tempfile::tempdir().unwrap();
    let identity = identity();
    let directory = temp.path().join(identity.meeting_id.to_string());
    fs::create_dir(&directory).unwrap();
    (temp, directory, identity)
}
fn manifest(id: &CaptureIdentity, last: u64) -> TerminalManifest {
    TerminalManifest {
        version: MANIFEST_VERSION,
        identity: id.clone(),
        provenance: provenance(),
        duration_ms: last * 1000,
        ended_at: chrono::Utc::now(),
        status: CaptureTerminalStatus::Failed,
        error_code: None,
        has_audio: false,
        has_video: false,
        last_confirmed_sequence: last,
    }
}

#[test]
fn live_owner_beats_recovery_and_lock_inode_is_retained_after_release() {
    let (_temp, directory, id) = fixture();
    let guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    // A service heartbeat can expire; the independent OS owner remains decisive.
    assert!(matches!(
        ArtifactGuard::acquire(&directory, &id),
        Err(HandoffError::Busy)
    ));
    assert_eq!(
        ArtifactGuard::probe(&directory, &id).unwrap(),
        OwnerState::Busy {
            owner_pid: std::process::id()
        }
    );
    let mut wrong = id.clone();
    wrong.generation = Uuid::new_v4();
    assert!(matches!(
        ArtifactGuard::probe(&directory, &wrong),
        Err(HandoffError::IdentityMismatch)
    ));
    assert!(!directory.join(JOURNAL_FILE).exists());
    drop(guard);
    assert!(directory.join(LOCK_FILE).is_file());
    assert_eq!(
        ArtifactGuard::probe(&directory, &id).unwrap(),
        OwnerState::Free
    );
    let recovery = ArtifactGuard::acquire(&directory, &id).unwrap();
    assert!(recovery.confirmed().is_empty());
}

#[test]
fn sealed_scan_uses_existing_lock_and_leaves_artifacts_untouched() {
    let (_temp, directory, id) = fixture();
    assert!(matches!(
        SealedCaptureGuard::acquire(&directory, &id),
        Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    writer.append_confirmed(&caption(1)).unwrap();
    assert!(matches!(
        SealedCaptureGuard::acquire(&directory, &id),
        Err(HandoffError::Busy)
    ));
    drop(writer);
    let owner = directory.join(OWNER_FILE);
    let owner_bytes = fs::read(&owner).unwrap();
    assert!(matches!(
        SealedCaptureGuard::acquire(&directory, &id),
        Err(HandoffError::MissingTerminal)
    ));
    assert_eq!(fs::read(&owner).unwrap(), owner_bytes);

    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    writer.append_confirmed(&caption(2)).unwrap();
    let terminal = manifest(&id, 2);
    writer.publish_terminal(&terminal).unwrap();
    drop(writer);
    let journal = directory.join(JOURNAL_FILE);
    let bytes = fs::read(&journal).unwrap();
    fs::remove_file(&owner).unwrap();
    let reader = SealedCaptureGuard::acquire(&directory, &id).unwrap();
    assert!(matches!(
        ArtifactGuard::acquire(&directory, &id),
        Err(HandoffError::Busy)
    ));
    assert_eq!(reader.terminal(), &terminal);
    let mut confirmed = Vec::new();
    reader
        .scan_confirmed::<HandoffError>(|caption| {
            confirmed.push(caption);
            Ok(())
        })
        .unwrap();
    assert_eq!(confirmed, vec![caption(1), caption(2)]);
    drop(reader);
    assert_eq!(fs::read(journal).unwrap(), bytes);
    assert!(!owner.exists());
}

#[test]
fn sealed_scan_rejects_partial_tail_without_repairing_the_journal() {
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    writer.append_confirmed(&caption(1)).unwrap();
    writer.publish_terminal(&manifest(&id, 1)).unwrap();
    drop(writer);
    let journal = directory.join(JOURNAL_FILE);
    fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(&[0, 0])
        .unwrap();
    let bytes = fs::read(&journal).unwrap();
    let reader = SealedCaptureGuard::acquire(&directory, &id).unwrap();
    assert!(matches!(
        reader.scan_confirmed::<HandoffError>(|_| Ok(())),
        Err(HandoffError::CorruptJournal)
    ));
    assert_eq!(fs::read(journal).unwrap(), bytes);
}

#[test]
fn sealed_empty_capture_does_not_require_or_create_a_journal() {
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    writer.publish_terminal(&manifest(&id, 0)).unwrap();
    drop(writer);
    let reader = SealedCaptureGuard::acquire(&directory, &id).unwrap();
    reader
        .scan_confirmed::<HandoffError>(|_| panic!("an empty seal has no confirmed frames"))
        .unwrap();
    assert!(!directory.join(JOURNAL_FILE).exists());
}

#[test]
fn v1_keeps_its_exact_disk_shape_and_accepts_legacy_soniox() {
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    let mut terminal = manifest(&id, 0);
    terminal.provenance.model_id = None;
    writer.publish_terminal(&terminal).unwrap();
    drop(writer);
    assert_eq!(
        fs::read(directory.join(TERMINAL_FILE)).unwrap(),
        serde_json::to_vec(&terminal).unwrap()
    );
    let snapshot = read_terminal_snapshot_with_evidence(&directory, &id).unwrap();
    assert_eq!(snapshot.manifest, terminal);
    assert!(snapshot.raw_screen.is_none());
    assert!(snapshot.soniox_outcome.is_none());
    let reader = SealedCaptureGuard::acquire(&directory, &id).unwrap();
    reader.verify_raw_screen_evidence().unwrap();
}

#[test]
fn v2_raw_only_is_not_published_video_and_uses_the_existing_guard() {
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    let mut terminal = manifest(&id, 0);
    terminal.provenance = ProviderProvenance {
        provider: SttProvider::Local,
        model_id: None,
    };
    terminal.status = CaptureTerminalStatus::Completed;
    terminal.has_audio = true;
    terminal.duration_ms = 1_000;
    fs::write(directory.join("mix.wav"), b"audio sentinel").unwrap();
    fs::write(directory.join("screen.raw.mp4"), b"raw screen").unwrap();
    fs::write(directory.join("screen.raw.timeline.json"), b"timeline").unwrap();
    fs::write(directory.join("mix.clock.json"), b"").unwrap();
    let outcome = Some(RawScreenWriterOutcome::Stopped);
    terminal.has_video = true;
    assert!(matches!(
        writer.publish_terminal_with_evidence(&terminal, outcome, None),
        Err(HandoffError::Invalid)
    ));
    terminal.has_video = false;
    writer
        .publish_terminal_with_evidence(&terminal, outcome, None)
        .unwrap();
    writer
        .publish_terminal_with_evidence(&terminal, outcome, None)
        .unwrap();
    assert!(matches!(
        writer.publish_terminal(&terminal),
        Err(HandoffError::SequenceConflict)
    ));
    assert!(matches!(
        SealedCaptureGuard::acquire(&directory, &id),
        Err(HandoffError::Busy)
    ));
    drop(writer);

    let bytes = fs::read(directory.join(TERMINAL_FILE)).unwrap();
    let disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(disk["version"], EVIDENCE_MANIFEST_VERSION);
    assert_eq!(disk["capture"]["has_video"], false);
    assert!(disk.get("soniox_outcome").is_none());
    assert!(!directory.join("screen.mp4").exists());
    let reader = SealedCaptureGuard::acquire(&directory, &id).unwrap();
    assert_eq!(reader.terminal(), &terminal);
    assert!(reader.soniox_outcome().is_none());
    let screen = reader.raw_screen_evidence().unwrap();
    assert_eq!(screen.raw_mp4_bytes, Some(10));
    assert_eq!(screen.timeline_bytes, Some(8));
    assert_eq!(screen.video_clock_bytes, None);
    assert_eq!(screen.audio_clock_bytes, Some(0));
    reader.verify_raw_screen_evidence().unwrap();

    fs::write(directory.join("screen.raw.mp4"), b"changed length").unwrap();
    assert!(matches!(
        reader.verify_raw_screen_evidence(),
        Err(HandoffError::Invalid)
    ));
    assert_eq!(
        read_terminal_snapshot_with_evidence(&directory, &id)
            .unwrap()
            .raw_screen
            .as_ref(),
        Some(screen)
    );
    drop(reader);
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    writer
        .publish_terminal_with_evidence(&terminal, outcome, None)
        .unwrap();
    assert_eq!(fs::read(directory.join(TERMINAL_FILE)).unwrap(), bytes);
}

#[test]
fn v2_can_keep_partial_video_and_confirmed_soniox_after_failure() {
    use bazza_domain::capture_stt::SonioxIncompleteReason;
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    writer.append_confirmed(&caption(1)).unwrap();
    let mut terminal = manifest(&id, 1);
    let raw = Some(RawScreenWriterOutcome::Failed);
    let soniox = Some(SonioxCaptureOutcome::Incomplete {
        reason: SonioxIncompleteReason::PermissionRevoked,
    });
    assert!(matches!(
        writer.publish_terminal_with_evidence(&terminal, raw, None),
        Err(HandoffError::Invalid)
    ));
    terminal.provenance.model_id = None;
    assert!(matches!(
        writer.publish_terminal_with_evidence(&terminal, raw, soniox),
        Err(HandoffError::Invalid)
    ));
    terminal.provenance = provenance();
    fs::write(directory.join("screen.raw.mp4"), b"").unwrap();
    writer
        .publish_terminal_with_evidence(&terminal, raw, soniox)
        .unwrap();
    assert!(matches!(
        writer.publish_terminal_with_evidence(
            &terminal,
            raw,
            Some(SonioxCaptureOutcome::Complete {})
        ),
        Err(HandoffError::SequenceConflict)
    ));
    drop(writer);

    let reader = SealedCaptureGuard::acquire(&directory, &id).unwrap();
    assert_eq!(reader.soniox_outcome(), soniox.as_ref());
    let evidence = reader.raw_screen_evidence().unwrap();
    assert_eq!(evidence.writer, RawScreenWriterOutcome::Failed);
    assert_eq!(evidence.raw_mp4_bytes, Some(0));
    assert_eq!(evidence.timeline_bytes, None);
    reader.verify_raw_screen_evidence().unwrap();
    let mut confirmed = Vec::new();
    reader
        .scan_confirmed::<HandoffError>(|record| {
            confirmed.push(record);
            Ok(())
        })
        .unwrap();
    assert_eq!(confirmed, vec![caption(1)]);
}

#[test]
fn v2_soniox_only_rejects_malformed_or_wrong_provider_evidence() {
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    let terminal = manifest(&id, 0);
    assert!(matches!(
        writer.publish_terminal_with_evidence(&terminal, None, None),
        Err(HandoffError::Invalid)
    ));
    writer
        .publish_terminal_with_evidence(&terminal, None, Some(SonioxCaptureOutcome::Complete {}))
        .unwrap();
    drop(writer);
    let path = directory.join(TERMINAL_FILE);
    let bytes = fs::read(&path).unwrap();
    let disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let snapshot = read_terminal_snapshot_with_evidence(&directory, &id).unwrap();
    assert_eq!(
        snapshot.soniox_outcome,
        Some(SonioxCaptureOutcome::Complete {})
    );
    assert!(snapshot.raw_screen.is_none());
    for (field, value) in [
        ("version", serde_json::json!(3)),
        (
            "soniox_outcome",
            serde_json::json!({"status":"complete","extra":true}),
        ),
        ("soniox_outcome", serde_json::Value::Null),
        (
            "raw_screen",
            serde_json::json!({"writer":"failed","raw_mp4_bytes":u64::MAX}),
        ),
        ("path", serde_json::json!("custom.mp4")),
    ] {
        let mut invalid = disk.clone();
        invalid[field] = value;
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(read_terminal_snapshot_with_evidence(&directory, &id).is_err());
        assert!(SealedCaptureGuard::acquire(&directory, &id).is_err());
    }
    let mut wrong_provider = disk;
    wrong_provider["capture"]["provenance"]["provider"] = serde_json::json!("local");
    fs::write(path, serde_json::to_vec(&wrong_provider).unwrap()).unwrap();
    assert!(read_terminal_snapshot_with_evidence(&directory, &id).is_err());
}

#[test]
fn raw_evidence_rejects_a_nonregular_file_without_publishing() {
    let (_temp, directory, id) = fixture();
    let mut writer = ArtifactGuard::acquire(&directory, &id).unwrap();
    fs::create_dir(directory.join("screen.raw.mp4")).unwrap();
    let mut terminal = manifest(&id, 0);
    terminal.provenance = ProviderProvenance {
        provider: SttProvider::Local,
        model_id: None,
    };
    assert!(matches!(
        writer.publish_terminal_with_evidence(
            &terminal,
            Some(RawScreenWriterOutcome::Failed),
            None
        ),
        Err(HandoffError::UnsafePath)
    ));
    assert!(!directory.join(TERMINAL_FILE).exists());
}

#[test]
fn durable_confirmed_sequence_replays_exactly_after_reopen_and_conflicts_fail() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    let first = guard.append_confirmed(&caption(1)).unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(first.digest.len(), 64);
    assert_eq!(guard.append_confirmed(&caption(1)).unwrap(), first);
    assert!(matches!(
        guard.append_confirmed(&caption(3)),
        Err(HandoffError::SequenceConflict)
    ));
    let mut conflict = caption(1);
    conflict.text = "изменённый raw".into();
    assert!(matches!(
        guard.append_confirmed(&conflict),
        Err(HandoffError::SequenceConflict)
    ));
    drop(guard);
    let mut reopened = ArtifactGuard::acquire(&directory, &id).unwrap();
    assert_eq!(reopened.confirmed(), [caption(1)]);
    assert_eq!(reopened.append_confirmed(&caption(1)).unwrap(), first);
    let second = reopened.append_confirmed(&caption(2)).unwrap();
    assert_ne!(first.digest, second.digest);
    assert_eq!(reopened.last_sequence(), 2);
}

#[test]
fn incomplete_final_frame_is_reported_and_replaced_without_losing_confirmed_prefix() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    guard.append_confirmed(&caption(1)).unwrap();
    drop(guard);
    let original = fs::read(directory.join(JOURNAL_FILE)).unwrap();
    for tail in [vec![0, 0], vec![0, 0, 0, 20, 123, 34]] {
        fs::write(
            directory.join(JOURNAL_FILE),
            [original.clone(), tail].concat(),
        )
        .unwrap();
        let mut reopened = ArtifactGuard::acquire(&directory, &id).unwrap();
        assert!(reopened.has_incomplete_tail());
        assert_eq!(reopened.confirmed(), [caption(1)]);
        reopened.append_confirmed(&caption(2)).unwrap();
        assert!(!reopened.has_incomplete_tail());
        drop(reopened);
        let checked = ArtifactGuard::acquire(&directory, &id).unwrap();
        assert_eq!(checked.confirmed(), [caption(1), caption(2)]);
        drop(checked);
    }
}

#[test]
fn complete_corruption_duplicate_frame_and_wrong_identity_are_not_truncated() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    guard.append_confirmed(&caption(1)).unwrap();
    drop(guard);
    let original = fs::read(directory.join(JOURNAL_FILE)).unwrap();
    let mut changed = original.clone();
    changed[80] ^= 1;
    fs::write(directory.join(JOURNAL_FILE), &changed).unwrap();
    assert!(matches!(
        ArtifactGuard::acquire(&directory, &id),
        Err(HandoffError::CorruptJournal)
    ));
    assert_eq!(fs::read(directory.join(JOURNAL_FILE)).unwrap(), changed);
    fs::write(directory.join(JOURNAL_FILE), &original).unwrap();
    let mut wrong = id.clone();
    wrong.generation = Uuid::new_v4();
    assert!(matches!(
        ArtifactGuard::acquire(&directory, &wrong),
        Err(HandoffError::IdentityMismatch)
    ));
    // A complete copied frame is corrupt, not an incomplete crash tail.
    let frame = &original[72..];
    fs::OpenOptions::new()
        .append(true)
        .open(directory.join(JOURNAL_FILE))
        .unwrap()
        .write_all(frame)
        .unwrap();
    assert!(matches!(
        ArtifactGuard::acquire(&directory, &id),
        Err(HandoffError::CorruptJournal)
    ));
}

#[test]
fn complete_unacknowledged_frame_is_reconciled_without_raw_replacement() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    guard.append_confirmed(&caption(1)).unwrap();
    let other = tempfile::tempdir().unwrap();
    let mirror = other.path().join(id.meeting_id.to_string());
    fs::create_dir(&mirror).unwrap();
    let mut second = ArtifactGuard::acquire(&mirror, &id).unwrap();
    second.append_confirmed(&caption(1)).unwrap();
    second.append_confirmed(&caption(2)).unwrap();
    drop(second);
    // Simulate write_all succeeding but sync_all/ACK failing before the first
    // guard updates its in-memory prefix.
    fs::write(
        directory.join(JOURNAL_FILE),
        fs::read(mirror.join(JOURNAL_FILE)).unwrap(),
    )
    .unwrap();
    let mut changed = caption(2);
    changed.text = "must not overwrite complete raw".into();
    assert!(matches!(
        guard.append_confirmed(&changed),
        Err(HandoffError::SequenceConflict)
    ));
    assert_eq!(guard.last_sequence(), 2);
    assert_eq!(guard.confirmed()[1], caption(2));
    assert_eq!(guard.append_confirmed(&caption(2)).unwrap().sequence, 2);
}

#[test]
fn terminal_seal_is_idempotent_persistent_and_blocks_further_captions() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    guard.append_confirmed(&caption(1)).unwrap();
    let terminal = manifest(&id, 1);
    guard.publish_terminal(&terminal).unwrap();
    let bytes = fs::read(directory.join(TERMINAL_FILE)).unwrap();
    guard.publish_terminal(&terminal).unwrap();
    assert_eq!(fs::read(directory.join(TERMINAL_FILE)).unwrap(), bytes);
    assert!(matches!(
        guard.append_confirmed(&caption(2)),
        Err(HandoffError::Sealed)
    ));
    let mut wrong = terminal.clone();
    wrong.duration_ms += 1;
    assert!(matches!(
        guard.publish_terminal(&wrong),
        Err(HandoffError::SequenceConflict)
    ));
    drop(guard);
    let recovered = ArtifactGuard::acquire(&directory, &id).unwrap();
    assert_eq!(recovered.terminal(), Some(&terminal));
    assert_eq!(recovered.confirmed(), [caption(1)]);
}

#[test]
fn terminal_checks_version_identity_sequence_provider_and_fixed_artifact_presence() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    guard.append_confirmed(&caption(1)).unwrap();
    let terminal = manifest(&id, 1);
    let mut bad = terminal.clone();
    bad.version = 2;
    assert!(matches!(
        guard.publish_terminal(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut bad = terminal.clone();
    bad.identity.worker_session_id = Uuid::new_v4();
    assert!(matches!(
        guard.publish_terminal(&bad),
        Err(HandoffError::IdentityMismatch)
    ));
    let mut bad = terminal.clone();
    bad.last_confirmed_sequence = 2;
    assert!(matches!(
        guard.publish_terminal(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut bad = terminal.clone();
    bad.provenance.provider = SttProvider::Local;
    assert!(matches!(
        guard.publish_terminal(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut bad = terminal.clone();
    bad.error_code = Some("raw transcript secret".into());
    assert!(matches!(
        guard.publish_terminal(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut success = terminal;
    success.status = CaptureTerminalStatus::Completed;
    success.has_audio = true;
    assert!(matches!(
        guard.publish_terminal(&success),
        Err(HandoffError::Io(_))
    ));
    assert!(!directory.join(TERMINAL_FILE).exists());
    fs::write(directory.join("mix.wav"), b"synthetic raw artifact").unwrap();
    guard.publish_terminal(&success).unwrap();
}

#[test]
fn bounds_and_private_provenance_do_not_write_invalid_or_oversized_data() {
    let (_temp, directory, id) = fixture();
    let mut guard = ArtifactGuard::acquire(&directory, &id).unwrap();
    let mut bad = caption(1);
    bad.text = "x".repeat(MAX_TEXT_BYTES + 1);
    assert!(matches!(
        guard.append_confirmed(&bad),
        Err(HandoffError::TooLarge)
    ));
    let mut bad = caption(1);
    bad.text = "raw\0payload".into();
    assert!(matches!(
        guard.append_confirmed(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut bad = caption(1);
    bad.provenance.model_id = Some("https://private.example/key".into());
    assert!(matches!(
        guard.append_confirmed(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut bad = caption(1);
    bad.provenance.model_id = Some("C:\\models\\file.gguf".into());
    assert!(matches!(
        guard.append_confirmed(&bad),
        Err(HandoffError::Invalid)
    ));
    let mut bad = caption(1);
    bad.end_ms = bad.start_ms;
    assert!(matches!(
        guard.append_confirmed(&bad),
        Err(HandoffError::Invalid)
    ));
    assert!(!directory.join(JOURNAL_FILE).exists());
    guard.append_confirmed(&caption(1)).unwrap();
    let mut changed = caption(2);
    changed.provenance.provider = SttProvider::Local;
    assert!(matches!(
        guard.append_confirmed(&changed),
        Err(HandoffError::Invalid)
    ));
    assert_eq!(guard.last_sequence(), 1);
}

#[test]
fn missing_or_mismatched_directory_never_gets_created() {
    let temporary = tempfile::tempdir().unwrap();
    let id = identity();
    let missing = temporary.path().join(id.meeting_id.to_string());
    assert!(ArtifactGuard::acquire(&missing, &id).is_err());
    assert!(!missing.exists());
    assert!(matches!(
        ArtifactGuard::acquire(temporary.path(), &id),
        Err(HandoffError::UnsafePath)
    ));
    assert!(!temporary.path().join(LOCK_FILE).exists());
}

#[cfg(windows)]
#[test]
fn parent_junction_or_leaf_link_is_rejected_without_mutating_external_files() {
    use std::os::windows::process::CommandExt;
    let temporary = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let id = identity();
    let junction = temporary.path().join(id.meeting_id.to_string());
    fs::write(outside.path().join("sentinel"), b"unchanged").unwrap();
    let output=std::process::Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:BAZZA_CAPTURE_JUNCTION -Target $env:BAZZA_CAPTURE_TARGET | Out-Null"])
        .env("BAZZA_CAPTURE_JUNCTION",&junction).env("BAZZA_CAPTURE_TARGET",outside.path()).creation_flags(0x08000000).output().unwrap();
    assert!(
        output.status.success(),
        "junction fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(matches!(
        ArtifactGuard::acquire(&junction, &id),
        Err(HandoffError::UnsafePath)
    ));
    assert_eq!(
        fs::read(outside.path().join("sentinel")).unwrap(),
        b"unchanged"
    );
    assert!(!outside.path().join(LOCK_FILE).exists());
    fs::remove_dir(&junction).unwrap();
}

// The subprocess entry is inert in a normal test run. Its parent below supplies
// a new temporary directory, kills only this owned child and checks lock release.
#[test]
fn lock_child_entry() {
    let Some(root) = std::env::var_os("BAZZA_CAPTURE_LOCK_CHILD") else {
        return;
    };
    let root = PathBuf::from(root);
    let identity: CaptureIdentity =
        serde_json::from_slice(&fs::read(root.join("test-identity.json")).unwrap()).unwrap();
    let _guard =
        ArtifactGuard::acquire(&root.join(identity.meeting_id.to_string()), &identity).unwrap();
    fs::write(root.join("child-ready"), b"ready").unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[test]
fn owned_process_crash_releases_artifact_lock_for_recovery() {
    let (temporary, directory, id) = fixture();
    fs::write(
        temporary.path().join("test-identity.json"),
        serde_json::to_vec(&id).unwrap(),
    )
    .unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "lock_child_entry", "--nocapture"])
        .env("BAZZA_CAPTURE_LOCK_CHILD", temporary.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let started = std::time::Instant::now();
    while !temporary.path().join("child-ready").exists() {
        if started.elapsed() > std::time::Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned lock child failed to become ready");
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("owned lock child exited: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut child = OwnedChild(child);
    assert!(matches!(
        ArtifactGuard::acquire(&directory, &id),
        Err(HandoffError::Busy)
    ));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let recovered = ArtifactGuard::acquire(&directory, &id).unwrap();
    assert!(recovered.confirmed().is_empty());
    assert!(directory.join(LOCK_FILE).exists());
}

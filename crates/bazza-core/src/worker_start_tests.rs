use super::*;
use crate::MigrationPolicy;
use bazza_service_test_support::create_v8;

#[test]
fn finalize_replays_receipt_committed_by_concurrent_recovery() {
    use bazza_capture_handoff as artifact;
    use std::sync::mpsc;

    let (_temporary, repository, proof, start, lease) = reservation(true);
    let peer = std::process::id();
    let path = repository.recording_path(lease.meeting_id);
    let stored_lease = convert(&lease).unwrap();
    let identity = repository.identity(&stored_lease);
    let mut writer = ArtifactGuard::acquire(&path, &identity).unwrap();
    repository
        .begin_capture(peer, begin_request(&proof, &start, &lease))
        .unwrap();
    fs::write(path.join("mix.wav"), b"RIFF\0\0\0\0WAVE").unwrap();
    fs::write(path.join("screen.raw.mp4"), b"synthetic raw screen").unwrap();
    writer
        .publish_terminal_with_evidence(
            &artifact::TerminalManifest {
                version: artifact::MANIFEST_VERSION,
                identity: identity.clone(),
                provenance: artifact::ProviderProvenance {
                    provider: artifact::SttProvider::Soniox,
                    model_id: start.provider.model.clone(),
                },
                duration_ms: 1000,
                ended_at: "2026-11-02T00:00:01Z".parse().unwrap(),
                status: artifact::CaptureTerminalStatus::Completed,
                error_code: None,
                has_audio: true,
                has_video: false,
                last_confirmed_sequence: 0,
            },
            Some(artifact::RawScreenWriterOutcome::Stopped),
            Some(artifact::SonioxCaptureOutcome::Complete {}),
        )
        .unwrap();
    drop(writer);
    let recovery_guard = SealedCaptureGuard::acquire(&path, &identity).unwrap();
    let terminal = verified_terminal(&recovery_guard, &convert(&start.provider).unwrap()).unwrap();
    let request = wire::FinalizeCaptureRequest {
        proof,
        lease,
        fence: start.fence,
    };
    let mut forged = request.clone();
    forged.fence = wire::WorkerSecret::random();
    assert!(matches!(
        repository.finalize_capture(peer, forged),
        Err(CoreError::Data(domain::DataError::Forbidden))
    ));
    let (started_tx, started_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let repository = &repository;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            reply_tx
                .send(repository.finalize_capture(peer, request))
                .unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            reply_rx.recv_timeout(Duration::from_millis(40)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        // Commit while recovery still holds the exact artifact lock.
        let recovered = repository
            .store
            .service_recover_capture(repository.epoch, &stored_lease, &terminal)
            .unwrap();
        let reply = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            reply,
            convert::<_, wire::WorkerMutationOutcome>(&recovered).unwrap()
        );
        assert!(reply.job_id.is_some());
    });
    drop(recovery_guard);
}

fn reservation(
    artifacts: bool,
) -> (
    tempfile::TempDir,
    Repository,
    wire::WorkerProof,
    wire::CaptureStart,
    wire::CaptureLease,
) {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = create_v8(&temporary.path().join("library")).unwrap();
    let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
    let (proof, start, lease) = reserve(&repository, artifacts, wire::CaptureProvider::Soniox);
    (temporary, repository, proof, start, lease)
}

fn reserve(
    repository: &Repository,
    artifacts: bool,
    provider: wire::CaptureProvider,
) -> (wire::WorkerProof, wire::CaptureStart, wire::CaptureLease) {
    let registration = wire::WorkerRegistration::new(vec![wire::WorkerCapability::Capture]);
    repository
        .register_worker(
            std::process::id(),
            wire::RegisterWorkerRequest {
                registration: registration.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap();
    let proof = wire::WorkerProof {
        session_id: registration.session_id,
        secret: registration.secret,
        library_epoch: repository.epoch,
    };
    let start = wire::CaptureStart {
        capture_id: uuid::Uuid::new_v4(),
        meeting_id: uuid::Uuid::new_v4(),
        fence: wire::WorkerSecret::random(),
        title: "Reserved capture".into(),
        app: None,
        started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        participants: vec![],
        provider: wire::CaptureSelection {
            provider,
            model: Some(
                match provider {
                    wire::CaptureProvider::Local => "fixture-whisper",
                    wire::CaptureProvider::Soniox => "fixture-soniox",
                }
                .into(),
            ),
            language: Some("ru".into()),
        },
    };
    let lease = if artifacts {
        repository
            .prepare_capture(
                std::process::id(),
                wire::PrepareCaptureRequest {
                    proof: proof.clone(),
                    start: start.clone(),
                },
            )
            .unwrap()
    } else {
        convert(
            &repository
                .store
                .service_prepare_capture(
                    std::process::id(),
                    repository.epoch,
                    &convert(&proof).unwrap(),
                    &convert(&start).unwrap(),
                )
                .unwrap(),
        )
        .unwrap()
    };
    (proof, start, lease)
}
fn begin_request(
    proof: &wire::WorkerProof,
    start: &wire::CaptureStart,
    lease: &wire::CaptureLease,
) -> wire::BeginCaptureRequest {
    wire::BeginCaptureRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: start.fence.clone(),
    }
}
fn abort_request(
    proof: &wire::WorkerProof,
    start: &wire::CaptureStart,
    lease: &wire::CaptureLease,
) -> wire::AbortCaptureRequest {
    wire::AbortCaptureRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: start.fence.clone(),
    }
}

#[test]
fn begin_requires_actual_worker_guard_and_begun_capture_cannot_be_aborted() {
    let (_temporary, repository, proof, start, lease) = reservation(true);
    let request = begin_request(&proof, &start, &lease);
    assert!(matches!(
        repository.begin_capture(std::process::id(), request.clone()),
        Err(CoreError::Data(domain::DataError::Busy))
    ));
    let path = repository.recording_path(lease.meeting_id);
    let guard =
        ArtifactGuard::acquire(&path, &repository.identity(&convert(&lease).unwrap())).unwrap();
    assert!(repository
        .begin_capture(std::process::id().wrapping_add(1), request.clone())
        .is_err());
    assert_eq!(
        repository
            .begin_capture(std::process::id(), request.clone())
            .unwrap(),
        lease
    );
    assert_eq!(
        repository
            .begin_capture(std::process::id(), request)
            .unwrap(),
        lease
    );
    drop(guard);
    assert!(matches!(
        repository.abort_capture(std::process::id(), abort_request(&proof, &start, &lease)),
        Err(CoreError::Data(domain::DataError::Busy))
    ));
    let root = repository.store.root().to_path_buf();
    drop(repository);
    let repository = Repository::open_fixture(&root, MigrationPolicy::RequireCurrent).unwrap();
    let recovery = repository.recover_capture_handoffs().unwrap();
    assert_eq!(recovery.aborted, 0);
    assert_eq!(recovery.recovered, 1);
    assert_eq!(
        repository
            .store
            .get_meeting(&lease.meeting_id.to_string())
            .unwrap()
            .unwrap()
            .status,
        bazza_store::MeetingStatus::Failed
    );
}

#[test]
fn managed_interrupted_capture_recovers_audio_and_confirmed_text_and_allows_next_recording() {
    use bazza_capture_handoff as artifact;
    use std::io::Write;

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("managed");
    let repository = Repository::create_managed(&root).unwrap();
    let (proof, start, lease) = reserve(&repository, true, wire::CaptureProvider::Local);
    let path = repository.recording_path(lease.meeting_id);
    let identity = repository.identity(&convert(&lease).unwrap());
    let mut writer = ArtifactGuard::acquire(&path, &identity).unwrap();
    repository
        .begin_capture(std::process::id(), begin_request(&proof, &start, &lease))
        .unwrap();
    let audio = path.join("mix.wav");
    // A second of declared mono PCM plus extra samples after the last WAV flush.
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&96_036_u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wav.extend_from_slice(&48_000_u32.to_le_bytes());
    wav.extend_from_slice(&96_000_u32.to_le_bytes());
    wav.extend_from_slice(b"\x02\0\x10\0data");
    wav.extend_from_slice(&96_000_u32.to_le_bytes());
    wav.resize(96_044 + 200, 1);
    fs::write(&audio, &wav).unwrap();
    let screen = path.join("screen.raw.mp4");
    fs::write(&screen, b"raw video prefix").unwrap();
    writer
        .append_local_live_confirmed(&artifact::ConfirmedCaption {
            sequence: 1,
            start_ms: 100,
            end_ms: 300,
            speaker: Some("Анна".into()),
            text: "сохранённый текст".into(),
            provenance: artifact::ProviderProvenance {
                provider: artifact::SttProvider::Local,
                model_id: start.provider.model.clone(),
            },
        })
        .unwrap();
    let journal = path.join(artifact::JOURNAL_FILE);
    fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(&[0, 1])
        .unwrap();
    let journal_bytes = fs::read(&journal).unwrap();
    assert_eq!(repository.recover_capture_handoffs().unwrap().active, 1);
    let next = wire::CaptureStart {
        capture_id: uuid::Uuid::new_v4(),
        meeting_id: uuid::Uuid::new_v4(),
        fence: wire::WorkerSecret::random(),
        ..start.clone()
    };
    assert!(matches!(
        repository.prepare_capture(
            std::process::id(),
            wire::PrepareCaptureRequest {
                proof: proof.clone(),
                start: next,
            }
        ),
        Err(CoreError::Data(domain::DataError::Busy))
    ));
    // Releasing the guard stands in for process death, without a Stop or a seal.
    drop(writer);
    drop(repository);
    let repository = Repository::open_managed(&root).unwrap();
    assert_eq!(repository.recover_capture_handoffs().unwrap().recovered, 1);
    assert_eq!(repository.recover_capture_handoffs().unwrap().recovered, 0);
    let id = lease.meeting_id.to_string();
    let meeting = repository.store.get_meeting(&id).unwrap().unwrap();
    assert_eq!(meeting.status, bazza_store::MeetingStatus::Failed);
    assert_eq!(meeting.duration_ms, 1000);
    assert!(!meeting.has_video);
    assert!(meeting.error.unwrap().contains("interrupted"));
    let segments = repository.store.segments(&id).unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "сохранённый текст");
    assert_eq!(segments[0].speaker.as_deref(), Some("Анна"));
    let stt = repository
        .store
        .get_setting::<serde_json::Value>(&format!("stt:{id}"))
        .unwrap()
        .unwrap();
    assert_eq!(stt["status"], "incomplete");
    assert_eq!(fs::read(audio).unwrap(), wav);
    assert_eq!(fs::read(screen).unwrap(), b"raw video prefix");
    assert_eq!(fs::read(journal).unwrap(), journal_bytes);
    assert!(!path.join(artifact::TERMINAL_FILE).exists());
    let (_proof, _start, next_lease) = reserve(&repository, true, wire::CaptureProvider::Local);
    assert_ne!(next_lease.meeting_id, lease.meeting_id);
}

#[test]
fn abort_retains_artifacts_and_receipt_survives_purge_without_recreating_paths() {
    let (temporary, repository, proof, start, lease) = reservation(true);
    let path = repository.recording_path(lease.meeting_id);
    let request = abort_request(&proof, &start, &lease);
    let receipt = repository
        .abort_capture(std::process::id(), request.clone())
        .unwrap();
    assert!(receipt.job_id.is_none());
    assert_ne!(receipt.revision, lease.generation.to_string());
    let meeting = repository
        .store
        .get_meeting(&lease.meeting_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(meeting.status, bazza_store::MeetingStatus::Failed);
    assert_eq!(meeting.error.as_deref(), Some("CAPTURE_NOT_STARTED"));
    assert_eq!(fs::read_dir(&path).unwrap().count(), 2);
    // Acquiring a later guard cannot resurrect the aborted reservation.
    let guard =
        ArtifactGuard::acquire(&path, &repository.identity(&convert(&lease).unwrap())).unwrap();
    assert!(repository
        .begin_capture(std::process::id(), begin_request(&proof, &start, &lease))
        .is_err());
    assert_eq!(
        repository
            .abort_capture(std::process::id(), request.clone())
            .unwrap(),
        receipt
    );
    drop(guard);
    repository
        .store
        .delete_meeting(&lease.meeting_id.to_string())
        .unwrap();
    fs::rename(&path, temporary.path().join("retained-artifacts")).unwrap();
    let root = repository.store.root().to_path_buf();
    drop(repository);
    let repository = Repository::open_fixture(&root, MigrationPolicy::RequireCurrent).unwrap();
    assert_eq!(
        repository
            .abort_capture(std::process::id(), request)
            .unwrap(),
        receipt
    );
    assert!(!path.exists());
    assert!(repository
        .prepare_capture(
            std::process::id(),
            wire::PrepareCaptureRequest { proof, start }
        )
        .is_err());
    assert!(!path.exists());
}

#[test]
fn abort_refuses_live_guard_and_every_possible_capture_artifact_even_if_empty() {
    for name in [
        "mix.wav",
        "screen.mp4",
        bazza_capture_handoff::JOURNAL_FILE,
        bazza_capture_handoff::TERMINAL_FILE,
        "unknown-sidecar",
    ] {
        let (_temporary, repository, proof, start, lease) = reservation(true);
        let path = repository.recording_path(lease.meeting_id);
        let guard =
            ArtifactGuard::acquire(&path, &repository.identity(&convert(&lease).unwrap())).unwrap();
        assert!(matches!(
            repository.abort_capture(std::process::id(), abort_request(&proof, &start, &lease)),
            Err(CoreError::Data(domain::DataError::Busy))
        ));
        drop(guard);
        fs::write(path.join(name), b"").unwrap();
        assert!(
            repository
                .abort_capture(std::process::id(), abort_request(&proof, &start, &lease))
                .is_err(),
            "{name}"
        );
        assert_eq!(fs::read(path.join(name)).unwrap(), b"");
        assert_eq!(
            repository
                .store
                .service_pending_reservations(uuid::Uuid::new_v4(), 32)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repository
                .store
                .get_meeting(&lease.meeting_id.to_string())
                .unwrap()
                .unwrap()
                .status,
            bazza_store::MeetingStatus::Recording
        );
    }
}

#[test]
fn current_reservation_is_never_cancelled_but_restart_compensates_missing_fs_init() {
    let (_temporary, repository, _proof, _start, lease) = reservation(false);
    let path = repository.recording_path(lease.meeting_id);
    assert!(!path.exists());
    assert_eq!(
        repository.reconcile_capture_reservations().unwrap().aborted,
        0
    );
    assert!(!path.exists());
    let root = repository.store.root().to_path_buf();
    drop(repository);
    let repository = Repository::open_fixture(&root, MigrationPolicy::RequireCurrent).unwrap();
    assert_eq!(
        repository.reconcile_capture_reservations().unwrap().aborted,
        1
    );
    assert_eq!(
        repository.reconcile_capture_reservations().unwrap().aborted,
        0
    );
    assert_eq!(
        repository
            .store
            .get_meeting(&lease.meeting_id.to_string())
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some("CAPTURE_NOT_STARTED")
    );
    assert_eq!(fs::read_dir(path).unwrap().count(), 2);
}

#[test]
fn held_reserved_guard_survives_restart_then_reattaches_before_begin() {
    let (_temporary, repository, proof, start, lease) = reservation(true);
    let root = repository.store.root().to_path_buf();
    let path = repository.recording_path(lease.meeting_id);
    let guard =
        ArtifactGuard::acquire(&path, &repository.identity(&convert(&lease).unwrap())).unwrap();
    drop(repository);
    let repository = Repository::open_fixture(&root, MigrationPolicy::RequireCurrent).unwrap();
    let recovery = repository.reconcile_capture_reservations().unwrap();
    assert_eq!(recovery.active, 1);
    assert_eq!(recovery.aborted, 0);
    drop(guard);
    assert_eq!(repository.recover_capture_handoffs().unwrap().aborted, 0);
    assert_eq!(
        repository
            .store
            .service_pending_reservations(repository.epoch, 32)
            .unwrap()
            .len(),
        1
    );
    let guard =
        ArtifactGuard::acquire(&path, &repository.identity(&convert(&lease).unwrap())).unwrap();
    let attached = repository
        .reattach_capture(
            std::process::id(),
            wire::ReattachCaptureRequest {
                proof: proof.clone(),
                capture_id: lease.capture_id,
                fence: start.fence.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap();
    assert_eq!(attached.generation, lease.generation);
    let proof = wire::WorkerProof {
        library_epoch: attached.library_epoch,
        ..proof
    };
    assert_eq!(
        repository
            .begin_capture(std::process::id(), begin_request(&proof, &start, &attached))
            .unwrap(),
        attached
    );
    drop(guard);
    assert!(repository
        .abort_capture(std::process::id(), abort_request(&proof, &start, &attached))
        .is_err());
}

#[test]
fn sealed_not_finalized_capture_rejects_begin_after_reacquire_but_reattaches_and_finalizes() {
    use bazza_capture_handoff::{
        CaptureTerminalStatus, ProviderProvenance, SttProvider, TerminalManifest, MANIFEST_VERSION,
    };
    let (_temporary, repository, proof, start, lease) = reservation(true);
    let root = repository.store.root().to_path_buf();
    let path = repository.recording_path(lease.meeting_id);
    let identity = repository.identity(&convert(&lease).unwrap());
    let mut guard = ArtifactGuard::acquire(&path, &identity).unwrap();
    let begin = begin_request(&proof, &start, &lease);
    repository
        .begin_capture(std::process::id(), begin.clone())
        .unwrap();
    guard
        .publish_terminal(&TerminalManifest {
            version: MANIFEST_VERSION,
            identity: identity.clone(),
            provenance: ProviderProvenance {
                provider: SttProvider::Soniox,
                model_id: start.provider.model.clone(),
            },
            status: CaptureTerminalStatus::Failed,
            duration_ms: 0,
            ended_at: start.started_at,
            has_audio: false,
            has_video: false,
            last_confirmed_sequence: 0,
            error_code: Some("CAPTURE_FAILED".into()),
        })
        .unwrap();
    assert!(repository
        .begin_capture(std::process::id(), begin.clone())
        .is_err());
    drop(guard);
    let guard = ArtifactGuard::acquire(&path, &identity).unwrap();
    assert!(repository.begin_capture(std::process::id(), begin).is_err());
    drop(repository);
    let repository = Repository::open_fixture(&root, MigrationPolicy::RequireCurrent).unwrap();
    let attached = repository
        .reattach_capture(
            std::process::id(),
            wire::ReattachCaptureRequest {
                proof: proof.clone(),
                capture_id: lease.capture_id,
                fence: start.fence.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap();
    let proof = wire::WorkerProof {
        library_epoch: attached.library_epoch,
        ..proof
    };
    assert!(repository
        .begin_capture(std::process::id(), begin_request(&proof, &start, &attached))
        .is_err());
    drop(guard);
    let receipt = repository
        .finalize_capture(
            std::process::id(),
            wire::FinalizeCaptureRequest {
                proof,
                lease: attached,
                fence: start.fence,
            },
        )
        .unwrap();
    assert!(receipt.job_id.is_none());
    assert_eq!(
        repository
            .store
            .get_meeting(&lease.meeting_id.to_string())
            .unwrap()
            .unwrap()
            .status,
        bazza_store::MeetingStatus::Failed
    );
    assert!(!path.join("mix.wav").exists());
}

#[test]
fn reserved_live_owner_with_malformed_terminal_cannot_begin_or_clear_the_file() {
    let (_temporary, repository, proof, start, lease) = reservation(true);
    let path = repository.recording_path(lease.meeting_id);
    let _guard =
        ArtifactGuard::acquire(&path, &repository.identity(&convert(&lease).unwrap())).unwrap();
    let terminal = path.join(bazza_capture_handoff::TERMINAL_FILE);
    fs::write(&terminal, b"malformed terminal").unwrap();
    assert!(repository
        .begin_capture(std::process::id(), begin_request(&proof, &start, &lease))
        .is_err());
    assert_eq!(
        repository
            .store
            .service_pending_reservations(uuid::Uuid::new_v4(), 32)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fs::read(terminal).unwrap(), b"malformed terminal");
}

//! Synthetic filesystem boundary tests; no capture device, model or network.
use super::*;
use bazza_capture_handoff::{
    media_read::MediaReadOwner, ArtifactGuard, ConfirmedCaption, ProviderProvenance,
    RawScreenWriterOutcome, SonioxCaptureOutcome, TerminalManifest, MANIFEST_VERSION,
    TERMINAL_FILE,
};
use bazza_media_gate::{MediaPurgeGuard, GATE_DIRECTORY};
use bazza_service_api::{postprocess as jobs, worker as wire};
use bazza_service_test_support::create_v8;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

struct Fixture {
    repo: Repository,
    directory: PathBuf,
    request: PostProcessMediaRequest,
    terminal: TerminalManifest,
    _temporary: tempfile::TempDir,
}

fn convert<T: serde::Serialize, D: serde::de::DeserializeOwned>(value: &T) -> D {
    serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
}

fn register(repo: &Repository, capability: wire::WorkerCapability) -> wire::WorkerProof {
    let registration = wire::WorkerRegistration::new(vec![capability]);
    let worker = repo
        .register_worker(
            std::process::id(),
            wire::RegisterWorkerRequest {
                registration: registration.clone(),
                ttl_seconds: 300,
            },
        )
        .unwrap();
    wire::WorkerProof {
        session_id: worker.session_id,
        secret: registration.secret,
        library_epoch: worker.library_epoch,
    }
}

fn wav() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&36_u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&32_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes
}

fn fixture() -> Fixture {
    fixture_with_soniox(None)
}

fn fixture_with_soniox(soniox_outcome: Option<SonioxCaptureOutcome>) -> Fixture {
    fixture_with_screen(soniox_outcome, None)
}

fn fixture_with_screen(
    soniox_outcome: Option<SonioxCaptureOutcome>,
    screen: Option<(RawScreenWriterOutcome, bool)>,
) -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let library = create_v8(&temporary.path().join("library Орион")).unwrap();
    let repo = Repository::open_fixture(&library.root, crate::MigrationPolicy::Upgrade).unwrap();
    let capture_proof = register(&repo, wire::WorkerCapability::Capture);
    let start = wire::CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: wire::WorkerSecret::random(),
        title: "Media fixture".into(),
        app: None,
        started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        participants: vec![],
        provider: wire::CaptureSelection {
            provider: if soniox_outcome.is_some() {
                wire::CaptureProvider::Soniox
            } else {
                wire::CaptureProvider::Local
            },
            model: Some("fixture-model".into()),
            language: Some("ru".into()),
        },
    };
    let capture_lease = repo
        .prepare_capture(
            std::process::id(),
            wire::PrepareCaptureRequest {
                proof: capture_proof.clone(),
                start: start.clone(),
            },
        )
        .unwrap();
    let directory = repo
        .store
        .root()
        .join("recordings")
        .join(capture_lease.meeting_id.to_string());
    let identity = CaptureIdentity {
        library_id: repo.library_id,
        meeting_id: capture_lease.meeting_id,
        worker_session_id: capture_lease.worker_session_id,
        generation: capture_lease.generation,
    };
    let mut guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    repo.begin_capture(
        std::process::id(),
        wire::BeginCaptureRequest {
            proof: capture_proof.clone(),
            lease: capture_lease.clone(),
            fence: start.fence.clone(),
        },
    )
    .unwrap();
    fs::write(directory.join("mix.wav"), wav()).unwrap();
    let provenance = ProviderProvenance {
        provider: if soniox_outcome.is_some() {
            SttProvider::Soniox
        } else {
            SttProvider::Local
        },
        model_id: start.provider.model.clone(),
    };
    guard
        .append_confirmed(&ConfirmedCaption {
            sequence: 1,
            start_ms: 0,
            end_ms: 1000,
            speaker: Some("Анна".into()),
            text: "Immutable confirmed fixture".into(),
            provenance: provenance.clone(),
        })
        .unwrap();
    let terminal = TerminalManifest {
        version: MANIFEST_VERSION,
        identity,
        provenance,
        duration_ms: 1000,
        ended_at: "2026-11-02T00:00:01Z".parse().unwrap(),
        status: CaptureTerminalStatus::Completed,
        error_code: None,
        has_audio: true,
        has_video: false,
        last_confirmed_sequence: 1,
    };
    if screen.is_some_and(|(_, raw)| raw) {
        fs::write(directory.join("screen.raw.mp4"), b"original raw frames").unwrap();
    }
    if soniox_outcome.is_some() || screen.is_some() {
        guard
            .publish_terminal_with_evidence(
                &terminal,
                screen.map(|(writer, _)| writer),
                soniox_outcome,
            )
            .unwrap();
    } else {
        guard.publish_terminal(&terminal).unwrap();
    }
    drop(guard);
    repo.finalize_capture(
        std::process::id(),
        wire::FinalizeCaptureRequest {
            proof: capture_proof,
            lease: capture_lease,
            fence: start.fence,
        },
    )
    .unwrap();
    // The completed journal was fully verified by finalize_capture. Media
    // tests never reopen an ArtifactGuard to read or rewrite that journal.
    let proof = register(&repo, wire::WorkerCapability::PostProcess);
    assert_ne!(proof.session_id, terminal.identity.worker_session_id);
    let job = repo
        .post_process_jobs(
            std::process::id(),
            jobs::PostProcessJobsRequest::new(proof.clone()),
        )
        .unwrap()
        .data
        .items
        .remove(0);
    let fence = wire::WorkerSecret::random();
    let lease = repo
        .claim_post_process_job(
            std::process::id(),
            jobs::ClaimPostProcessJobRequest {
                proof: proof.clone(),
                job,
                claim_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                fence: fence.clone(),
                ttl_seconds: 60,
            },
        )
        .unwrap()
        .data;
    Fixture {
        repo,
        directory,
        request: PostProcessMediaRequest {
            proof,
            lease,
            fence,
            owner_nonce: Uuid::new_v4(),
            expected_provider: start.provider,
        },
        terminal,
        _temporary: temporary,
    }
}

fn identity(f: &Fixture) -> MediaReadIdentity {
    let input = &f.request.lease.input;
    MediaReadIdentity {
        library_id: f.repo.library_id,
        library_epoch: f.repo.epoch,
        worker_session_id: f.request.proof.session_id,
        claim_id: f.request.lease.claim_id,
        job_id: f.request.lease.job_id.clone(),
        meeting_id: f.request.lease.meeting_id,
        capture_id: input.capture_id,
        capture_generation: input.capture_generation,
        checkpoint_revision: input.checkpoint_revision.clone(),
        lifecycle_revision: input.lifecycle_revision.clone(),
        transcript_revision: input.transcript_revision.clone(),
        source_id: input.source_id.clone(),
    }
}

fn owner(f: &Fixture) -> (MediaReadOwner, PostProcessMediaRequest) {
    let owner = MediaReadOwner::acquire(f.repo.store.root(), &identity(f)).unwrap();
    let mut request = f.request.clone();
    request.owner_nonce = owner.nonce();
    (owner, request)
}

mod video {
    use super::*;
    use bazza_service_api::postprocess_video::{
        video_staging_name, PostProcessVideoAction, PostProcessVideoFailure,
        PostProcessVideoOutcome, PostProcessVideoRequest,
    };

    fn mp4() -> Vec<u8> {
        let mut bytes = Vec::new();
        for (kind, payload) in [
            (b"ftyp", b"isom\0\0\0\0".as_slice()),
            (b"moov", b"fixture metadata".as_slice()),
            (b"mdat", b"fixture stream".as_slice()),
        ] {
            bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
            bytes.extend_from_slice(kind);
            bytes.extend_from_slice(payload);
        }
        bytes
    }

    fn staged(f: &Fixture, request: &PostProcessMediaRequest) -> PathBuf {
        f.directory.join(video_staging_name(
            request.lease.claim_id,
            request.owner_nonce,
        ))
    }

    fn publish(media: &PostProcessMediaRequest) -> PostProcessVideoRequest {
        PostProcessVideoRequest {
            media: media.clone(),
            result: PostProcessVideoAction::Publish {},
        }
    }

    fn visible(f: &Fixture) -> bool {
        f.repo
            .store
            .get_meeting(&f.request.lease.meeting_id.to_string())
            .unwrap()
            .unwrap()
            .has_video
    }

    #[test]
    fn video_publication_validates_owner_container_and_collision_and_preserves_raw() {
        let f = fixture_with_screen(None, Some((RawScreenWriterOutcome::Stopped, true)));
        let (_owner, request) = owner(&f);
        let publish = publish(&request);
        let peer = std::process::id();
        let stage = staged(&f, &request);
        let target = f.directory.join("screen.mp4");
        fs::write(&stage, b"not MP4").unwrap();
        assert!(f.repo.post_process_video(peer, publish.clone()).is_err());
        assert!(!visible(&f));
        assert!(!target.exists());
        let mp4 = mp4();
        fs::write(&stage, &mp4).unwrap();
        fs::hard_link(&stage, f.directory.join("wrong-alias.mp4")).unwrap();
        assert!(f.repo.post_process_video(peer, publish.clone()).is_err());
        fs::remove_file(f.directory.join("wrong-alias.mp4")).unwrap();
        let mut wrong = publish.clone();
        wrong.media.owner_nonce = Uuid::new_v4();
        assert!(f.repo.post_process_video(peer, wrong).is_err());
        let mut expired = publish.clone();
        expired.media.lease.expires_at = "2020-01-01T00:00:00Z".parse().unwrap();
        assert!(f.repo.post_process_video(peer, expired).is_err());
        fs::write(&target, &mp4).unwrap();
        assert!(f.repo.post_process_video(peer, publish.clone()).is_err());
        assert_eq!(fs::read(&target).unwrap(), mp4);
        assert!(!visible(&f));
        fs::remove_file(&target).unwrap();
        let outcome = f
            .repo
            .post_process_video(peer, publish.clone())
            .unwrap()
            .data;
        assert_eq!(
            outcome.outcome,
            PostProcessVideoOutcome::Published {
                bytes: mp4.len() as u64
            }
        );
        assert!(visible(&f));
        assert_eq!(fs::read(&target).unwrap(), mp4);
        assert_eq!(
            fs::read(f.directory.join("screen.raw.mp4")).unwrap(),
            b"original raw frames"
        );
        assert!(!stage.exists());
        assert_eq!(
            f.repo.post_process_video(peer, publish).unwrap().data,
            outcome
        );
    }

    #[test]
    fn video_publication_recovers_from_install_interruption_or_missing_stage() {
        for (installed, missing_stage) in [(false, false), (true, false), (false, true)] {
            let f = fixture_with_screen(None, Some((RawScreenWriterOutcome::Stopped, true)));
            let (old_owner, old_request) = owner(&f);
            let stage = staged(&f, &old_request);
            let target = f.directory.join("screen.mp4");
            let mp4 = mp4();
            fs::write(&stage, &mp4).unwrap();
            let content = data::PostProcessVideoContent::read(
                mp4.as_slice(),
                mp4.len() as u64,
                old_request.lease.expires_at,
            )
            .unwrap();
            let peer = std::process::id();
            f.repo
                .store
                .service_prepare_post_process_video(
                    peer,
                    f.repo.epoch,
                    &convert(&old_request),
                    &content,
                    || Ok(()),
                )
                .unwrap();
            if installed {
                bazza_capture_handoff::install_synced_media(&stage, &target).unwrap();
            } else if missing_stage {
                fs::remove_file(&stage).unwrap();
            }
            drop(old_owner);
            assert!(!visible(&f));
            let (_owner, request) = owner(&f);
            let grant = f
                .repo
                .post_process_media(peer, request.clone())
                .unwrap()
                .data;
            assert_eq!(grant.video_publication_candidate, !missing_stage);
            assert_ne!(grant.owner_nonce, old_request.owner_nonce);
            if missing_stage {
                // The new owner can remux intact raw input under its own name.
                fs::write(staged(&f, &request), &mp4).unwrap();
            }
            let result = f
                .repo
                .post_process_video(peer, publish(&request))
                .unwrap()
                .data;
            assert_eq!(
                result.outcome,
                PostProcessVideoOutcome::Published {
                    bytes: mp4.len() as u64
                }
            );
            assert!(visible(&f));
            assert_eq!(fs::read(&target).unwrap(), mp4);
            assert!(!stage.exists());
        }
    }

    #[test]
    fn sealed_screen_failure_is_durable_and_never_sets_video_visibility() {
        let f = fixture_with_screen(None, Some((RawScreenWriterOutcome::Failed, false)));
        let (_owner, request) = owner(&f);
        let peer = std::process::id();
        let failure = |reason| PostProcessVideoRequest {
            media: request.clone(),
            result: PostProcessVideoAction::Unavailable { reason },
        };
        assert!(f
            .repo
            .post_process_video(peer, failure(PostProcessVideoFailure::MuxFailed))
            .is_err());
        let request = failure(PostProcessVideoFailure::MissingRaw);
        let result = f
            .repo
            .post_process_video(peer, request.clone())
            .unwrap()
            .data;
        assert_eq!(
            result.outcome,
            PostProcessVideoOutcome::Unavailable {
                reason: PostProcessVideoFailure::MissingRaw
            }
        );
        assert_eq!(
            f.repo
                .post_process_video(peer, request.clone())
                .unwrap()
                .data,
            result
        );
        let grant = f.repo.post_process_media(peer, request.media).unwrap().data;
        assert_eq!(
            grant.video_failure,
            Some(PostProcessVideoFailure::MissingRaw)
        );
        assert!(!grant.video_publication_candidate);
        assert!(!visible(&f));
    }
}

fn database_bytes(repo: &Repository) -> Vec<Option<Vec<u8>>> {
    ["bazza.db", "bazza.db-wal"]
        .iter()
        .map(|name| match fs::read(repo.store.root().join(name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => panic!("read owned synthetic database bytes: {error}"),
        })
        .collect()
}

fn recording_bytes(directory: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            let name = path.strip_prefix(root).unwrap().to_path_buf();
            if metadata.is_dir() {
                out.insert(name, None);
                visit(root, &path, out);
            } else {
                assert!(metadata.is_file());
                out.insert(name, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut bytes = BTreeMap::new();
    visit(directory, directory, &mut bytes);
    bytes
}

#[test]
fn fixed_seven_artifacts_have_exact_bytes_and_reads_preserve_recordings_and_database() {
    let f = fixture();
    for kind in PostProcessMediaKind::ALL {
        if kind != PostProcessMediaKind::AudioMix {
            fs::write(
                f.directory.join(kind.file_name()),
                format!("synthetic {}", kind.file_name()),
            )
            .unwrap();
        }
    }
    fs::write(
        f.directory.join("unknown.private"),
        b"never grant this file",
    )
    .unwrap();
    let (_owner, request) = owner(&f);
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    let envelope = f
        .repo
        .post_process_media(std::process::id(), request.clone())
        .unwrap();
    assert_eq!(envelope.library_id, f.repo.library_id);
    assert_eq!(envelope.library_epoch, f.repo.epoch);
    let grant = envelope.data;
    assert!(grant.valid_for(&request));
    assert_eq!(grant.artifacts.len(), 7);
    for (artifact, kind) in grant.artifacts.iter().zip(PostProcessMediaKind::ALL) {
        let path = f.directory.join(kind.file_name());
        assert_eq!(artifact.kind, kind);
        assert_eq!(Path::new(&artifact.private_path), path);
        assert_eq!(artifact.bytes, fs::metadata(path).unwrap().len());
    }
    let mut forged = request;
    forged.owner_nonce = Uuid::new_v4();
    assert!(matches!(
        f.repo.post_process_media(std::process::id(), forged),
        Err(CoreError::Data(data::DataError::Forbidden))
    ));
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(database_bytes(&f.repo), database);
}

#[test]
fn soniox_v2_without_screen_evidence_pins_outcome_and_omits_unsealed_screen() {
    let f = fixture_with_soniox(Some(SonioxCaptureOutcome::Complete {}));
    for kind in [
        PostProcessMediaKind::RawVideo,
        PostProcessMediaKind::VideoTimeline,
        PostProcessMediaKind::VideoClock,
        PostProcessMediaKind::AudioClock,
    ] {
        fs::write(f.directory.join(kind.file_name()), b"synthetic").unwrap();
    }
    let (_owner, request) = owner(&f);
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    let grant = f
        .repo
        .post_process_media(std::process::id(), request.clone())
        .unwrap()
        .data;
    assert_eq!(
        grant
            .artifacts
            .iter()
            .map(|artifact| artifact.kind)
            .collect::<Vec<_>>(),
        [
            PostProcessMediaKind::AudioMix,
            PostProcessMediaKind::AudioClock
        ]
    );
    assert_eq!(recording_bytes(&f.directory), recordings);

    let path = f.directory.join(TERMINAL_FILE);
    let original = fs::read(&path).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    changed["soniox_outcome"] =
        serde_json::json!({"status":"incomplete","reason":"provider_failed"});
    for bytes in [
        serde_json::to_vec(&f.terminal).unwrap(),
        serde_json::to_vec(&changed).unwrap(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            f.repo
                .post_process_media(std::process::id(), request.clone()),
            Err(CoreError::Data(data::DataError::Invalid(
                "post-process capture terminal"
            )))
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::write(path, original).unwrap();
    assert!(f
        .repo
        .post_process_media(std::process::id(), request)
        .is_ok());
    assert_eq!(database_bytes(&f.repo), database);
}

#[test]
fn missing_optionals_are_omitted_and_empty_optional_is_reported_as_zero_bytes() {
    let f = fixture();
    let (_owner, request) = owner(&f);
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    let grant = f
        .repo
        .post_process_media(std::process::id(), request.clone())
        .unwrap()
        .data;
    assert_eq!(grant.artifacts.len(), 1);
    assert_eq!(grant.artifacts[0].kind, PostProcessMediaKind::AudioMix);
    assert_eq!(grant.artifacts[0].bytes, wav().len() as u64);
    assert_eq!(recording_bytes(&f.directory), recordings);
    fs::write(
        f.directory.join(PostProcessMediaKind::AudioMic.file_name()),
        [],
    )
    .unwrap();
    let recordings = recording_bytes(&f.directory);
    let grant = f
        .repo
        .post_process_media(std::process::id(), request)
        .unwrap()
        .data;
    assert_eq!(grant.artifacts.len(), 2);
    assert_eq!(grant.artifacts[1].kind, PostProcessMediaKind::AudioMic);
    assert_eq!(grant.artifacts[1].bytes, 0);
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(database_bytes(&f.repo), database);
}

#[test]
fn bad_peer_is_rejected_before_any_media_gate_or_owner_filesystem_access() {
    let f = fixture();
    assert!(f.request.valid());
    let gates = f.repo.store.root().join(GATE_DIRECTORY);
    assert!(!gates.exists());
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    assert!(matches!(
        f.repo
            .post_process_media(std::process::id() + 1, f.request.clone()),
        Err(CoreError::Data(data::DataError::Forbidden))
    ));
    assert!(!gates.exists());
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(database_bytes(&f.repo), database);
}

#[test]
fn missing_or_malformed_mix_and_unsafe_optional_directory_never_produce_a_grant() {
    for fault in 0..3 {
        let f = fixture();
        let (_owner, request) = owner(&f);
        match fault {
            0 => fs::remove_file(f.directory.join("mix.wav")).unwrap(),
            1 => fs::write(f.directory.join("mix.wav"), b"NOT!0000WAVE").unwrap(),
            _ => fs::create_dir(f.directory.join("mic.wav")).unwrap(),
        }
        let database = database_bytes(&f.repo);
        let recordings = recording_bytes(&f.directory);
        let error = f
            .repo
            .post_process_media(std::process::id(), request)
            .unwrap_err();
        match fault {
            0 => assert!(
                matches!(error, CoreError::Io(ref io) if io.kind()==io::ErrorKind::NotFound)
            ),
            1 => assert!(matches!(
                error,
                CoreError::Data(data::DataError::Invalid("post-process WAV header"))
            )),
            _ => assert!(matches!(error, CoreError::Data(data::DataError::Forbidden))),
        }
        assert_eq!(recording_bytes(&f.directory), recordings);
        assert_eq!(database_bytes(&f.repo), database);
    }
}

#[cfg(unix)]
#[test]
fn optional_symlink_refuses_a_grant_without_touching_its_external_target() {
    let f = fixture();
    let (_owner, request) = owner(&f);
    let external = tempfile::tempdir().unwrap();
    let target = external.path().join("preserve.wav");
    fs::write(&target, b"external synthetic bytes").unwrap();
    let link = f.directory.join("mic.wav");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let database = database_bytes(&f.repo);
    assert!(matches!(
        f.repo.post_process_media(std::process::id(), request),
        Err(CoreError::Data(data::DataError::Forbidden))
    ));
    assert_eq!(fs::read(target).unwrap(), b"external synthetic bytes");
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    assert_eq!(database_bytes(&f.repo), database);
}

#[test]
fn terminal_identity_provider_model_and_confirmed_sequence_must_match_stored_capture() {
    for fault in 0..4 {
        let f = fixture();
        let (_owner, request) = owner(&f);
        let mut terminal = f.terminal.clone();
        match fault {
            0 => terminal.identity.worker_session_id = request.proof.session_id,
            1 => terminal.provenance.provider = SttProvider::Soniox,
            2 => terminal.provenance.model_id = Some("other-fixture-model".into()),
            _ => terminal.last_confirmed_sequence += 1,
        }
        // A deliberate synthetic disk fault after real finalization. No read
        // path reopens ArtifactGuard or repairs/reseals this manifest.
        fs::write(
            f.directory.join(TERMINAL_FILE),
            serde_json::to_vec(&terminal).unwrap(),
        )
        .unwrap();
        let database = database_bytes(&f.repo);
        let recordings = recording_bytes(&f.directory);
        let error = f
            .repo
            .post_process_media(std::process::id(), request)
            .unwrap_err();
        if fault == 0 {
            assert!(matches!(
                error,
                CoreError::Handoff(bazza_capture_handoff::HandoffError::IdentityMismatch)
            ));
        } else {
            assert!(matches!(
                error,
                CoreError::Data(data::DataError::Invalid("post-process capture terminal"))
            ));
        }
        assert_eq!(recording_bytes(&f.directory), recordings);
        assert_eq!(database_bytes(&f.repo), database);
    }
}

#[test]
fn renewing_a_claim_reuses_the_owner_but_requires_the_new_job_receipt() {
    let f = fixture();
    let (owner, mut request) = owner(&f);
    assert!(f
        .repo
        .post_process_media(std::process::id(), request.clone())
        .is_ok());
    let renewed = f
        .repo
        .renew_post_process_job(
            std::process::id(),
            jobs::RenewPostProcessJobRequest {
                proof: request.proof.clone(),
                lease: request.lease.clone(),
                operation_id: Uuid::new_v4(),
                fence: request.fence.clone(),
                ttl_seconds: 120,
            },
        )
        .unwrap()
        .data;
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    assert!(matches!(
        f.repo
            .post_process_media(std::process::id(), request.clone()),
        Err(CoreError::Data(data::DataError::Conflict { .. }))
    ));
    assert_ne!(renewed.job_revision, request.lease.job_revision);
    request.lease = renewed;
    assert_eq!(owner.identity(), &identity(&f));
    assert_eq!(owner.nonce(), request.owner_nonce);
    let grant = f
        .repo
        .post_process_media(std::process::id(), request.clone())
        .unwrap()
        .data;
    assert!(grant.valid_for(&request));
    assert_eq!(grant.expires_at, request.lease.expires_at);
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(database_bytes(&f.repo), database);
}

#[test]
fn failed_job_denies_new_grants_and_media_owner_clone_keeps_physical_purge_pending() {
    let f = fixture();
    let (owner, request) = owner(&f);
    let clone = owner.try_clone_file().unwrap();
    f.repo
        .post_process_media(std::process::id(), request.clone())
        .unwrap();
    f.repo
        .fail_post_process_job(
            std::process::id(),
            jobs::FailPostProcessJobRequest {
                proof: request.proof.clone(),
                lease: request.lease.clone(),
                operation_id: Uuid::new_v4(),
                fence: request.fence.clone(),
                error_code: jobs::PostProcessFailureCode::PostProcessFailed,
            },
        )
        .unwrap();
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    assert!(matches!(
        f.repo.post_process_media(std::process::id(), request),
        Err(CoreError::Data(data::DataError::Forbidden))
    ));
    assert_eq!(database_bytes(&f.repo), database);
    let id = f.request.lease.meeting_id.to_string();
    let live = f.repo.store.service_meeting(&id).unwrap().unwrap();
    let deletion = f
        .repo
        .store
        .service_delete_meeting(
            &data::MutationContext {
                principal: "fixture-user".into(),
                idempotency_key: "delete-media".into(),
            },
            &id,
            &live.lifecycle_revision,
        )
        .unwrap();
    let job = deletion.job_id.unwrap();
    assert!(f.repo.store.service_meeting(&id).unwrap().is_none());
    assert!(!f.repo.store.service_run_purge(&job).unwrap());
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(
        f.repo
            .store
            .service_job(&job)
            .unwrap()
            .error_code
            .as_deref(),
        Some("PURGE_RETRY")
    );
    drop(owner);
    assert!(!f.repo.store.service_run_purge(&job).unwrap());
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(f.repo.store.service_job(&job).unwrap().state, "queued");
    drop(clone);
    assert!(f.repo.store.service_run_purge(&job).unwrap());
    assert!(!f.directory.exists());
    assert_eq!(f.repo.store.service_job(&job).unwrap().state, "succeeded");
    assert!(f.repo.store.root().join(GATE_DIRECTORY).is_dir());
}

#[test]
fn revoking_worker_denies_new_grants_without_releasing_an_existing_reader_pin() {
    let f = fixture();
    let (owner, request) = owner(&f);
    let clone = owner.try_clone_file().unwrap();
    f.repo
        .post_process_media(std::process::id(), request.clone())
        .unwrap();
    f.repo
        .store
        .service_close_worker(std::process::id(), f.repo.epoch, &convert(&request.proof))
        .unwrap();
    let database = database_bytes(&f.repo);
    let recordings = recording_bytes(&f.directory);
    assert!(matches!(
        f.repo.post_process_media(std::process::id(), request),
        Err(CoreError::Data(data::DataError::Forbidden))
    ));
    let id = f.request.lease.meeting_id.to_string();
    let live = f.repo.store.service_meeting(&id).unwrap().unwrap();
    assert!(matches!(
        f.repo.store.service_delete_meeting(
            &data::MutationContext {
                principal: "fixture-user".into(),
                idempotency_key: "still-processing".into(),
            },
            &id,
            &live.lifecycle_revision
        ),
        Err(data::DataError::Busy)
    ));
    assert!(matches!(
        MediaPurgeGuard::acquire(f.repo.store.root(), &id),
        Err(MediaGateError::Busy)
    ));
    drop(owner);
    assert!(matches!(
        MediaPurgeGuard::acquire(f.repo.store.root(), &id),
        Err(MediaGateError::Busy)
    ));
    drop(clone);
    drop(MediaPurgeGuard::acquire(f.repo.store.root(), &id).unwrap());
    assert_eq!(recording_bytes(&f.directory), recordings);
    assert_eq!(database_bytes(&f.repo), database);
}

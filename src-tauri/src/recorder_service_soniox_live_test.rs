//! Explicit live provider acceptance using only a generated speech test asset.
use super::*;
use bazza_service_client::{
    api::{
        configuration::{NativeSttProvider, UpdateNativeConfigurationRequest},
        content::DesktopTranscriptRepresentation,
        rpc::ServiceStatus,
        soniox_permission::{SonioxCredential, SonioxKeyOperation},
    },
    repository::RepositoryTranscriptRequest,
    validate_managed_root, RepositoryClient,
};
use std::{
    fs,
    io::{BufRead, BufReader},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    time::Instant,
};

struct OwnedLibrary {
    root: PathBuf,
    id: Uuid,
    service: Child,
}

impl OwnedLibrary {
    fn create(workspace: &Path) -> Self {
        let scratch = workspace.join(".git/local-tests/20261102-mcp");
        let executable = std::env::var_os("BAZZA_TEST_MANAGED_SERVICE_EXE")
            .map(PathBuf::from)
            .unwrap_or_else(|| scratch.join("target/debug/bazza-service.exe"));
        assert!(
            executable.is_absolute() && executable.is_file(),
            "current service executable required"
        );
        let evidence = tempfile::Builder::new()
            .prefix("managed-soniox-live-")
            .tempdir_in(scratch)
            .unwrap()
            .keep();
        let root = evidence.join("library");
        let initialized = Command::new(&executable)
            .args(["init-library", "--library"])
            .arg(&root)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            initialized.status.success(),
            "managed initialization failed"
        );
        let status: ServiceStatus = serde_json::from_slice(&initialized.stdout).unwrap();
        let (root, id) = validate_managed_root(&root).unwrap();
        assert!(!status.development_fixture_only && status.library_id == id);
        let service = Command::new(executable)
            .args(["serve-library", "--library"])
            .arg(&root)
            .args(["--idle-timeout-ms", "60000"])
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut library = Self { root, id, service };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(status) = async_runtime::block_on(async {
                ServiceClient::connect_managed(&library.root)
                    .await?
                    .status()
                    .await
            }) {
                assert_eq!(status.process_id, library.service.id());
                assert_eq!(status.library_id, library.id);
                break;
            }
            assert!(
                library.service.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "owned Soniox test service did not become ready"
            );
            thread::sleep(Duration::from_millis(25));
        }
        println!("Managed Soniox evidence root: {}", evidence.display());
        library
    }
}

impl Drop for OwnedLibrary {
    fn drop(&mut self) {
        if self.service.try_wait().ok().flatten().is_none() {
            // The owned child is the only service admitted for this root.
            let shutdown = async_runtime::block_on(async {
                let mut client = ServiceClient::connect_managed(&self.root).await?;
                if client.status().await?.process_id == self.service.id() {
                    if let Ok(repository) = RepositoryClient::connect_managed(&self.root).await {
                        let _ = repository.soniox_key(SonioxKeyOperation::Clear {}).await;
                    }
                    client.shutdown().await?;
                }
                Ok::<(), ClientError>(())
            });
            if shutdown.is_ok() {
                let deadline = Instant::now() + Duration::from_secs(2);
                while self.service.try_wait().ok().flatten().is_none() && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(20));
                }
            }
            if self.service.try_wait().ok().flatten().is_none() {
                let _ = self.service.kill();
            }
        }
        let _ = self.service.wait();
        // Even a successful Clear can leave old bytes in SQLite free pages or
        // a WAL. Remove only the known database files in this owned test root.
        if validate_managed_root(&self.root)
            .is_ok_and(|(root, id)| root == self.root && id == self.id)
        {
            for leaf in [
                "bazza.db",
                "bazza.db-wal",
                "bazza.db-shm",
                "bazza.db-journal",
            ] {
                if let Err(error) = fs::remove_file(self.root.join(leaf)) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("Disposable Soniox database file could not be removed: {leaf}");
                    }
                }
            }
        }
    }
}

fn designated_key(workspace: &Path) -> SonioxCredential {
    let path = workspace.join("Secrets.txt");
    assert!(
        fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() <= 64 * 1024),
        "designated local credential file is unavailable"
    );
    let reader =
        BufReader::new(fs::File::open(path).expect("open designated local credential file"));
    let mut in_section = false;
    for line in reader.lines() {
        let line = line.expect("read designated local credential format");
        let value = line.trim_start_matches('\u{feff}').trim();
        if value.is_empty() {
            continue;
        }
        if in_section {
            assert!(
                !value.chars().any(char::is_whitespace),
                "invalid designated Soniox credential format"
            );
            return SonioxCredential::parse(value.to_owned())
                .expect("invalid designated Soniox credential format");
        }
        in_section = value.trim_end_matches(':').eq_ignore_ascii_case("Soniox");
    }
    panic!("the designated Soniox credential section is missing");
}

#[test]
#[ignore = "requires BAZZA_TEST_MANAGED_SONIOX_LIVE=1 and the designated credential"]
fn managed_soniox_real_provider_finishes_synthetic_capture() {
    assert!(
        std::env::var_os("BAZZA_TEST_MANAGED_SONIOX_LIVE").as_deref()
            == Some(std::ffi::OsStr::new("1")),
        "set BAZZA_TEST_MANAGED_SONIOX_LIVE=1 to authorize the live synthetic Soniox request"
    );
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap();
    let asset =
        workspace.join(".git/local-tests/20261102-model-acceptance-assets/speech/david-a.wav");
    let pcm = crate::recorder::read_wav_as_16k_mono(&asset).expect("read synthetic WAV");
    assert!(
        (160_000..=192_000).contains(&pcm.len()),
        "unexpected synthetic audio length"
    );
    let library = OwnedLibrary::create(&workspace);
    let owner = ServiceCapture::for_root(library.root.clone(), library.id, true);
    let repository =
        async_runtime::block_on(RepositoryClient::connect_managed(&library.root)).unwrap();
    let model = bazza_asr::soniox::DEFAULT_MODEL.to_owned();
    async_runtime::block_on(async {
        assert!(!repository
            .soniox_key(SonioxKeyOperation::Status {})
            .await
            .unwrap());
        let current = repository.native_configuration().await.unwrap();
        let mut settings = current.data.settings;
        settings.stt_provider = NativeSttProvider::Soniox;
        settings.soniox_consent = true;
        settings.soniox_model = model.clone();
        settings.language = Some("en".into());
        repository
            .update_native_configuration(UpdateNativeConfigurationRequest {
                expected_revision: current.data.revision,
                idempotency_key: Uuid::new_v4().to_string(),
                settings,
            })
            .await
            .unwrap();
        assert!(repository
            .soniox_key(SonioxKeyOperation::Set {
                credential: designated_key(&workspace),
            })
            .await
            .unwrap());
    });

    let (mut client, status) = owner.connect().unwrap();
    let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
    let worker = async_runtime::block_on(client.register_worker(RegisterWorkerRequest {
        registration: registration.clone(),
        ttl_seconds: REGISTRATION_TTL_SECONDS,
    }))
    .unwrap();
    let meeting_id = Uuid::new_v4();
    let mut reservation = Reservation {
        request: PrepareCaptureRequest {
            proof: WorkerProof {
                session_id: worker.session_id,
                library_epoch: worker.library_epoch,
                secret: registration.secret,
            },
            start: CaptureStart {
                capture_id: Uuid::new_v4(),
                meeting_id,
                fence: WorkerSecret::random(),
                title: "Synthetic managed Soniox acceptance".into(),
                app: None,
                started_at: Utc::now(),
                participants: Vec::new(),
                provider: CaptureSelection {
                    provider: CaptureProvider::Soniox,
                    model: Some(model),
                    language: Some("en".into()),
                },
            },
        },
        lease: None,
        guard: None,
        prepare_replied: false,
        begin_sent: false,
    };
    reservation
        .begin(&owner, &mut client, status.library_epoch)
        .unwrap();
    let (directory, artifact) = {
        let guard = reservation.guard.as_ref().unwrap().lock();
        (guard.directory().to_path_buf(), guard.identity().clone())
    };
    let mix = directory.join("mix.wav");
    fs::copy(&asset, &mix).unwrap();
    let mut session = owner.start_soniox(client, &reservation, None);
    let input = session.take_input();
    let mut sent = 0;
    if let Some(CaptionInput::Soniox(control)) = input {
        // No native recorder or device is constructed. Only this fixture's
        // samples cross the actual Soniox WebSocket under the live permit.
        for chunk in pcm.chunks(640) {
            if !control.try_send_pcm(chunk.to_vec()) {
                break;
            }
            sent += chunk.len();
            thread::sleep(Duration::from_millis(40));
        }
    }
    let outcome = session.finish();
    let mut completion = reservation.complete(
        Ok(RecordOutput {
            mic_wav: directory.join("mic.wav"),
            sys_wav: None,
            mix_wav: mix,
            duration: Duration::from_millis((pcm.len() as u64) * 1000 / 16_000),
        }),
        None,
        outcome,
    );
    assert!(completion.commit(&owner).unwrap().job_id.is_some());
    let snapshot =
        bazza_capture_handoff::read_terminal_snapshot_with_evidence(&directory, &artifact).unwrap();
    assert_eq!(snapshot.manifest.provenance.provider, SttProvider::Soniox);
    assert!(
        snapshot.manifest.has_audio
            && !snapshot.manifest.has_video
            && snapshot.raw_screen.is_none()
    );
    assert_eq!(
        sent,
        pcm.len(),
        "provider stopped accepting the synthetic source"
    );
    assert_eq!(
        snapshot.soniox_outcome,
        Some(SonioxCaptureOutcome::Complete {})
    );
    assert!(snapshot.manifest.last_confirmed_sequence > 0);

    async_runtime::block_on(async {
        let repository = RepositoryClient::connect_managed(&library.root)
            .await
            .unwrap();
        let id = meeting_id.to_string();
        let meeting = repository.meeting(id.clone()).await.unwrap().data.unwrap();
        let page = repository
            .transcript(RepositoryTranscriptRequest {
                meeting_id: id,
                source_id: None,
                expected_revision: meeting.transcript_revision,
                representation: DesktopTranscriptRepresentation::Raw,
                cursor: None,
                limit: 100,
                max_bytes: 65_024,
            })
            .await
            .unwrap();
        assert_eq!(page.identity.library_id, library.id);
        assert!(page.next.is_none());
        let text = page
            .items
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.to_ascii_lowercase().contains("monday"),
            "expected synthetic speech absent from Soniox transcript"
        );
        assert!(!repository
            .soniox_key(SonioxKeyOperation::Clear {})
            .await
            .unwrap());
        assert!(!repository
            .soniox_key(SonioxKeyOperation::Status {})
            .await
            .unwrap());
    });
    println!("Managed live Soniox finished and published {} confirmed captions; private credential cleared.", snapshot.manifest.last_confirmed_sequence);
}

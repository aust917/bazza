#![cfg(windows)]
//! Opt-in native Whisper acceptance using a fresh managed root and known WAV.

use bazza_capture_handoff::{
    ArtifactGuard, CaptureIdentity, CaptureTerminalStatus, ConfirmedCaption, ProviderProvenance,
    SttProvider, TerminalManifest, MANIFEST_VERSION,
};
use bazza_core::Repository;
use bazza_service_api::{content::DesktopTranscriptRepresentation, worker::*};
use bazza_service_client::{
    repository::RepositoryTranscriptRequest, RepositoryClient, ServiceClient,
};
use std::{
    env, fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

struct OwnedChild(Child);

impl OwnedChild {
    fn start(exe: &Path, args: &[&str], root: &Path, log: &str) -> Self {
        let stderr = fs::File::create(root.join(log)).unwrap();
        Self(
            Command::new(exe)
                .args(args)
                .arg(root)
                .current_dir(root)
                .env("BAZZA_DATA_DIR", root)
                .env("WEBVIEW2_USER_DATA_FOLDER", root.join("webview-fixture"))
                .env_remove("BAZZA_SMOKE")
                .env_remove("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(stderr)
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .spawn()
                .unwrap(),
        )
    }

    fn assert_running(&mut self, name: &str) {
        assert!(self.0.try_wait().unwrap().is_none(), "owned {name} exited");
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

const LIVE_TEXT: &str = "Original live caption for native speaker acceptance";

async fn stage_wav(
    client: &mut ServiceClient,
    root: &Path,
    library_id: Uuid,
    retain_live: bool,
) -> Uuid {
    let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
    let registered = client
        .register_worker(RegisterWorkerRequest {
            registration: registration.clone(),
            ttl_seconds: 120,
        })
        .await
        .unwrap();
    let proof = WorkerProof {
        session_id: registered.session_id,
        secret: registration.secret,
        library_epoch: registered.library_epoch,
    };
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Managed Whisper acceptance".into(),
        app: None,
        started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        participants: vec![],
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: Some("whisper-base".into()),
            language: Some("en".into()),
        },
    };
    let lease = client
        .prepare_capture(PrepareCaptureRequest {
            proof: proof.clone(),
            start: start.clone(),
        })
        .await
        .unwrap();
    let directory = root.join("recordings").join(lease.meeting_id.to_string());
    let identity = CaptureIdentity {
        library_id,
        meeting_id: lease.meeting_id,
        worker_session_id: lease.worker_session_id,
        generation: lease.generation,
    };
    let mut guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
    client
        .begin_capture(BeginCaptureRequest {
            proof: proof.clone(),
            lease: lease.clone(),
            fence: start.fence.clone(),
        })
        .await
        .unwrap();
    fs::write(
        directory.join("mix.wav"),
        include_bytes!("../../bazza-asr/tests/fixtures/jfk.wav"),
    )
    .unwrap();
    let terminal = TerminalManifest {
        version: MANIFEST_VERSION,
        identity,
        provenance: ProviderProvenance {
            provider: SttProvider::Local,
            model_id: start.provider.model,
        },
        duration_ms: 11_000,
        ended_at: "2026-11-02T00:00:11Z".parse().unwrap(),
        status: CaptureTerminalStatus::Completed,
        error_code: None,
        has_audio: true,
        has_video: false,
        last_confirmed_sequence: u64::from(retain_live),
    };
    if retain_live {
        guard
            .append_local_live_confirmed(&ConfirmedCaption {
                sequence: 1,
                start_ms: 1000,
                end_ms: 8000,
                speaker: None,
                text: LIVE_TEXT.into(),
                provenance: terminal.provenance.clone(),
            })
            .unwrap();
        guard
            .publish_terminal_with_local_live(&terminal, None)
            .unwrap();
    } else {
        guard.publish_terminal(&terminal).unwrap();
    }
    drop(guard);
    let finalized = client
        .finalize_capture(FinalizeCaptureRequest {
            proof,
            lease,
            fence: start.fence,
        })
        .await
        .unwrap();
    assert!(
        finalized.job_id.is_some(),
        "service must queue native post-processing"
    );
    start.meeting_id
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches an explicit packaged Windows desktop on a disposable managed root"]
async fn packaged_managed_desktop_transcribes_known_wav_with_whisper() {
    packaged_managed_desktop_processes_fixture(false).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches a packaged desktop with local speaker models on a disposable library"]
async fn packaged_managed_desktop_adds_speakers_to_original_live_captions() {
    packaged_managed_desktop_processes_fixture(true).await;
}

async fn packaged_managed_desktop_processes_fixture(retain_live: bool) {
    let desktop_exe = PathBuf::from(
        env::var_os("BAZZA_TEST_MANAGED_DESKTOP_EXE")
            .expect("set BAZZA_TEST_MANAGED_DESKTOP_EXE to the current packaged bazza.exe"),
    );
    assert!(desktop_exe.is_absolute() && desktop_exe.is_file());
    assert!(desktop_exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("bazza.exe")));
    // A stale package may treat an unknown flag as ordinary startup. Verify
    // the managed entry point exists before ever launching the desktop.
    let managed_entry =
        b"Use --service-library followed by one absolute managed library directory.";
    assert!(
        fs::read(&desktop_exe)
            .unwrap()
            .windows(managed_entry.len())
            .any(|bytes| bytes == managed_entry),
        "selected desktop lacks managed library admission"
    );
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = workspace.join(".git/local-tests/20261102-mcp");
    let model =
        workspace.join(".git/local-tests/20261102-model-acceptance-assets/models/ggml-base.bin");
    let evidence = tempfile::Builder::new()
        .prefix("managed-whisper-")
        .tempdir_in(scratch)
        .unwrap()
        .keep();
    let root = evidence.join("library");
    let owner = Repository::create_managed(&root).unwrap();
    let library_id = owner.status().library_id;
    drop(owner);
    println!(
        "Managed Whisper acceptance evidence: {}",
        evidence.display()
    );
    let models = root.join("models");
    fs::create_dir_all(&models).unwrap();
    assert_eq!(
        fs::copy(model, models.join("ggml-base.bin")).unwrap(),
        147_951_465
    );
    if retain_live {
        let assets = workspace.join(".git/local-tests/20261102-model-acceptance-assets/models");
        for file in [
            "nemo_en_titanet_small.onnx",
            "pyannote-segmentation-3-0.onnx",
        ] {
            fs::copy(assets.join(file), models.join(file)).unwrap();
        }
    }

    let mut service = OwnedChild::start(
        Path::new(env!("CARGO_BIN_EXE_bazza-service")),
        &["serve-library", "--library"],
        &root,
        "service.stderr.log",
    );
    let mut control = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(client) = ServiceClient::connect_managed(&root).await {
                break client;
            }
            service.assert_running("managed service");
            sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("managed service readiness");
    assert_eq!(control.status().await.unwrap().process_id, service.0.id());
    let meeting_id = stage_wav(&mut control, &root, library_id, retain_live)
        .await
        .to_string();
    let repository = RepositoryClient::connect_managed(&root).await.unwrap();
    let mut desktop = OwnedChild::start(
        &desktop_exe,
        &["--service-library"],
        &root,
        "desktop.stderr.log",
    );
    let meeting = timeout(Duration::from_secs(90), async {
        loop {
            desktop.assert_running("service desktop");
            let meeting = repository
                .meeting(meeting_id.clone())
                .await
                .unwrap()
                .data
                .unwrap();
            assert_ne!(
                meeting.status, "failed",
                "native post-processing failed: {:?}",
                meeting.error
            );
            if meeting.status == "ready" {
                break meeting;
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("desktop did not process the synthetic audio");
    let raw = repository
        .transcript(RepositoryTranscriptRequest {
            meeting_id,
            source_id: None,
            expected_revision: meeting.transcript_revision,
            representation: DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 100,
            max_bytes: 65_024,
        })
        .await
        .unwrap();
    assert_eq!(raw.identity.library_id, library_id);
    assert!(raw.next.is_none());
    if retain_live {
        assert_eq!(raw.items.len(), 1);
        let row = &raw.items[0];
        assert_eq!(
            (&*row.text, row.start_ms, row.end_ms),
            (LIVE_TEXT, 1000, 8000)
        );
        assert_eq!(row.speaker.as_deref(), Some("Speaker 1"));
        println!(
            "Native diarization labeled the initial live row without replacing its text or timing."
        );
        return;
    }
    let text = raw
        .items
        .into_iter()
        .map(|item| item.text)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.to_ascii_lowercase().contains("country"),
        "unexpected Whisper output: {text}"
    );
    println!("Native Whisper published a nonempty JFK transcript to the managed library.");
}

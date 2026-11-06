#![cfg(any(windows, target_os = "macos"))]
//! Real owned service processes and synthetic capture handoff artifacts only.
//! No microphone, native engine, ffmpeg, normal GUI or production library.

#[path = "worker_start/mod.rs"]
mod start_tests;

use bazza_capture_handoff::{
    ArtifactGuard, CaptureIdentity, CaptureTerminalStatus, ConfirmedCaption, ProviderProvenance,
    RawScreenWriterOutcome, SttProvider, TerminalManifest, JOURNAL_FILE, MANIFEST_VERSION,
    OWNER_FILE,
};
use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::{
    rpc::ErrorCode,
    soniox_permission::{
        SonioxIncompleteReason, SonioxPermissionOperation, SonioxPermissionReply,
        SonioxPermissionRequest, FIXTURE_SONIOX_PERMISSION_CAPABILITY,
    },
    worker::*,
};
use bazza_service_client::{ClientError, ServiceClient};
use bazza_service_test_support::create_v8;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(12);
const START_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("capture-handoff-fixture");
        create_v8(&root).unwrap();
        drop(Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap());
        Self {
            _temporary: temporary,
            root,
        }
    }
}
struct OwnedService {
    child: Child,
}
impl OwnedService {
    fn spawn(root: &Path) -> Self {
        Self::spawn_with_idle(root, 10_000)
    }
    fn spawn_with_idle(root: &Path, idle_ms: u64) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-service"));
        command
            .args(["serve-fixture", "--library"])
            .arg(root)
            .arg("--idle-timeout-ms")
            .arg(idle_ms.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        Self {
            child: command.spawn().unwrap(),
        }
    }
    async fn ready(&mut self, root: &Path) -> ServiceClient {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Ok(client) = ServiceClient::connect(root).await {
                return client;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                use std::io::Read;
                let mut error = String::new();
                if let Some(mut stderr) = self.child.stderr.take() {
                    let _ = stderr.read_to_string(&mut error);
                }
                panic!("owned fixture service exited before readiness ({status}): {error}");
            }
            assert!(
                Instant::now() < deadline,
                "fixture service readiness timeout"
            );
            sleep(Duration::from_millis(15)).await;
        }
    }
    fn crash(&mut self) {
        // Authority is the owned Child handle, never an untrusted discovery PID.
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    async fn shutdown(&mut self, mut client: ServiceClient) {
        client.shutdown().await.unwrap();
        drop(client);
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned service shutdown timeout");
    }
}
impl Drop for OwnedService {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

async fn worker(client: &mut ServiceClient) -> (WorkerRegistration, WorkerProof) {
    let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
    let request = RegisterWorkerRequest {
        registration: registration.clone(),
        ttl_seconds: 60,
    };
    let registered = client.register_worker(request.clone()).await.unwrap();
    assert_eq!(registered, client.register_worker(request).await.unwrap());
    assert_eq!(registered.peer_pid, std::process::id());
    let proof = WorkerProof {
        session_id: registered.session_id,
        secret: registration.secret.clone(),
        library_epoch: registered.library_epoch,
    };
    (registration, proof)
}
async fn prepare(client: &mut ServiceClient, proof: &WorkerProof) -> (CaptureStart, CaptureLease) {
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Synthetic capture handoff".into(),
        app: Some("fixture".into()),
        started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        participants: vec!["Sam".into()],
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: Some("fixture-local".into()),
            language: Some("en".into()),
        },
    };
    let request = PrepareCaptureRequest {
        proof: proof.clone(),
        start: start.clone(),
    };
    let lease = client.prepare_capture(request.clone()).await.unwrap();
    assert_eq!(lease, client.prepare_capture(request).await.unwrap());
    (start, lease)
}
async fn begin(
    client: &mut ServiceClient,
    proof: &WorkerProof,
    start: &CaptureStart,
    lease: &CaptureLease,
) {
    let request = BeginCaptureRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: start.fence.clone(),
    };
    assert_eq!(client.begin_capture(request.clone()).await.unwrap(), *lease);
    assert_eq!(client.begin_capture(request).await.unwrap(), *lease);
}
fn provenance() -> ProviderProvenance {
    ProviderProvenance {
        provider: SttProvider::Local,
        model_id: Some("fixture-local".into()),
    }
}
fn identity(library_id: Uuid, lease: &CaptureLease) -> CaptureIdentity {
    CaptureIdentity {
        library_id,
        meeting_id: lease.meeting_id,
        worker_session_id: lease.worker_session_id,
        generation: lease.generation,
    }
}
fn artifact_guard(root: &Path, identity: &CaptureIdentity) -> ArtifactGuard {
    let directory = root
        .join("recordings")
        .join(identity.meeting_id.to_string());
    fs::create_dir_all(&directory).unwrap();
    ArtifactGuard::acquire(&directory, identity).unwrap()
}
fn wav(path: &Path) {
    // One second of synthetic silence, PCM16 / mono / 16 kHz.
    let data_bytes = 32_000_u32;
    let mut bytes = Vec::with_capacity(data_bytes as usize + 44);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(data_bytes + 36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&32_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    bytes.resize(data_bytes as usize + 44, 0);
    fs::write(path, bytes).unwrap();
}
fn append(guard: &mut ArtifactGuard, sequence: u64, text: &str) {
    guard
        .append_confirmed(&ConfirmedCaption {
            sequence,
            start_ms: (sequence - 1) * 400,
            end_ms: sequence * 400,
            speaker: Some("Sam".into()),
            text: text.into(),
            provenance: provenance(),
        })
        .unwrap();
}
fn seal(guard: &mut ArtifactGuard) {
    seal_with_status(guard, CaptureTerminalStatus::Completed);
}
fn seal_with_status(guard: &mut ArtifactGuard, status: CaptureTerminalStatus) {
    wav(&guard.directory().join("mix.wav"));
    let manifest = TerminalManifest {
        version: MANIFEST_VERSION,
        identity: guard.identity().clone(),
        provenance: provenance(),
        duration_ms: 1000,
        ended_at: "2026-11-02T00:00:02Z".parse().unwrap(),
        status,
        error_code: (status == CaptureTerminalStatus::Failed)
            .then(|| "CAPTURE_SYNTHETIC_FAILURE".into()),
        has_audio: true,
        has_video: false,
        last_confirmed_sequence: guard.last_sequence(),
    };
    guard.publish_terminal(&manifest).unwrap();
}
fn error(error: ClientError, code: ErrorCode) {
    assert!(
        matches!(error,ClientError::Remote(ref e) if e.code==code),
        "expected {code:?}, got {error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn private_soniox_permit_requires_begun_owner_and_returns_key_only_on_issue() {
    timeout(TEST_TIMEOUT, async {
        const SYNTHETIC_KEY: &str = "only-synthetic-soniox-credential";
        let fixture = Fixture::new();
        {
            // Seed synthetic preferences before the owned service opens.
            let connection = rusqlite::Connection::open(fixture.root.join("bazza.db")).unwrap();
            for (key, value) in [
                (
                    "settings",
                    serde_json::json!({
                        "stt_provider": "soniox", "soniox_consent": true,
                        "soniox_model": "fixture-soniox", "language": "en"
                    }),
                ),
                ("private:soniox_api_key", serde_json::json!(SYNTHETIC_KEY)),
            ] {
                connection
                    .execute(
                        "INSERT INTO settings(key,value) VALUES(?1,?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        rusqlite::params![key, value.to_string()],
                    )
                    .unwrap();
            }
        }
        let mut host = OwnedService::spawn(&fixture.root);
        let mut client = host.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        assert!(status
            .capabilities
            .iter()
            .any(|capability| capability == FIXTURE_SONIOX_PERMISSION_CAPABILITY));
        let (_, proof) = worker(&mut client).await;
        let start = CaptureStart {
            capture_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            fence: WorkerSecret::random(),
            title: "Synthetic Soniox permission".into(),
            app: None,
            started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
            participants: vec![],
            provider: CaptureSelection {
                provider: CaptureProvider::Soniox,
                model: Some("fixture-soniox".into()),
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
        let request = |operation| SonioxPermissionRequest {
            proof: proof.clone(),
            lease: lease.clone(),
            fence: start.fence.clone(),
            operation,
        };
        let capture_identity = identity(status.library_id, &lease);
        let guard = artifact_guard(&fixture.root, &capture_identity);
        error(
            client
                .soniox_permission(request(SonioxPermissionOperation::Issue {}))
                .await
                .err()
                .unwrap(),
            ErrorCode::PermissionDenied,
        );
        begin(&mut client, &proof, &start, &lease).await;
        drop(guard);
        error(
            client
                .soniox_permission(request(SonioxPermissionOperation::Issue {}))
                .await
                .err()
                .unwrap(),
            ErrorCode::PermissionDenied,
        );
        let guard = artifact_guard(&fixture.root, &capture_identity);
        let issued = client
            .soniox_permission(request(SonioxPermissionOperation::Issue {}))
            .await
            .unwrap();
        assert!(issued.expires_at.unwrap() > std::time::Instant::now());
        assert!(!format!("{:?}", issued.reply).contains(SYNTHETIC_KEY));
        let SonioxPermissionReply::Issued { permit, credential } = issued.reply else {
            panic!("expected Soniox permission");
        };
        assert_eq!(credential.expose(), SYNTHETIC_KEY);
        assert_eq!(permit.capture_id, lease.capture_id);
        let permission_epoch = permit.permission_epoch;
        let renew = request(SonioxPermissionOperation::Renew { permission_epoch });
        assert!(matches!(
            client.soniox_permission(renew.clone()).await.unwrap().reply,
            SonioxPermissionReply::Renewed { .. }
        ));
        let ack = request(SonioxPermissionOperation::AckDrained { permission_epoch });
        assert_eq!(
            client.soniox_permission(ack).await.unwrap().reply,
            SonioxPermissionReply::Drained { permission_epoch }
        );
        let stopped = client.soniox_permission(renew).await.unwrap();
        assert_eq!(
            stopped.reply,
            SonioxPermissionReply::Stop {
                permission_epoch,
                reason: SonioxIncompleteReason::ControlUnavailable
            }
        );
        assert!(stopped.expires_at.is_none());
        drop(guard);
        host.shutdown(client).await;
    })
    .await
    .expect("bounded synthetic Soniox authority test");
}
fn one_job(root: &Path, meeting: Uuid, expected_job: Option<&str>, expected_text: &[&str]) {
    let repository = Repository::open_fixture(root, MigrationPolicy::RequireCurrent).unwrap();
    let changes = repository.data().changes(0, 100).unwrap();
    let jobs: Vec<_> = changes
        .iter()
        .filter(|change| change.entity_kind == "job")
        .collect();
    assert_eq!(
        jobs.len(),
        1,
        "capture finalization must create one job event"
    );
    if let Some(expected) = expected_job {
        assert_eq!(jobs[0].entity_id, expected);
    }
    let job = repository.data().job(&jobs[0].entity_id).unwrap();
    assert_eq!(job.state, "waiting_for_worker");
    let mut request = bazza_core::data::TranscriptRead::default();
    let mut texts = Vec::new();
    loop {
        let source = repository
            .data()
            .transcript(&meeting.to_string(), &request)
            .unwrap();
        assert_eq!(
            source.origin.as_deref(),
            if expected_text.is_empty() {
                None
            } else {
                Some("captured")
            }
        );
        texts.extend(source.segments.into_iter().map(|segment| segment.text));
        if let Some(next) = source.next_offset {
            request.offset = next;
            request.expected_revision = Some(source.state.revision);
        } else {
            break;
        }
    }
    assert_eq!(
        texts.iter().map(String::as_str).collect::<Vec<_>>(),
        expected_text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn finalize_requires_released_artifact_owner_and_lost_ack_retry_is_exact() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        assert_eq!(
            status.schema_version,
            bazza_service_api::rpc::STORAGE_SCHEMA_MAX
        );
        let (_registration, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let begin_replay = BeginCaptureRequest {
            proof: proof.clone(),
            lease: lease.clone(),
            fence: start.fence.clone(),
        };
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        append(&mut guard, 1, "first confirmed caption");
        seal(&mut guard);
        let request = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence,
        };
        error(
            client.finalize_capture(request.clone()).await.unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        assert_eq!(
            client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "recording"
        );
        let owner_path = guard.directory().join(OWNER_FILE);
        let owner_bytes = fs::read(&owner_path).unwrap();
        drop(guard);
        let ack = client.finalize_capture(request.clone()).await.unwrap();
        assert_eq!(ack, client.finalize_capture(request).await.unwrap());
        assert_eq!(fs::read(&owner_path).unwrap(), owner_bytes);
        let terminal_guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        error(
            client.begin_capture(begin_replay).await.unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        drop(terminal_guard);
        assert_eq!(
            client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "processing"
        );
        service.shutdown(client).await;
        one_job(
            &fixture.root,
            lease.meeting_id,
            ack.job_id.as_deref(),
            &["first confirmed caption"],
        );
    })
    .await
    .expect("bounded real capture handoff test");
}

#[tokio::test(flavor = "current_thread")]
async fn failed_screen_writer_preserves_raw_without_claiming_playable_video() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        let directory = guard.directory().to_owned();
        let raw = directory.join("screen.raw.mp4");
        let timeline = directory.join("screen.raw.timeline.json");
        wav(&directory.join("mix.wav"));
        fs::write(&raw, b"partial screen output").unwrap();
        fs::write(&timeline, b"{}").unwrap();
        guard
            .publish_terminal_with_evidence(
                &TerminalManifest {
                    version: MANIFEST_VERSION,
                    identity: guard.identity().clone(),
                    provenance: provenance(),
                    duration_ms: 1000,
                    ended_at: "2026-11-02T00:00:02Z".parse().unwrap(),
                    status: CaptureTerminalStatus::Completed,
                    error_code: None,
                    has_audio: true,
                    has_video: false,
                    last_confirmed_sequence: 0,
                },
                Some(RawScreenWriterOutcome::Failed),
                None,
            )
            .unwrap();
        fs::write(&raw, b"different length").unwrap();
        drop(guard);
        let request = FinalizeCaptureRequest {
            proof,
            lease,
            fence: start.fence,
        };
        error(
            client.finalize_capture(request.clone()).await.unwrap_err(),
            ErrorCode::BackendUnavailable,
        );
        assert_eq!(
            client
                .meeting(request.lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "recording"
        );
        fs::write(&raw, b"partial screen output").unwrap();
        let ack = client.finalize_capture(request.clone()).await.unwrap();
        assert!(ack.job_id.is_some());
        assert_eq!(client.finalize_capture(request.clone()).await.unwrap(), ack);
        assert_eq!(fs::read(&raw).unwrap(), b"partial screen output");
        assert_eq!(fs::read(&timeline).unwrap(), b"{}");
        assert!(!directory.join("screen.mp4").exists());
        service.shutdown(client).await;
        let store = rusqlite::Connection::open(fixture.root.join("bazza.db")).unwrap();
        let row: (bool, String) = store
            .query_row(
                "SELECT m.has_video, s.evidence_json FROM service_capture_terminal_screen s
             JOIN service_capture_leases l ON l.id=s.capture_id
             JOIN meetings m ON m.id=l.meeting_id WHERE l.id=?1",
                [request.lease.capture_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(!row.0);
        let evidence: serde_json::Value = serde_json::from_str(&row.1).unwrap();
        assert_eq!(evidence["writer"], "failed");
        assert_eq!(evidence["raw_mp4_bytes"], b"partial screen output".len());
    })
    .await
    .expect("bounded raw screen handoff test");
}

#[tokio::test(flavor = "current_thread")]
async fn live_worker_reattaches_after_service_crash_and_keeps_appending_confirmed_text() {
    timeout(TEST_TIMEOUT,async {
        let fixture=Fixture::new();let mut service=OwnedService::spawn(&fixture.root);let mut client=service.ready(&fixture.root).await;
        let status=client.status().await.unwrap();let (_registration,old_proof)=worker(&mut client).await;let (start,old_lease)=prepare(&mut client,&old_proof).await;
        let mut guard=artifact_guard(&fixture.root,&identity(status.library_id,&old_lease));begin(&mut client,&old_proof,&start,&old_lease).await;append(&mut guard,1,"before service crash");
        service.crash();drop(client);
        append(&mut guard,2,"local writer continued");
        let mut restarted=OwnedService::spawn(&fixture.root);let mut client=restarted.ready(&fixture.root).await;
        let new_status=client.status().await.unwrap();assert_eq!(new_status.library_id,status.library_id);assert_ne!(new_status.library_epoch,status.library_epoch);
        assert_eq!(client.meeting(old_lease.meeting_id).await.unwrap().unwrap().status,"recording");
        let stale=client.renew_worker(RenewWorkerRequest{proof:old_proof.clone(),ttl_seconds:60}).await.unwrap_err();
        assert!(matches!(stale,ClientError::Remote(ref e) if matches!(e.code,ErrorCode::PermissionDenied|ErrorCode::ResyncRequired)));
        let lease=client.reattach_capture(ReattachCaptureRequest{proof:old_proof.clone(),capture_id:old_lease.capture_id,fence:start.fence.clone(),ttl_seconds:60}).await.unwrap();
        let replay=client.reattach_capture(ReattachCaptureRequest{proof:old_proof.clone(),capture_id:old_lease.capture_id,fence:start.fence.clone(),ttl_seconds:60}).await.unwrap();
        assert_eq!(replay,lease,"lost reattach ACK retry with original proof");
        assert_eq!(lease.generation,old_lease.generation);assert_eq!(lease.meeting_id,old_lease.meeting_id);assert_eq!(lease.library_epoch,new_status.library_epoch);
        let proof=WorkerProof{library_epoch:lease.library_epoch,..old_proof};seal(&mut guard);drop(guard);
        let request=FinalizeCaptureRequest{proof,lease:lease.clone(),fence:start.fence};let ack=client.finalize_capture(request.clone()).await.unwrap();
        assert_eq!(ack,client.finalize_capture(request).await.unwrap());
        restarted.shutdown(client).await;
        one_job(&fixture.root,lease.meeting_id,ack.job_id.as_deref(),&["before service crash","local writer continued"]);
    }).await.expect("bounded real service reattach test");
}

#[tokio::test(flavor = "current_thread")]
async fn durable_journal_larger_than_private_frame_finalizes_without_forwarding_text() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        let texts: Vec<String> = (0..10)
            .map(|n| format!("confirmed{n} {}", "я".repeat(10_000)))
            .collect();
        for (n, text) in texts.iter().enumerate() {
            guard
                .append_confirmed(&ConfirmedCaption {
                    sequence: n as u64 + 1,
                    start_ms: n as u64 * 50,
                    end_ms: (n as u64 + 1) * 50,
                    speaker: Some("Sam".into()),
                    text: text.clone(),
                    provenance: provenance(),
                })
                .unwrap();
        }
        assert!(
            guard
                .directory()
                .join(JOURNAL_FILE)
                .metadata()
                .unwrap()
                .len()
                > 128 * 1024
        );
        seal(&mut guard);
        drop(guard);
        let request = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence,
        };
        assert!(serde_json::to_vec(&request).unwrap().len() < 4096);
        let ack = client.finalize_capture(request.clone()).await.unwrap();
        assert_eq!(ack, client.finalize_capture(request).await.unwrap());
        service.shutdown(client).await;
        one_job(
            &fixture.root,
            lease.meeting_id,
            ack.job_id.as_deref(),
            &texts.iter().map(String::as_str).collect::<Vec<_>>(),
        );
    })
    .await
    .expect("bounded large-journal private RPC test");
}

#[tokio::test(flavor = "current_thread")]
async fn local_recording_handoff_without_an_asr_model_preserves_absence() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_, proof) = worker(&mut client).await;
        let start = CaptureStart {
            capture_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            fence: WorkerSecret::random(),
            title: "Local recording, no model".into(),
            app: None,
            started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
            participants: vec![],
            provider: CaptureSelection {
                provider: CaptureProvider::Local,
                model: None,
                language: None,
            },
        };
        let lease = client
            .prepare_capture(PrepareCaptureRequest {
                proof: proof.clone(),
                start: start.clone(),
            })
            .await
            .unwrap();
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        wav(&guard.directory().join("mix.wav"));
        guard
            .publish_terminal(&TerminalManifest {
                version: MANIFEST_VERSION,
                identity: guard.identity().clone(),
                provenance: ProviderProvenance {
                    provider: SttProvider::Local,
                    model_id: None,
                },
                duration_ms: 1000,
                ended_at: "2026-11-02T00:00:02Z".parse().unwrap(),
                status: CaptureTerminalStatus::Completed,
                error_code: None,
                has_audio: true,
                has_video: false,
                last_confirmed_sequence: 0,
            })
            .unwrap();
        drop(guard);
        let ack = client
            .finalize_capture(FinalizeCaptureRequest {
                proof,
                lease: lease.clone(),
                fence: start.fence,
            })
            .await
            .unwrap();
        service.shutdown(client).await;
        one_job(&fixture.root, lease.meeting_id, ack.job_id.as_deref(), &[]);
    })
    .await
    .expect("bounded no-ASR-model handoff test");
}

#[tokio::test(flavor = "current_thread")]
async fn sealed_handoff_without_finalize_ack_is_recovered_once_on_service_startup() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_registration, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        append(&mut guard, 1, "confirmed before GUI exit");
        seal(&mut guard);
        service.crash();
        drop(client);
        drop(guard);
        let request = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence,
        };
        let mut receipt = None;
        for _ in 0..2 {
            let mut restarted = OwnedService::spawn(&fixture.root);
            let mut client = restarted.ready(&fixture.root).await;
            assert_eq!(
                client
                    .meeting(lease.meeting_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                "processing"
            );
            // Recovery committed before this new connection. An old proof can
            // retrieve its already-committed receipt without acquiring a lease
            // in the new epoch or creating another processing job.
            let replay = client.finalize_capture(request.clone()).await.unwrap();
            if let Some(receipt) = &receipt {
                assert_eq!(receipt, &replay);
            } else {
                receipt = Some(replay);
            }
            restarted.shutdown(client).await;
        }
        one_job(
            &fixture.root,
            lease.meeting_id,
            None,
            &["confirmed before GUI exit"],
        );
    })
    .await
    .expect("bounded pending handoff recovery test");
}

#[tokio::test(flavor = "current_thread")]
async fn failed_partial_handoff_recovers_without_job_and_replays_across_restart() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_registration, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        let caption = " confirmed before the writer failed — Орион ";
        append(&mut guard, 1, caption);
        seal_with_status(&mut guard, CaptureTerminalStatus::Failed);
        let audio_path = guard.directory().join("mix.wav");
        let journal_path = guard.directory().join(JOURNAL_FILE);
        let audio = fs::read(&audio_path).unwrap();
        let journal = fs::read(&journal_path).unwrap();
        service.crash();
        drop(client);
        drop(guard);

        let request = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence,
        };
        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut client = restarted.ready(&fixture.root).await;
        assert_eq!(
            client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        let receipt = client.finalize_capture(request.clone()).await.unwrap();
        assert!(receipt.job_id.is_none());
        assert_eq!(
            client.finalize_capture(request.clone()).await.unwrap(),
            receipt
        );
        restarted.shutdown(client).await;

        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut client = restarted.ready(&fixture.root).await;
        assert_eq!(client.finalize_capture(request).await.unwrap(), receipt);
        restarted.shutdown(client).await;

        assert_eq!(fs::read(audio_path).unwrap(), audio);
        assert_eq!(fs::read(journal_path).unwrap(), journal);
        let repository =
            Repository::open_fixture(&fixture.root, MigrationPolicy::RequireCurrent).unwrap();
        let source = repository
            .data()
            .transcript(&lease.meeting_id.to_string(), &Default::default())
            .unwrap();
        assert_eq!(source.origin.as_deref(), Some("captured"));
        assert_eq!(source.segments.len(), 1);
        assert_eq!(source.segments[0].text, caption);
        let changes = repository.data().changes(0, 100).unwrap();
        assert!(changes.iter().all(|change| change.entity_kind != "job"));
    })
    .await
    .expect("bounded failed handoff recovery and receipt replay test");
}

#[tokio::test(flavor = "current_thread")]
async fn committed_finalize_receipt_replays_with_old_proof_after_service_restart() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_registration, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        append(&mut guard, 1, "confirmed before lost response");
        seal(&mut guard);
        drop(guard);
        let request = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence,
        };
        // Keep the first reply only as the test oracle, then lose the connection
        // and service process. The caller retries its original credentials.
        let original = client.finalize_capture(request.clone()).await.unwrap();
        service.crash();
        drop(client);
        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut client = restarted.ready(&fixture.root).await;
        let new_status = client.status().await.unwrap();
        assert_ne!(new_status.library_epoch, status.library_epoch);
        assert_eq!(
            client.finalize_capture(request.clone()).await.unwrap(),
            original
        );
        assert_eq!(client.finalize_capture(request).await.unwrap(), original);
        restarted.shutdown(client).await;
        one_job(
            &fixture.root,
            lease.meeting_id,
            original.job_id.as_deref(),
            &["confirmed before lost response"],
        );
    })
    .await
    .expect("bounded old proof committed receipt replay test");
}

#[tokio::test(flavor = "current_thread")]
async fn raw_artifact_owner_outlives_client_eof_and_handoff_is_recovered_without_restart() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn_with_idle(&fixture.root, 250);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_registration, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        append(&mut guard, 1, "raw owner remains local");
        let journal = guard.directory().join(JOURNAL_FILE);
        let before = fs::read(&journal).unwrap();
        // No grace period for the 500ms recovery monitor: the raw owner must
        // already prevent the shorter 250ms idle policy from closing the host.
        drop(client);
        sleep(Duration::from_millis(1100)).await;
        assert!(
            service.child.try_wait().unwrap().is_none(),
            "live artifact owner must retain service beyond idle"
        );
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert_eq!(guard.confirmed().len(), 1);
        let mut client = service.ready(&fixture.root).await;
        let current = client.status().await.unwrap();
        assert_eq!(current.process_id, status.process_id);
        assert_eq!(current.library_epoch, status.library_epoch);
        seal(&mut guard);
        drop(guard);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status
                == "processing"
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "live service did not recover released terminal handoff"
            );
            sleep(Duration::from_millis(20)).await;
        }
        service.shutdown(client).await;
        one_job(
            &fixture.root,
            lease.meeting_id,
            None,
            &["raw owner remains local"],
        );
    })
    .await
    .expect("bounded artifact keepalive and live recovery test");
}

#[tokio::test(flavor = "current_thread")]
async fn corrupt_confirmed_journal_cannot_be_replaced_by_a_wire_finalize_claim() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_registration, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        append(&mut guard, 1, "preserve invalid evidence");
        seal(&mut guard);
        let journal = guard.directory().join(JOURNAL_FILE);
        drop(guard);
        let mut incomplete = fs::read(&journal).unwrap();
        let mut bad_checksum = incomplete.clone();
        *bad_checksum.last_mut().unwrap() ^= 1;
        incomplete.extend_from_slice(&[0, 0]);
        let request = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence,
        };
        for bytes in [bad_checksum, incomplete] {
            fs::write(&journal, &bytes).unwrap();
            error(
                client.finalize_capture(request.clone()).await.unwrap_err(),
                ErrorCode::BackendUnavailable,
            );
            assert_eq!(fs::read(&journal).unwrap(), bytes);
        }
        assert_eq!(
            client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "recording"
        );
        service.shutdown(client).await;
    })
    .await
    .expect("bounded corrupt handoff test");
}

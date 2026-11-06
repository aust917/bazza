#![cfg(any(windows, target_os = "macos"))]
//! Owned service-process post-process control with synthetic handoffs only.
//! No audio devices, native engines, screen capture, provider calls or normal GUI.
use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::{
    content::{DesktopTranscriptRepresentation, DesktopTranscriptRequest},
    desktop::*,
    postprocess::*,
    postprocess_input::PostProcessTranscriptRequest,
    postprocess_media::{PostProcessMediaKind, PostProcessMediaRequest},
    postprocess_result::CommitPostProcessAutoTitleRequest,
    rpc::{ErrorCode, STORAGE_SCHEMA_MAX},
    worker::*,
};

use bazza_capture_handoff::{
    ArtifactGuard, CaptureIdentity, CaptureTerminalStatus as TerminalStatus, ConfirmedCaption,
    ProviderProvenance, SttProvider, TerminalManifest, JOURNAL_FILE, MANIFEST_VERSION,
};
use bazza_service_api::rpc::{
    ApiVersion, ClientHello, Reply, ReplyEnvelope, Request, RequestEnvelope, MAX_CALL_MS,
};
use bazza_service_client::postprocess_media::WorkerMediaInputRequest;
use bazza_service_client::{ClientError, ServiceClient};
use bazza_service_test_support::create_v8;
use bazza_service_transport::{connect, read_frame, write_frame};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

#[path = "postprocess_cleanup/mod.rs"]
mod cleanup;
#[path = "postprocess_complete/mod.rs"]
mod complete;
#[path = "postprocess_source/mod.rs"]
mod source;
#[path = "postprocess_speakers/mod.rs"]
mod speakers;
#[path = "postprocess_video/mod.rs"]
mod video;

struct Harness {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    child: Option<Child>,
}
impl Harness {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("postprocess fixture Орион");
        create_v8(&root).unwrap();
        drop(Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap());
        Self {
            _temporary: temporary,
            root,
            child: None,
        }
    }
    async fn start(&mut self) {
        self.start_with_idle(30_000).await;
    }
    async fn start_with_idle(&mut self, idle_ms: u64) {
        assert!(self.child.is_none());
        let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-service"));
        command
            .args(["serve-fixture", "--library"])
            .arg(&self.root)
            .arg("--idle-timeout-ms")
            .arg(idle_ms.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        self.child = Some(command.spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if ServiceClient::connect(&self.root).await.is_ok() {
                break;
            }
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "owned service exited before readiness"
            );
            assert!(Instant::now() < until, "owned service readiness timeout");
            sleep(Duration::from_millis(15)).await;
        }
    }
    fn crash(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    async fn stop(&mut self) {
        let mut control = ServiceClient::connect(&self.root).await.unwrap();
        control.shutdown().await.unwrap();
        drop(control);
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.child.take();
    }
    async fn idle_exit(&mut self, maximum: Duration) {
        timeout(maximum, async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    assert!(status.success(), "owned idle service failed: {status}");
                    self.child.take().unwrap().wait().unwrap();
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .expect("owned service did not finish after its idle/lease boundary");
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn remote(error: ClientError, code: ErrorCode) {
    assert!(
        matches!(error,ClientError::Remote(ref value) if value.code==code),
        "expected{code:?},got{error:?}"
    );
}
async fn worker(
    client: &mut ServiceClient,
    capability: WorkerCapability,
) -> (RegisteredWorker, WorkerProof) {
    let registration = WorkerRegistration::new(vec![capability]);
    let registered = client
        .register_worker(RegisterWorkerRequest {
            registration: registration.clone(),
            ttl_seconds: 120,
        })
        .await
        .unwrap();
    assert_eq!(registered.peer_pid, std::process::id());
    let proof = WorkerProof {
        session_id: registered.session_id,
        secret: registration.secret,
        library_epoch: registered.library_epoch,
    };
    (registered, proof)
}
fn wav() -> Vec<u8> {
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
    bytes
}
struct Capture<JobId = String> {
    meeting_id: Uuid,
    capture_id: Uuid,
    generation: Uuid,
    job_id: JobId,
    finalized_revision: String,
    wave: Vec<u8>,
    journal: Vec<u8>,
}
async fn capture(client: &mut ServiceClient, root: &Path, proof: &WorkerProof) -> Capture {
    capture_with_provider(client, root, proof, CaptureProvider::Local).await
}

async fn capture_with_provider(
    client: &mut ServiceClient,
    root: &Path,
    proof: &WorkerProof,
    provider: CaptureProvider,
) -> Capture {
    let captured = capture_terminal(client, root, proof, true, provider).await;
    Capture {
        meeting_id: captured.meeting_id,
        capture_id: captured.capture_id,
        generation: captured.generation,
        job_id: captured
            .job_id
            .expect("completed capture schedules processing"),
        finalized_revision: captured.finalized_revision,
        wave: captured.wave,
        journal: captured.journal,
    }
}

async fn capture_terminal(
    client: &mut ServiceClient,
    root: &Path,
    proof: &WorkerProof,
    completed: bool,
    provider: CaptureProvider,
) -> Capture<Option<String>> {
    capture_terminal_with_screen(client, root, proof, completed, provider, None).await
}

async fn capture_terminal_with_screen(
    client: &mut ServiceClient,
    root: &Path,
    proof: &WorkerProof,
    completed: bool,
    provider: CaptureProvider,
    raw_screen: Option<&[u8]>,
) -> Capture<Option<String>> {
    let status = client.status().await.unwrap();
    let (journal_provider, model) = match provider {
        CaptureProvider::Local => (SttProvider::Local, "fixture-local"),
        CaptureProvider::Soniox => (SttProvider::Soniox, "fixture-soniox"),
    };
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Synthetic post-process input".into(),
        app: Some("fixture".into()),
        started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        participants: vec![],
        provider: CaptureSelection {
            provider,
            model: Some(model.into()),
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
        library_id: status.library_id,
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
    let provenance = ProviderProvenance {
        provider: journal_provider,
        model_id: Some(model.into()),
    };
    guard
        .append_confirmed(&ConfirmedCaption {
            sequence: 1,
            start_ms: 0,
            end_ms: 400,
            speaker: Some("Sam".into()),
            text: "Original raw caption Орион".into(),
            provenance: provenance.clone(),
        })
        .unwrap();
    let wave = wav();
    fs::write(directory.join("mix.wav"), &wave).unwrap();
    let terminal = TerminalManifest {
        version: MANIFEST_VERSION,
        identity,
        provenance,
        duration_ms: 1000,
        ended_at: "2026-11-02T00:00:02Z".parse().unwrap(),
        status: if completed {
            TerminalStatus::Completed
        } else {
            TerminalStatus::Failed
        },
        error_code: (!completed).then(|| "CAPTURE_SYNTHETIC_FAILURE".into()),
        has_audio: true,
        has_video: false,
        last_confirmed_sequence: 1,
    };
    if let Some(raw) = raw_screen {
        fs::write(directory.join("screen.raw.mp4"), raw).unwrap();
        guard
            .publish_terminal_with_evidence(
                &terminal,
                Some(bazza_capture_handoff::RawScreenWriterOutcome::Stopped),
                None,
            )
            .unwrap();
    } else {
        guard.publish_terminal(&terminal).unwrap();
    }
    let journal = fs::read(directory.join(JOURNAL_FILE)).unwrap();
    drop(guard);
    let request = FinalizeCaptureRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: start.fence,
    };
    let finalized = client.finalize_capture(request.clone()).await.unwrap();
    assert_eq!(finalized, client.finalize_capture(request).await.unwrap());
    Capture {
        meeting_id: lease.meeting_id,
        capture_id: lease.capture_id,
        generation: lease.generation,
        job_id: finalized.job_id,
        finalized_revision: finalized.revision,
        wave,
        journal,
    }
}
fn claim(proof: &WorkerProof, job: PostProcessJob) -> ClaimPostProcessJobRequest {
    ClaimPostProcessJobRequest {
        proof: proof.clone(),
        job,
        claim_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        ttl_seconds: 30,
    }
}
fn transcript_input(
    proof: &WorkerProof,
    lease: &PostProcessJobLease,
    fence: &WorkerSecret,
) -> PostProcessTranscriptRequest {
    PostProcessTranscriptRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: fence.clone(),
        page: DesktopTranscriptRequest {
            meeting_id: lease.meeting_id.to_string(),
            source_id: None,
            expected_revision: lease.input.transcript_revision.clone(),
            representation: DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 1,
            max_bytes: 4096,
        },
    }
}
async fn header(client: &mut ServiceClient, id: Uuid) -> DesktopMeeting {
    client
        .desktop_meeting(DesktopMeetingLookup {
            meeting_id: id.to_string(),
        })
        .await
        .unwrap()
        .data
        .unwrap()
}
fn raw_unchanged<JobId>(root: &Path, capture: &Capture<JobId>) {
    let directory = root.join("recordings").join(capture.meeting_id.to_string());
    assert_eq!(fs::read(directory.join("mix.wav")).unwrap(), capture.wave);
    assert_eq!(
        fs::read(directory.join(JOURNAL_FILE)).unwrap(),
        capture.journal
    );
}
fn durable_failure(
    root: &Path,
    capture: &Capture,
    receipt: &PostProcessFailureReceipt,
    input: &PostProcessSourceFence,
) {
    // The owned service has exited before opening the repository for inspection.
    let repository = Repository::open_fixture(root, MigrationPolicy::RequireCurrent).unwrap();
    let job = repository.data().job(&capture.job_id).unwrap();
    assert_eq!(job.state, "failed");
    assert_eq!(job.revision, receipt.job_revision);
    let code = match receipt.error_code {
        PostProcessFailureCode::PostProcessFailed => "POST_PROCESS_FAILED",
        PostProcessFailureCode::PostProcessInputUnavailable => "POST_PROCESS_INPUT_UNAVAILABLE",
    };
    assert_eq!(job.error_code.as_deref(), Some(code));
    let meeting = repository
        .data()
        .meeting(&capture.meeting_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(meeting.meeting.status.as_str(), "failed");
    assert_eq!(meeting.lifecycle_revision, receipt.lifecycle_revision);
    assert_eq!(meeting.meeting.error.as_deref(), Some(code));
    let state = repository
        .data()
        .transcript_state(&capture.meeting_id.to_string())
        .unwrap();
    assert_eq!(state.revision, input.transcript_revision);
    assert_eq!(state.current_source_id, input.source_id);
    let read = bazza_core::data::TranscriptRead::default();
    let source = repository
        .data()
        .transcript(&capture.meeting_id.to_string(), &read)
        .unwrap();
    assert_eq!(source.segments.len(), 1);
    assert_eq!(source.segments[0].text, "Original raw caption Орион");
    raw_unchanged(root, capture);
}

#[tokio::test(flavor = "current_thread")]
async fn finalized_only_jobs_claim_renew_fail_atomically_and_replay_without_changing_raw_input() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let status = client.status().await.unwrap();
        let (capture_worker, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        assert!(client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap()
            .data
            .items
            .is_empty());
        let first = capture(&mut client, &h.root, &capture_proof).await;
        let second = capture(&mut client, &h.root, &capture_proof).await;
        let failed_capture = capture_terminal(
            &mut client,
            &h.root,
            &capture_proof,
            false,
            CaptureProvider::Local,
        )
        .await;
        assert!(failed_capture.job_id.is_none());
        assert_eq!(
            header(&mut client, failed_capture.meeting_id).await.status,
            "failed"
        );
        let mut request = PostProcessJobsRequest::new(proof.clone());
        request.limit = 1;
        request.max_bytes = 4096;
        let first_page = client
            .post_process_jobs(&registered, request.clone())
            .await
            .unwrap();
        assert_eq!(first_page.library_id, status.library_id);
        assert_eq!(first_page.library_epoch, status.library_epoch);
        assert_eq!(first_page.data.items.len(), 1);
        let original_cursor = first_page.data.next.clone().unwrap();
        request.cursor = Some(original_cursor.clone());
        let second_page = client
            .post_process_jobs(&registered, request)
            .await
            .unwrap();
        assert_eq!(second_page.data.items.len(), 1);
        assert!(second_page.data.next.is_none());
        let mut jobs = first_page.data.items.clone();
        jobs.extend(second_page.data.items);
        let mut expected = vec![first.job_id.clone(), second.job_id.clone()];
        expected.sort();
        assert_eq!(
            jobs.iter()
                .map(|job| job.job_id.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(!jobs
            .iter()
            .any(|job| job.meeting_id == failed_capture.meeting_id));
        let job = jobs
            .iter()
            .find(|job| job.job_id == first.job_id)
            .unwrap()
            .clone();
        assert_eq!(job.input.capture_id, first.capture_id);
        assert_eq!(job.input.capture_generation, first.generation);
        assert_eq!(job.input.lifecycle_revision, first.finalized_revision);
        assert_ne!(job.input.lifecycle_revision, first.generation.to_string());
        assert!(job.input.source_id.is_some());
        assert_eq!(job.provider.model.as_deref(), Some("fixture-local"));

        let mut forged = claim(&proof, job.clone());
        forged.job.input.source_id = Some("foreign-source".into());
        remote(
            client
                .claim_post_process_job(&registered, forged)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let mut wrong_secret = proof.clone();
        wrong_secret.secret = WorkerSecret::random();
        remote(
            client
                .post_process_jobs(&registered, PostProcessJobsRequest::new(wrong_secret))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        // Deliberately alter only client-side metadata to reach the real server's
        // capability check. No role/capability selector is present on the wire.
        let mut fake_registration = capture_worker;
        fake_registration
            .capabilities
            .push(WorkerCapability::PostProcess);
        remote(
            client
                .post_process_jobs(
                    &fake_registration,
                    PostProcessJobsRequest::new(capture_proof),
                )
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let unchanged = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap();
        assert_eq!(
            unchanged.data.snapshot_sequence,
            first_page.data.snapshot_sequence
        );

        let before = header(&mut client, first.meeting_id).await;
        client
            .rename_desktop_meeting(RenameDesktopMeetingRequest {
                meeting_id: first.meeting_id.to_string(),
                expected_revision: before.metadata_revision,
                idempotency_key: Uuid::new_v4().to_string(),
                title: "Renamed while processing".into(),
            })
            .await
            .unwrap();
        let request = claim(&proof, job.clone());
        let fence = request.fence.clone();
        let claimed = client
            .claim_post_process_job(&registered, request.clone())
            .await
            .unwrap();
        assert_eq!(
            claimed.data,
            client
                .claim_post_process_job(&registered, request)
                .await
                .unwrap()
                .data
        );
        let original_input = transcript_input(&proof, &claimed.data, &fence);
        let input_before = client
            .post_process_transcript(&registered, original_input.clone())
            .await
            .unwrap();
        assert_eq!(
            input_before.data.state.revision,
            job.input.transcript_revision
        );
        assert_eq!(input_before.data.source_id, job.input.source_id);
        assert_eq!(input_before.data.items.len(), 1);
        assert_eq!(
            input_before.data.items[0].text,
            "Original raw caption Орион"
        );
        let mut stale_page = PostProcessJobsRequest::new(proof.clone());
        stale_page.limit = 1;
        stale_page.max_bytes = 4096;
        stale_page.cursor = Some(original_cursor);
        remote(
            client
                .post_process_jobs(&registered, stale_page)
                .await
                .unwrap_err(),
            ErrorCode::ResyncRequired,
        );
        let renewal = RenewPostProcessJobRequest {
            proof: proof.clone(),
            lease: claimed.data,
            operation_id: Uuid::new_v4(),
            fence: fence.clone(),
            ttl_seconds: 300,
        };
        let renewed = client
            .renew_post_process_job(&registered, renewal.clone())
            .await
            .unwrap();
        assert!(renewed.data.expires_at <= registered.expires_at);
        assert_eq!(
            renewed.data,
            client
                .renew_post_process_job(&registered, renewal.clone())
                .await
                .unwrap()
                .data
        );

        remote(
            client
                .post_process_transcript(&registered, original_input)
                .await
                .unwrap_err(),
            ErrorCode::Conflict,
        );
        let current_input = transcript_input(&proof, &renewed.data, &fence);
        let input_after = client
            .post_process_transcript(&registered, current_input.clone())
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(input_before.data).unwrap(),
            serde_json::to_value(input_after.data).unwrap()
        );
        let mut archive = current_input.clone();
        archive.page.source_id = Some("archived-source".into());
        remote(
            client
                .post_process_transcript(&registered, archive.clone())
                .await
                .unwrap_err(),
            ErrorCode::InvalidArgument,
        );
        raw_expect_error(
            &h.root,
            Request::PostProcessTranscript(Box::new(archive)),
            ErrorCode::InvalidArgument,
        )
        .await;
        // Keep the DTO internally consistent while forging the pinned source,
        // reaching the server's actual binding check rather than client guards.
        let mut foreign = current_input.clone();
        foreign.lease.input.source_id = Some("foreign-source".into());
        foreign.page.source_id = foreign.lease.input.source_id.clone();
        remote(
            client
                .post_process_transcript(&registered, foreign.clone())
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        raw_expect_error(
            &h.root,
            Request::PostProcessTranscript(Box::new(foreign)),
            ErrorCode::PermissionDenied,
        )
        .await;
        let failure = FailPostProcessJobRequest {
            proof: proof.clone(),
            lease: renewed.data.clone(),
            operation_id: Uuid::new_v4(),
            fence,
            error_code: PostProcessFailureCode::PostProcessInputUnavailable,
        };
        let mut stale_failure = failure.clone();
        stale_failure.lease.input.transcript_revision = "foreign-revision".into();
        remote(
            client
                .fail_post_process_job(&registered, stale_failure)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let still_processing = header(&mut client, first.meeting_id).await;
        assert_eq!(still_processing.status, "processing");
        assert_eq!(
            still_processing.lifecycle_revision,
            job.input.lifecycle_revision
        );
        let receipt = client
            .fail_post_process_job(&registered, failure.clone())
            .await
            .unwrap();
        assert_eq!(
            receipt.data,
            client
                .fail_post_process_job(&registered, failure.clone())
                .await
                .unwrap()
                .data
        );
        remote(
            client
                .post_process_transcript(&registered, current_input)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let retired_fence = failure.fence.clone();
        let mut conflicting = failure;
        conflicting.error_code = PostProcessFailureCode::PostProcessFailed;
        remote(
            client
                .fail_post_process_job(&registered, conflicting)
                .await
                .unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );
        assert_eq!(
            renewed.data,
            client
                .renew_post_process_job(&registered, renewal)
                .await
                .unwrap()
                .data,
            "renew receipt remains historical after fail"
        );
        remote(
            client
                .renew_post_process_job(
                    &registered,
                    RenewPostProcessJobRequest {
                        proof,
                        lease: renewed.data,
                        operation_id: Uuid::new_v4(),
                        fence: retired_fence,
                        ttl_seconds: 30,
                    },
                )
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let after = header(&mut client, first.meeting_id).await;
        assert_eq!(after.status, "failed");
        assert_eq!(after.title, "Renamed while processing");
        assert_eq!(after.lifecycle_revision, receipt.data.lifecycle_revision);
        assert_eq!(after.transcript_revision, job.input.transcript_revision);
        raw_unchanged(&h.root, &first);
        h.stop().await;
        durable_failure(&h.root, &first, &receipt.data, &job.input);
        let repository =
            Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        assert_eq!(
            repository.data().job(&second.job_id).unwrap().state,
            "waiting_for_worker"
        );
        raw_unchanged(&h.root, &failed_capture);
    })
    .await
    .expect("bounded owned post-process lifecycle test");
}

async fn raw_expect_error(root: &Path, request: Request, expected: ErrorCode) {
    // Exercise server-side proof rejection independently of ServiceClient's
    // earlier no-wire rejection of a registration from another epoch.
    let descriptor = bazza_service_client::read_endpoint(root).unwrap();
    let mut stream = connect(&descriptor.endpoint, descriptor.process_id)
        .await
        .unwrap();
    let envelope = |request| RequestEnvelope {
        request_id: Uuid::new_v4(),
        library_epoch: descriptor.library_epoch,
        deadline_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + MAX_CALL_MS,
        request,
    };
    let hello = envelope(Request::Hello(ClientHello {
        api: ApiVersion::CURRENT,
        schema_min: 1,
        schema_max: STORAGE_SCHEMA_MAX,
        build_id: descriptor.build_id.clone(),
        library_id: descriptor.library_id,
    }));
    write_frame(&mut stream, &hello).await.unwrap();
    let response: ReplyEnvelope = read_frame(&mut stream).await.unwrap();
    assert_eq!(response.request_id, hello.request_id);
    assert!(matches!(response.reply, Reply::Hello(_)));
    let call = envelope(request);
    write_frame(&mut stream, &call).await.unwrap();
    let response: ReplyEnvelope = read_frame(&mut stream).await.unwrap();
    assert_eq!(response.request_id, call.request_id);
    assert_eq!(response.library_epoch, descriptor.library_epoch);
    assert!(matches!(response.reply,Reply::Error(error) if error.code==expected));
}

#[tokio::test(flavor = "current_thread")]
async fn restart_reclaims_old_epoch_claim_without_replaying_old_worker_authority() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut old_client = ServiceClient::connect(&h.root).await.unwrap();
        let old_status = old_client.status().await.unwrap();
        let (_, capture_proof) = worker(&mut old_client, WorkerCapability::Capture).await;
        let captured = capture(&mut old_client, &h.root, &capture_proof).await;
        let (old_worker, old_proof) = worker(&mut old_client, WorkerCapability::PostProcess).await;
        let page = old_client
            .post_process_jobs(&old_worker, PostProcessJobsRequest::new(old_proof.clone()))
            .await
            .unwrap();
        assert_eq!(page.data.items.len(), 1);
        let old_request = claim(&old_proof, page.data.items[0].clone());
        let old_lease = old_client
            .claim_post_process_job(&old_worker, old_request.clone())
            .await
            .unwrap()
            .data;
        let old_input = transcript_input(&old_proof, &old_lease, &old_request.fence);
        let input = old_client
            .post_process_transcript(&old_worker, old_input.clone())
            .await
            .unwrap();
        assert_eq!(input.data.items[0].text, "Original raw caption Орион");
        h.crash();
        // Keep the old connected client alive and unpolled until the new listener
        // is bound; closing it first would conceal Windows stale pipe lifetimes.
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let status = client.status().await.unwrap();
        assert_eq!(status.library_id, old_status.library_id);
        assert_ne!(status.library_epoch, old_status.library_epoch);
        assert!(old_client.status().await.is_err());
        remote(
            client
                .claim_post_process_job(&old_worker, old_request.clone())
                .await
                .unwrap_err(),
            ErrorCode::InvalidArgument,
        );
        let old_fence = old_request.fence.clone();
        raw_expect_error(
            &h.root,
            Request::ClaimPostProcessJob(Box::new(old_request)),
            ErrorCode::PermissionDenied,
        )
        .await;
        remote(
            client
                .post_process_transcript(&old_worker, old_input.clone())
                .await
                .unwrap_err(),
            ErrorCode::InvalidArgument,
        );
        raw_expect_error(
            &h.root,
            Request::PostProcessTranscript(Box::new(old_input)),
            ErrorCode::PermissionDenied,
        )
        .await;

        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let page = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap();
        assert_eq!(
            page.data.items.len(),
            1,
            "old epoch is explicitly reclaimable without waiting for TTL"
        );
        assert_eq!(page.data.items[0].revision, old_lease.job_revision);
        assert_eq!(page.data.items[0].input, old_lease.input);
        let request = claim(&proof, page.data.items[0].clone());
        let fence = request.fence.clone();
        let lease = client
            .claim_post_process_job(&registered, request.clone())
            .await
            .unwrap()
            .data;
        assert_ne!(lease.claim_id, old_lease.claim_id);
        assert_ne!(lease.job_revision, old_lease.job_revision);
        assert_eq!(
            lease,
            client
                .claim_post_process_job(&registered, request)
                .await
                .unwrap()
                .data
        );
        let input = client
            .post_process_transcript(&registered, transcript_input(&proof, &lease, &fence))
            .await
            .unwrap();
        assert_eq!(input.data.source_id, lease.input.source_id);
        assert_eq!(input.data.items[0].text, "Original raw caption Орион");
        let mut forged_worker = old_worker;
        forged_worker.library_epoch = status.library_epoch;
        let mut forged_proof = old_proof;
        forged_proof.library_epoch = status.library_epoch;
        let old_fail = FailPostProcessJobRequest {
            proof: forged_proof,
            lease: PostProcessJobLease {
                library_epoch: status.library_epoch,
                ..old_lease
            },
            operation_id: Uuid::new_v4(),
            fence: old_fence,
            error_code: PostProcessFailureCode::PostProcessFailed,
        };
        remote(
            client
                .fail_post_process_job(&forged_worker, old_fail)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let failure = FailPostProcessJobRequest {
            proof,
            lease: lease.clone(),
            operation_id: Uuid::new_v4(),
            fence,
            error_code: PostProcessFailureCode::PostProcessFailed,
        };
        let receipt = client
            .fail_post_process_job(&registered, failure.clone())
            .await
            .unwrap();
        assert_eq!(
            receipt.data,
            client
                .fail_post_process_job(&registered, failure)
                .await
                .unwrap()
                .data
        );
        h.stop().await;
        durable_failure(&h.root, &captured, &receipt.data, &lease.input);
    })
    .await
    .expect("bounded owned post-process restart test");
}

#[tokio::test(flavor = "current_thread")]
async fn idle_waiting_jobs_exit_but_live_registered_claim_retains_until_expiry() {
    timeout(Duration::from_secs(20), async {
        let mut h = Harness::new();
        h.start_with_idle(250).await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let waiting = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof))
            .await
            .unwrap();
        assert_eq!(waiting.data.items.len(), 1);
        drop(client);
        // A registration plus durable waiting work is deliberately idle-safe.
        h.idle_exit(Duration::from_secs(3)).await;

        h.start_with_idle(250).await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let jobs = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap();
        let mut request = claim(&proof, jobs.data.items[0].clone());
        request.ttl_seconds = 5;
        let lease = client
            .claim_post_process_job(&registered, request)
            .await
            .unwrap()
            .data;
        assert_eq!(lease.job_id, captured.job_id);
        drop(client);
        sleep(Duration::from_millis(750)).await;
        assert!(
            h.child.as_mut().unwrap().try_wait().unwrap().is_none(),
            "a live disconnected registered claim must retain service beyond idle250ms"
        );
        // No heartbeat, recovery mutation or connection keeps this host alive.
        // Once its bounded lease expires, normal idle shutdown can proceed.
        h.idle_exit(Duration::from_secs(8)).await;
        let repository =
            Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        let job = repository.data().job(&captured.job_id).unwrap();
        assert_eq!(job.state, "running");
        assert_eq!(job.revision, lease.job_revision);
        raw_unchanged(&h.root, &captured);
    })
    .await
    .expect("bounded registered-claim idle lifecycle test");
}

async fn discard_committed_reply(root: &Path, request: Request) {
    let descriptor = bazza_service_client::read_endpoint(root).unwrap();
    let mut stream = connect(&descriptor.endpoint, descriptor.process_id)
        .await
        .unwrap();
    let envelope = |request| RequestEnvelope {
        request_id: Uuid::new_v4(),
        library_epoch: descriptor.library_epoch,
        deadline_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + MAX_CALL_MS,
        request,
    };
    let hello = envelope(Request::Hello(ClientHello {
        api: ApiVersion::CURRENT,
        schema_min: 1,
        schema_max: STORAGE_SCHEMA_MAX,
        build_id: descriptor.build_id.clone(),
        library_id: descriptor.library_id,
    }));
    write_frame(&mut stream, &hello).await.unwrap();
    let response: ReplyEnvelope = read_frame(&mut stream).await.unwrap();
    assert_eq!(response.request_id, hello.request_id);
    assert!(matches!(response.reply, Reply::Hello(_)));
    write_frame(&mut stream, &envelope(request)).await.unwrap();
    // Host writes this frame only after the operation has committed. Close
    // without reading its outcome: the application retains no acknowledgement.
    let length = stream.read_u32().await.unwrap();
    assert!((1..=128 * 1024).contains(&length));
}

#[tokio::test(flavor = "current_thread")]
async fn automatic_title_results_preserve_claim_fences_and_never_overwrite_manual_titles() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let automatic = capture(&mut client, &h.root, &capture_proof).await;
        let manual = capture(&mut client, &h.root, &capture_proof).await;
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let jobs = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap()
            .data
            .items;
        assert_eq!(jobs.len(), 2);
        let automatic_job = jobs
            .iter()
            .find(|job| job.job_id == automatic.job_id)
            .unwrap()
            .clone();
        let manual_job = jobs
            .iter()
            .find(|job| job.job_id == manual.job_id)
            .unwrap()
            .clone();
        assert_eq!(
            automatic_job.provider.model.as_deref(),
            Some("fixture-local")
        );
        let claim_request = claim(&proof, automatic_job.clone());
        let fence = claim_request.fence.clone();
        let lease = client
            .claim_post_process_job(&registered, claim_request)
            .await
            .unwrap()
            .data;
        let before = header(&mut client, automatic.meeting_id).await;
        assert!(before.title_auto);
        let request = CommitPostProcessAutoTitleRequest {
            proof: proof.clone(),
            lease: lease.clone(),
            fence: fence.clone(),
            operation_id: Uuid::new_v4(),
            expected_metadata_revision: before.metadata_revision.clone(),
            title: "  SapphireAquila generated title Орион  ".into(),
        };
        let bootstrap = client.desktop_bootstrap().await.unwrap().data;
        let mut wrong = request.clone();
        wrong.fence = WorkerSecret::random();
        remote(
            client
                .commit_post_process_auto_title(&registered, wrong.clone())
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        raw_expect_error(
            &h.root,
            Request::CommitPostProcessAutoTitle(Box::new(wrong)),
            ErrorCode::PermissionDenied,
        )
        .await;
        let mut wrong = request.clone();
        wrong.proof.secret = WorkerSecret::random();
        remote(
            client
                .commit_post_process_auto_title(&registered, wrong)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let mut oversized = request.clone();
        oversized.title = "x".repeat(513);
        raw_expect_error(
            &h.root,
            Request::CommitPostProcessAutoTitle(Box::new(oversized)),
            ErrorCode::InvalidArgument,
        )
        .await;
        assert_eq!(
            client
                .desktop_bootstrap()
                .await
                .unwrap()
                .data
                .change_sequence,
            bootstrap.change_sequence,
            "rejected results create no change or receipt side effects"
        );

        discard_committed_reply(
            &h.root,
            Request::CommitPostProcessAutoTitle(Box::new(request.clone())),
        )
        .await;
        assert_eq!(
            header(&mut client, automatic.meeting_id).await.title,
            request.title,
            "the discarded reply belongs to an already committed result"
        );
        let receipt = client
            .commit_post_process_auto_title(&registered, request.clone())
            .await
            .unwrap();
        assert_eq!(receipt.data.operation_id, request.operation_id);
        assert_eq!(receipt.data.claim_id, lease.claim_id);
        assert_eq!(receipt.data.job_id, lease.job_id);
        assert_ne!(receipt.data.metadata_revision, before.metadata_revision);
        let after = header(&mut client, automatic.meeting_id).await;
        assert_eq!(
            after.title, request.title,
            "store the exact generated bytes"
        );
        assert!(after.title_auto);
        assert_eq!(after.metadata_revision, receipt.data.metadata_revision);
        assert_eq!(after.lifecycle_revision, before.lifecycle_revision);
        assert_eq!(after.transcript_revision, before.transcript_revision);
        assert_eq!(after.status, "processing");
        let changes = client
            .desktop_changes(DesktopChangesRequest {
                cursor: Some(DesktopChangeCursor {
                    library_scope: bootstrap.cursor_scope,
                    after: bootstrap.change_sequence,
                }),
                limit: 100,
                max_bytes: 65_024,
            })
            .await
            .unwrap()
            .data;
        assert!(!changes.has_more);
        assert_eq!(
            changes.items.len(),
            1,
            "lost ACK replay must not write twice"
        );
        assert_eq!(changes.items[0].entity_kind, "meeting");
        assert_eq!(changes.items[0].entity_id, automatic.meeting_id.to_string());
        assert_eq!(changes.items[0].revision, receipt.data.metadata_revision);
        assert_eq!(changes.items[0].sequence, receipt.data.change_sequence);
        let mut conflict = request.clone();
        conflict.title = "Different generated result".into();
        remote(
            client
                .commit_post_process_auto_title(&registered, conflict)
                .await
                .unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );
        let input_before = client
            .post_process_transcript(&registered, transcript_input(&proof, &lease, &fence))
            .await
            .unwrap()
            .data;
        let renewed = client
            .renew_post_process_job(
                &registered,
                RenewPostProcessJobRequest {
                    proof: proof.clone(),
                    lease,
                    operation_id: Uuid::new_v4(),
                    fence: fence.clone(),
                    ttl_seconds: 30,
                },
            )
            .await
            .unwrap()
            .data;
        assert_eq!(renewed.input, automatic_job.input);
        let next = CommitPostProcessAutoTitleRequest {
            lease: renewed.clone(),
            operation_id: Uuid::new_v4(),
            expected_metadata_revision: after.metadata_revision,
            title: "ZirconAurora Орион".into(),
            ..request.clone()
        };
        let next_receipt = client
            .commit_post_process_auto_title(&registered, next.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(
            receipt.data,
            client
                .commit_post_process_auto_title(&registered, request)
                .await
                .unwrap()
                .data,
            "original result receipt remains inert history after renewal and another title"
        );
        let input_after = client
            .post_process_transcript(&registered, transcript_input(&proof, &renewed, &fence))
            .await
            .unwrap()
            .data;
        assert_eq!(
            serde_json::to_value(input_before).unwrap(),
            serde_json::to_value(input_after).unwrap()
        );
        let automatic_header = header(&mut client, automatic.meeting_id).await;
        assert_eq!(automatic_header.title, next.title);
        assert!(automatic_header.title_auto);
        assert_eq!(
            automatic_header.metadata_revision,
            next_receipt.metadata_revision
        );
        assert_eq!(
            automatic_header.lifecycle_revision,
            automatic_job.input.lifecycle_revision
        );

        let manual_claim = claim(&proof, manual_job.clone());
        let manual_fence = manual_claim.fence.clone();
        let manual_lease = client
            .claim_post_process_job(&registered, manual_claim)
            .await
            .unwrap()
            .data;
        let original_manual = header(&mut client, manual.meeting_id).await;
        assert!(original_manual.title_auto);
        let mut stale = CommitPostProcessAutoTitleRequest {
            proof: proof.clone(),
            lease: manual_lease.clone(),
            fence: manual_fence.clone(),
            operation_id: Uuid::new_v4(),
            expected_metadata_revision: original_manual.metadata_revision.clone(),
            title: "Must not replace manual title".into(),
        };
        client
            .rename_desktop_meeting(RenameDesktopMeetingRequest {
                meeting_id: manual.meeting_id.to_string(),
                expected_revision: original_manual.metadata_revision,
                idempotency_key: Uuid::new_v4().to_string(),
                title: "Manual TopazNebula".into(),
            })
            .await
            .unwrap();
        let manual_header = header(&mut client, manual.meeting_id).await;
        assert!(!manual_header.title_auto);
        let before_rejections = client
            .desktop_bootstrap()
            .await
            .unwrap()
            .data
            .change_sequence;
        remote(
            client
                .commit_post_process_auto_title(&registered, stale.clone())
                .await
                .unwrap_err(),
            ErrorCode::Conflict,
        );
        stale.expected_metadata_revision = manual_header.metadata_revision.clone();
        remote(
            client
                .commit_post_process_auto_title(&registered, stale)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        assert_eq!(
            client
                .desktop_bootstrap()
                .await
                .unwrap()
                .data
                .change_sequence,
            before_rejections
        );
        let still_manual = header(&mut client, manual.meeting_id).await;
        assert_eq!(still_manual.title, manual_header.title);
        assert_eq!(
            still_manual.metadata_revision,
            manual_header.metadata_revision
        );
        assert!(!still_manual.title_auto);
        assert_eq!(
            client
                .post_process_transcript(
                    &registered,
                    transcript_input(&proof, &manual_lease, &manual_fence),
                )
                .await
                .unwrap()
                .data
                .source_id,
            manual_job.input.source_id
        );
        raw_unchanged(&h.root, &automatic);
        raw_unchanged(&h.root, &manual);
        h.stop().await;

        // Inspect search and job projection only after the owned DB owner exits.
        let repository =
            Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        for (capture, revision) in [
            (&automatic, &renewed.job_revision),
            (&manual, &manual_lease.job_revision),
        ] {
            let job = repository.data().job(&capture.job_id).unwrap();
            assert_eq!(job.state, "running", "a title result does not finish a job");
            assert_eq!(&job.revision, revision);
        }
        for (capture, input) in [
            (&automatic, &automatic_job.input),
            (&manual, &manual_job.input),
        ] {
            let state = repository
                .data()
                .transcript_state(&capture.meeting_id.to_string())
                .unwrap();
            assert_eq!(state.revision, input.transcript_revision);
            assert_eq!(state.current_source_id, input.source_id);
            let meeting = repository
                .data()
                .meeting(&capture.meeting_id.to_string())
                .unwrap()
                .unwrap();
            assert_eq!(meeting.lifecycle_revision, input.lifecycle_revision);
        }
        for (capture, term, metadata_revision) in [
            (&automatic, "ZirconAurora", &next_receipt.metadata_revision),
            (&manual, "TopazNebula", &manual_header.metadata_revision),
        ] {
            let page = repository
                .data()
                .search(&bazza_core::data::SearchRequest {
                    query: term.into(),
                    fields: vec!["title".into()],
                    meeting_ids: vec![capture.meeting_id.to_string()],
                    limit: 10,
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(page.hits.len(), 1);
            assert_eq!(page.hits[0].source_kind, "title");
            assert_eq!(&page.hits[0].revision, metadata_revision);
        }
        let old_title = repository
            .data()
            .search(&bazza_core::data::SearchRequest {
                query: "SapphireAquila".into(),
                fields: vec!["title".into()],
                meeting_ids: vec![automatic.meeting_id.to_string()],
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert!(old_title.hits.is_empty());
        raw_unchanged(&h.root, &automatic);
        raw_unchanged(&h.root, &manual);
    })
    .await
    .expect("bounded owned automatic-title result test");
}

fn captured_directory_bytes(root: &Path, capture: &Capture) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let directory = root.join("recordings").join(capture.meeting_id.to_string());
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

#[tokio::test(flavor = "current_thread")]
async fn media_input_is_guarded_over_rpc_and_purge_waits_for_the_final_read_clone() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let status = client.status().await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let directory = h
            .root
            .join("recordings")
            .join(captured.meeting_id.to_string());
        let microphone = wav();
        let raw_video = b"synthetic undecoded raw video input";
        let timeline = br#"{"version":1,"fps":15,"pauses":[]}"#;
        fs::write(directory.join("mic.wav"), &microphone).unwrap();
        fs::write(directory.join("screen.raw.mp4"), raw_video).unwrap();
        fs::write(directory.join("screen.raw.timeline.json"), timeline).unwrap();
        fs::write(
            directory.join("private-do-not-disclose.txt"),
            b"synthetic private bytes",
        )
        .unwrap();
        let original_bytes = captured_directory_bytes(&h.root, &captured);
        let before = header(&mut client, captured.meeting_id).await;
        assert!(
            !before.has_video,
            "raw bytes are not a playable-video assertion"
        );
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let jobs = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap()
            .data
            .items;
        assert_eq!(jobs.len(), 1);
        let job = jobs[0].clone();
        let request = claim(&proof, job.clone());
        let fence = request.fence.clone();
        let lease = client
            .claim_post_process_job(&registered, request)
            .await
            .unwrap()
            .data;
        let before_reads = client
            .desktop_bootstrap()
            .await
            .unwrap()
            .data
            .change_sequence;
        let input = WorkerMediaInputRequest {
            proof: proof.clone(),
            lease: lease.clone(),
            fence: fence.clone(),
            expected_provider: job.provider.clone(),
        };
        let unrelated_root = tempfile::tempdir().unwrap();
        remote(
            client
                .acquire_post_process_media(&registered, unrelated_root.path(), input.clone())
                .await
                .unwrap_err(),
            ErrorCode::InvalidArgument,
        );
        assert_eq!(
            fs::read_dir(unrelated_root.path()).unwrap().count(),
            0,
            "wrong root is rejected before acquiring its gate"
        );
        let media = client
            .acquire_post_process_media(&registered, &h.root, input.clone())
            .await
            .unwrap();
        assert_eq!(media.envelope().library_id, status.library_id);
        assert_eq!(media.envelope().library_epoch, status.library_epoch);
        assert_eq!(media.grant().input, job.input);
        assert_eq!(media.grant().provider, job.provider);
        assert_eq!(media.grant().job_revision, lease.job_revision);
        assert_eq!(media.grant().expires_at, lease.expires_at);
        assert_eq!(
            media
                .grant()
                .artifacts
                .iter()
                .map(|artifact| artifact.kind)
                .collect::<Vec<_>>(),
            vec![
                PostProcessMediaKind::AudioMix,
                PostProcessMediaKind::AudioMic,
                PostProcessMediaKind::RawVideo,
                PostProcessMediaKind::VideoTimeline,
            ]
        );
        let canonical = directory.canonicalize().unwrap();
        for artifact in &media.grant().artifacts {
            let expected_path = canonical.join(artifact.kind.file_name());
            assert_eq!(Path::new(&artifact.private_path), expected_path.as_path());
            let expected: &[u8] = match artifact.kind {
                PostProcessMediaKind::AudioMix => &captured.wave,
                PostProcessMediaKind::AudioMic => &microphone,
                PostProcessMediaKind::RawVideo => raw_video,
                PostProcessMediaKind::VideoTimeline => timeline,
                other => panic!("unexpected optional fixture artifact: {other:?}"),
            };
            assert_eq!(artifact.bytes, expected.len() as u64);
            assert_eq!(fs::read(&artifact.private_path).unwrap(), expected);
        }
        let nonce = media.grant().owner_nonce;
        let wire = PostProcessMediaRequest {
            proof: proof.clone(),
            lease: lease.clone(),
            fence: fence.clone(),
            owner_nonce: nonce,
            expected_provider: job.provider.clone(),
        };
        let mut forged = wire.clone();
        forged.owner_nonce = Uuid::new_v4();
        raw_expect_error(
            &h.root,
            Request::PostProcessMedia(Box::new(forged)),
            ErrorCode::PermissionDenied,
        )
        .await;
        let mut wrong_provider = wire.clone();
        wrong_provider.expected_provider.model = Some("different-local-model".into());
        remote(
            client
                .post_process_media(&registered, wrong_provider)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        assert_eq!(captured_directory_bytes(&h.root, &captured), original_bytes);
        assert_eq!(
            client
                .desktop_bootstrap()
                .await
                .unwrap()
                .data
                .change_sequence,
            before_reads
        );
        let renewed = client
            .renew_post_process_job(
                &registered,
                RenewPostProcessJobRequest {
                    proof: proof.clone(),
                    lease: lease.clone(),
                    operation_id: Uuid::new_v4(),
                    fence: fence.clone(),
                    ttl_seconds: 30,
                },
            )
            .await
            .unwrap()
            .data;
        assert_ne!(renewed.job_revision, lease.job_revision);
        assert_eq!(renewed.input, lease.input);
        remote(
            client
                .post_process_media(&registered, wire.clone())
                .await
                .unwrap_err(),
            ErrorCode::Conflict,
        );
        let current_wire = PostProcessMediaRequest {
            lease: renewed.clone(),
            ..wire
        };
        // A job renewal changes its CAS revision, not the guard identity. The
        // existing owner can authenticate a new grant without rotating nonce.
        let reused_owner = client
            .post_process_media(&registered, current_wire.clone())
            .await
            .unwrap();
        assert_eq!(reused_owner.data.owner_nonce, nonce);
        assert_eq!(reused_owner.data.artifacts, media.grant().artifacts);
        let renewed_media = client
            .acquire_post_process_media(
                &registered,
                &h.root,
                WorkerMediaInputRequest {
                    lease: renewed.clone(),
                    ..input
                },
            )
            .await
            .unwrap();
        assert_ne!(renewed_media.grant().owner_nonce, nonce);
        assert_eq!(renewed_media.grant().provider, job.provider);
        assert_eq!(renewed_media.grant().artifacts, media.grant().artifacts);
        let clone = renewed_media.try_clone_file().unwrap();
        let after = header(&mut client, captured.meeting_id).await;
        assert_eq!(after.metadata_revision, before.metadata_revision);
        assert_eq!(after.lifecycle_revision, before.lifecycle_revision);
        assert_eq!(after.transcript_revision, before.transcript_revision);
        assert_eq!(after.status, "processing");
        assert!(
            !after.has_video,
            "media reads do not validate or publish raw video"
        );
        assert_eq!(captured_directory_bytes(&h.root, &captured), original_bytes);
        assert_eq!(
            client
                .desktop_bootstrap()
                .await
                .unwrap()
                .data
                .change_sequence,
            renewed.change_sequence
        );
        let failed = client
            .fail_post_process_job(
                &registered,
                FailPostProcessJobRequest {
                    proof,
                    lease: renewed,
                    operation_id: Uuid::new_v4(),
                    fence,
                    error_code: PostProcessFailureCode::PostProcessFailed,
                },
            )
            .await
            .unwrap()
            .data;
        remote(
            client
                .post_process_media(&registered, current_wire)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        h.stop().await;

        // There is no simultaneous database opener: the actual owned service
        // has exited before this separate repository performs logical deletion.
        let repository =
            Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        let deleted = repository
            .data()
            .delete_meeting(
                &bazza_core::data::MutationContext {
                    principal: "synthetic-media-purge".into(),
                    idempotency_key: Uuid::new_v4().to_string(),
                },
                &captured.meeting_id.to_string(),
                &failed.lifecycle_revision,
            )
            .unwrap();
        let purge_id = deleted.job_id.unwrap();
        assert!(!repository.data().run_purge(&purge_id).unwrap());
        let pending = repository.data().job(&purge_id).unwrap();
        assert_eq!(pending.state, "queued");
        assert_eq!(pending.error_code.as_deref(), Some("PURGE_RETRY"));
        assert_eq!(captured_directory_bytes(&h.root, &captured), original_bytes);
        drop(media);
        drop(renewed_media);
        assert!(
            !repository.data().run_purge(&purge_id).unwrap(),
            "the explicit read-file clone still pins the gate"
        );
        assert_eq!(captured_directory_bytes(&h.root, &captured), original_bytes);
        drop(clone);
        assert!(repository.data().run_purge(&purge_id).unwrap());
        assert_eq!(repository.data().job(&purge_id).unwrap().state, "succeeded");
        assert!(!directory.exists());
        assert!(repository
            .data()
            .meeting(&captured.meeting_id.to_string())
            .unwrap()
            .is_none());
        assert!(
            h.root.join(".bazza-service-media-gates").is_dir(),
            "persistent gate coordination survives media purge"
        );
    })
    .await
    .expect("bounded owned media input and purge lifecycle test");
}

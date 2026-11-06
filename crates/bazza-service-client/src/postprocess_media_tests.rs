#![cfg(any(windows, target_os = "macos"))]
use super::*;
use crate::*;
use bazza_capture_handoff::media_read::verify_owner;
use bazza_media_gate::{MediaGateError, MediaPurgeGuard, GATE_DIRECTORY};
use bazza_service_api::{
    postprocess::PostProcessSourceFence,
    postprocess_media::{
        PostProcessMediaArtifact, PostProcessMediaKind, FIXTURE_POST_PROCESS_MEDIA_CAPABILITY,
    },
    worker::{CaptureProvider, WorkerCapability},
};
use bazza_service_transport::Listener;
use std::{future::Future, time::Duration};
use tokio::task::JoinHandle;

fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(12), future)
                .await
                .expect("bounded media-owner fake-peer test");
        });
}

struct Fake {
    root: tempfile::TempDir,
    task: Option<JoinHandle<()>>,
}
impl Fake {
    async fn finish(&mut self) {
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn send(stream: &mut PeerStream, request: &RequestEnvelope, reply: Reply) {
    write_frame(
        stream,
        &ReplyEnvelope {
            request_id: request.request_id,
            library_epoch: request.library_epoch,
            reply,
        },
    )
    .await
    .unwrap();
}

fn fake<F, Fut>(serve: F) -> Fake
where
    F: FnOnce(PeerStream, EndpointDescriptor, PathBuf) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let epoch = Uuid::new_v4();
    let descriptor = EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: ApiVersion::CURRENT,
        process_id: std::process::id(),
        library_id: Uuid::new_v4(),
        library_epoch: epoch,
        build_id: build_id().into(),
        endpoint: session_endpoint_name(&canonical, epoch.as_bytes()).unwrap(),
    };
    let mut listener = Listener::bind(&descriptor.endpoint).unwrap();
    publish_endpoint(&canonical, &descriptor).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.process_id, std::process::id());
        let hello: RequestEnvelope = read_frame(&mut stream).await.unwrap();
        assert!(matches!(hello.request, Request::Hello(_)));
        send(
            &mut stream,
            &hello,
            Reply::Hello(ServiceStatus {
                api: ApiVersion::CURRENT,
                schema_version: STORAGE_SCHEMA_MAX,
                build_id: build_id().into(),
                library_id: descriptor.library_id,
                library_epoch: descriptor.library_epoch,
                process_id: descriptor.process_id,
                authentication: "local_os_user".into(),
                development_fixture_only: true,
                capabilities: vec![
                    FIXTURE_STATUS_CAPABILITY.into(),
                    FIXTURE_METADATA_CAPABILITY.into(),
                    FIXTURE_POST_PROCESS_MEDIA_CAPABILITY.into(),
                ],
            }),
        )
        .await;
        serve(stream, descriptor, canonical).await;
    });
    Fake {
        root,
        task: Some(task),
    }
}

fn input(client: &ServiceClient) -> (RegisteredWorker, WorkerMediaInputRequest) {
    let now = std::time::SystemTime::now();
    let worker = RegisteredWorker {
        session_id: Uuid::new_v4(),
        library_epoch: client.descriptor.library_epoch,
        peer_pid: std::process::id(),
        capabilities: vec![WorkerCapability::PostProcess],
        expires_at: (now + Duration::from_secs(240)).into(),
    };
    let input = WorkerMediaInputRequest {
        proof: WorkerProof {
            session_id: worker.session_id,
            secret: WorkerSecret::random(),
            library_epoch: worker.library_epoch,
        },
        lease: PostProcessJobLease {
            operation_id: Uuid::new_v4(),
            claim_id: Uuid::new_v4(),
            worker_session_id: worker.session_id,
            library_epoch: worker.library_epoch,
            job_id: "media-job".into(),
            meeting_id: Uuid::new_v4(),
            job_revision: "claimed-media".into(),
            input: PostProcessSourceFence {
                checkpoint_revision: "checkpoint-initial".into(),
                capture_id: Uuid::new_v4(),
                capture_generation: Uuid::new_v4(),
                lifecycle_revision: "finalized-life".into(),
                transcript_revision: "original-transcript".into(),
                source_id: Some("captured-source".into()),
            },
            expires_at: (now + Duration::from_secs(120)).into(),
            change_sequence: 9,
            dedup_until: (now + Duration::from_secs(7 * 24 * 60 * 60)).into(),
        },
        fence: WorkerSecret::random(),
        expected_provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: Some("fixture-local".into()),
            language: Some("ru".into()),
        },
    };
    assert!(input.with_nonce(Uuid::from_u128(1)).valid());
    (worker, input)
}

fn identity(
    descriptor: &EndpointDescriptor,
    request: &PostProcessMediaRequest,
) -> MediaReadIdentity {
    MediaReadIdentity {
        library_id: descriptor.library_id,
        library_epoch: descriptor.library_epoch,
        worker_session_id: request.lease.worker_session_id,
        claim_id: request.lease.claim_id,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        capture_id: request.lease.input.capture_id,
        capture_generation: request.lease.input.capture_generation,
        checkpoint_revision: request.lease.input.checkpoint_revision.clone(),
        lifecycle_revision: request.lease.input.lifecycle_revision.clone(),
        transcript_revision: request.lease.input.transcript_revision.clone(),
        source_id: request.lease.input.source_id.clone(),
    }
}

fn grant(root: &Path, request: &PostProcessMediaRequest) -> PostProcessMediaGrant {
    let directory = root
        .join("recordings")
        .join(request.lease.meeting_id.to_string());
    std::fs::create_dir_all(&directory).unwrap();
    let bytes = b"synthetic fixture, never decoded";
    std::fs::write(directory.join("mix.wav"), bytes).unwrap();
    PostProcessMediaGrant {
        library_scope: "a".repeat(32),
        library_epoch: request.proof.library_epoch,
        worker_session_id: request.proof.session_id,
        claim_id: request.lease.claim_id,
        job_id: request.lease.job_id.clone(),
        job_revision: request.lease.job_revision.clone(),
        meeting_id: request.lease.meeting_id,
        input: request.lease.input.clone(),
        owner_nonce: request.owner_nonce,
        provider: request.expected_provider.clone(),
        expires_at: request.lease.expires_at,
        screen_writer: None,
        video_publication_candidate: false,
        video_failure: None,
        artifacts: vec![PostProcessMediaArtifact {
            kind: PostProcessMediaKind::AudioMix,
            private_path: directory.join("mix.wav").to_str().unwrap().into(),
            bytes: bytes.len() as u64,
        }],
    }
}

fn envelope(
    descriptor: &EndpointDescriptor,
    data: PostProcessMediaGrant,
) -> DesktopEnvelope<PostProcessMediaGrant> {
    DesktopEnvelope {
        library_id: descriptor.library_id,
        library_epoch: descriptor.library_epoch,
        data,
    }
}

async fn no_extra_request(stream: &mut PeerStream) {
    assert!(tokio::time::timeout(
        Duration::from_millis(120),
        read_frame::<RequestEnvelope>(stream)
    )
    .await
    .is_err());
}

#[test]
fn owned_grant_retains_real_gate_and_explicit_file_clone_until_last_drop() {
    run(async {
        let mut server = fake(|mut stream, descriptor, root| async move {
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessMedia(request) = &frame.request else {
                panic!("media request")
            };
            let proof_identity = identity(&descriptor, request);
            let nonce = request.owner_nonce;
            let proof_root = root.clone();
            tokio::task::spawn_blocking(move || {
                verify_owner(&proof_root, &proof_identity, nonce, std::process::id())
            })
            .await
            .unwrap()
            .unwrap();
            assert!(matches!(
                MediaPurgeGuard::acquire(&root, &request.lease.meeting_id.to_string()),
                Err(MediaGateError::Busy)
            ));
            let data = grant(&root, request);
            send(
                &mut stream,
                &frame,
                Reply::PostProcessMedia(Box::new(envelope(&descriptor, data))),
            )
            .await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, input) = input(&client);
        let meeting = input.lease.meeting_id.to_string();
        let owned = client
            .acquire_post_process_media(&worker, server.root.path(), input.clone())
            .await
            .unwrap();
        assert_eq!(owned.envelope().library_id, client.descriptor.library_id);
        assert_eq!(owned.grant().input, input.lease.input);
        assert_eq!(owned.grant().provider, input.expected_provider);
        assert!(matches!(
            MediaPurgeGuard::acquire(server.root.path(), &meeting),
            Err(MediaGateError::Busy)
        ));
        let debug = format!("{owned:?}");
        assert!(!debug.contains(&owned.grant().artifacts[0].private_path));
        assert!(!debug.contains(input.proof.secret.expose()));
        let inherited = owned.try_clone_file().unwrap();
        drop(owned);
        assert!(matches!(
            MediaPurgeGuard::acquire(server.root.path(), &meeting),
            Err(MediaGateError::Busy)
        ));
        drop(inherited);
        drop(MediaPurgeGuard::acquire(server.root.path(), &meeting).unwrap());
        server.finish().await;
    });
}

#[test]
fn invalid_registration_and_foreign_root_create_no_owner_and_send_no_request() {
    run(async {
        let mut server = fake(|mut stream, _, _| async move {
            no_extra_request(&mut stream).await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, original) = input(&client);
        let foreign = tempfile::tempdir().unwrap();
        for fault in 0..7 {
            let mut worker = worker.clone();
            let mut input = original.clone();
            let root = match fault {
                0 => foreign.path(),
                _ => server.root.path(),
            };
            match fault {
                1 => worker.peer_pid = 0,
                2 => worker.library_epoch = Uuid::new_v4(),
                3 => worker.capabilities = vec![WorkerCapability::Capture],
                4 => input.proof.session_id = Uuid::new_v4(),
                5 => {
                    input.lease.expires_at =
                        (std::time::SystemTime::now() - Duration::from_secs(1)).into()
                }
                6 => input.expected_provider.model = Some("../invalid".into()),
                _ => {}
            }
            assert!(
                matches!(client.acquire_post_process_media(&worker,root,input.clone()).await,
                Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument),
                "fault {fault}"
            );
            if fault != 0 {
                assert!(
                    matches!(client.post_process_media(&worker,input.with_nonce(Uuid::from_u128(1))).await,
                    Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument),
                    "low-level fault {fault}"
                );
            }
            assert!(client.stream.is_some());
            assert!(!server.root.path().join(GATE_DIRECTORY).exists());
            assert!(!foreign.path().join(GATE_DIRECTORY).exists());
        }
        server.finish().await;
    });
}

#[test]
fn local_gate_busy_is_safe_resource_busy_without_wire_or_connection_poisoning() {
    run(async {
        let mut server = fake(|mut stream, _, _| async move {
            no_extra_request(&mut stream).await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, input) = input(&client);
        let exclusive =
            MediaPurgeGuard::acquire(server.root.path(), &input.lease.meeting_id.to_string())
                .unwrap();
        let error = client
            .acquire_post_process_media(&worker, server.root.path(), input)
            .await
            .unwrap_err();
        assert!(matches!(&error,ClientError::Remote(error) if error.code==ErrorCode::ResourceBusy));
        assert!(!format!("{error}").contains(server.root.path().to_str().unwrap()));
        assert!(client.stream.is_some());
        drop(exclusive);
        server.finish().await;
    });
}

#[test]
fn malformed_local_ownership_is_safe_backend_error_and_preserves_existing_entry() {
    run(async {
        let mut server = fake(|mut stream, _, _| async move {
            no_extra_request(&mut stream).await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, input) = input(&client);
        let malformed = server.root.path().join(GATE_DIRECTORY);
        std::fs::write(&malformed, b"preserve malformed coordination entry").unwrap();
        let error = client
            .acquire_post_process_media(&worker, server.root.path(), input)
            .await
            .unwrap_err();
        assert!(
            matches!(&error,ClientError::Remote(error) if error.code==ErrorCode::BackendUnavailable)
        );
        assert!(!format!("{error}").contains(server.root.path().to_str().unwrap()));
        assert_eq!(
            std::fs::read(&malformed).unwrap(),
            b"preserve malformed coordination entry"
        );
        assert!(client.stream.is_some());
        server.finish().await;
    });
}

#[test]
fn forged_identity_nonce_provider_expiry_kind_or_exact_path_poisons_and_releases_owner() {
    run(async {
        for fault in 0..10 {
            let mut server = fake(move |mut stream, descriptor, root| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessMedia(request) = &frame.request else {
                    panic!("media request")
                };
                let mut data = grant(&root, request);
                match fault {
                    0 => data.meeting_id = Uuid::new_v4(),
                    1 => data.owner_nonce = Uuid::new_v4(),
                    2 => data.provider.language = Some("en".into()),
                    3 => {
                        data.expires_at =
                            (std::time::SystemTime::now() + Duration::from_secs(300)).into()
                    }
                    4 => data.artifacts[0].kind = PostProcessMediaKind::AudioMic,
                    5 => {
                        data.artifacts[0].private_path = root
                            .join("recordings")
                            .join(Uuid::new_v4().to_string())
                            .join("mix.wav")
                            .to_str()
                            .unwrap()
                            .into()
                    }
                    6 => data.job_revision = "refreshed-job".into(),
                    7 => data.artifacts.push(data.artifacts[0].clone()),
                    8 => {
                        let expected = data.artifacts[0].private_path.clone();
                        let directory = expected.strip_suffix("mix.wav").unwrap();
                        let separator = std::path::MAIN_SEPARATOR;
                        // Verbatim Windows PathBuf::push normalizes '..'. Keep
                        // this wire forgery literal so the exact-path guard runs.
                        data.artifacts[0].private_path = format!(
                            "{directory}..{separator}{}{separator}mix.wav",
                            request.lease.meeting_id
                        );
                        assert_ne!(data.artifacts[0].private_path, expected);
                        assert!(data.valid_for(request));
                    }
                    _ => {}
                }
                let mut value = envelope(&descriptor, data);
                if fault == 9 {
                    value.library_id = Uuid::new_v4();
                }
                send(
                    &mut stream,
                    &frame,
                    Reply::PostProcessMedia(Box::new(value)),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let (worker, input) = input(&client);
            let meeting = input.lease.meeting_id.to_string();
            assert!(
                matches!(
                    client
                        .acquire_post_process_media(&worker, server.root.path(), input)
                        .await,
                    Err(ClientError::Protocol)
                ),
                "fault {fault}"
            );
            assert!(client.stream.is_none());
            drop(MediaPurgeGuard::acquire(server.root.path(), &meeting).unwrap());
            server.finish().await;
        }
    });
}

#[test]
fn domain_conflict_has_no_implicit_retry_and_drops_owner_without_poisoning() {
    run(async {
        let mut server = fake(|mut stream, _, _| async move {
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.request, Request::PostProcessMedia(_)));
            send(
                &mut stream,
                &frame,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            no_extra_request(&mut stream).await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, input) = input(&client);
        let meeting = input.lease.meeting_id.to_string();
        assert!(
            matches!(client.acquire_post_process_media(&worker,server.root.path(),input).await,
            Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        assert!(client.stream.is_some());
        drop(MediaPurgeGuard::acquire(server.root.path(), &meeting).unwrap());
        server.finish().await;
    });
}

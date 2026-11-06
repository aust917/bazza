#![cfg(any(windows, target_os = "macos"))]

#[path = "postprocess_complete_tests.rs"]
mod postprocess_complete_tests;
#[path = "postprocess_source_tests.rs"]
mod postprocess_source_tests;
#[path = "postprocess_speakers_tests.rs"]
mod postprocess_speakers_tests;
#[path = "worker_start_tests.rs"]
mod worker_start_tests;

use super::*;
use bazza_service_transport::{current_user_id, Listener};
use std::future::Future;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use uuid::Uuid;

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(2), test)
                .await
                .expect("synthetic client test must not wait for production deadlines");
        });
}

struct FakeService {
    root: tempfile::TempDir,
    descriptor: EndpointDescriptor,
    task: JoinHandle<()>,
}

impl Drop for FakeService {
    fn drop(&mut self) {
        // This task and pipe belong only to this disposable fake service. Abort
        // also runs if an assertion panics or the outer test deadline expires.
        self.task.abort();
    }
}

fn fake_service<F, Fut>(serve: F) -> FakeService
where
    F: FnOnce(PeerStream, EndpointDescriptor) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let library_epoch = Uuid::new_v4();
    let descriptor = EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: ApiVersion::CURRENT,
        process_id: std::process::id(),
        library_id: Uuid::new_v4(),
        library_epoch,
        build_id: build_id().into(),
        endpoint: session_endpoint_name(root.path(), library_epoch.as_bytes()).unwrap(),
    };
    let mut listener = Listener::bind(&descriptor.endpoint).unwrap();
    publish_endpoint(root.path(), &descriptor).unwrap();
    let server_descriptor = descriptor.clone();
    let task = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.process_id, std::process::id());
        assert_eq!(peer.os_user, current_user_id().unwrap());
        serve(stream, server_descriptor).await;
    });
    FakeService {
        root,
        descriptor,
        task,
    }
}

fn status(descriptor: &EndpointDescriptor) -> ServiceStatus {
    ServiceStatus {
        api: ApiVersion::CURRENT,
        schema_version: 9,
        build_id: build_id().into(),
        library_id: descriptor.library_id,
        library_epoch: descriptor.library_epoch,
        process_id: descriptor.process_id,
        authentication: "local_os_user".into(),
        capabilities: vec![
            FIXTURE_STATUS_CAPABILITY.into(),
            FIXTURE_METADATA_CAPABILITY.into(),
        ],
        development_fixture_only: true,
    }
}

fn reply(request: &RequestEnvelope, kind: Reply) -> ReplyEnvelope {
    ReplyEnvelope {
        request_id: request.request_id,
        library_epoch: request.library_epoch,
        reply: kind,
    }
}

async fn accept_hello(stream: &mut PeerStream, descriptor: &EndpointDescriptor) {
    let request: RequestEnvelope = read_frame(stream).await.unwrap();
    assert!(matches!(request.request, Request::Hello(_)));
    assert_eq!(request.library_epoch, descriptor.library_epoch);
    write_frame(stream, &reply(&request, Reply::Hello(status(descriptor))))
        .await
        .unwrap();
}

async fn assert_no_reuse(client: &mut ServiceClient) {
    assert!(client.stream.is_none(), "failed transport must be dropped");
    assert!(matches!(client.status().await, Err(ClientError::Protocol)));
    assert!(client.stream.is_none());
}

#[test]
fn compatible_authenticated_hello_and_status_keep_the_connection_usable() {
    run(async {
        let mut server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::GetStatus));
            write_frame(
                &mut stream,
                &reply(&request, Reply::Status(status(&descriptor))),
            )
            .await
            .unwrap();
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let result = client.status().await.unwrap();
        assert_eq!(result.library_id, server.descriptor.library_id);
        assert_eq!(result.library_epoch, server.descriptor.library_epoch);
        assert_eq!(result.schema_version, 9);
        assert!(client.stream.is_some());
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn typed_worker_roundtrip_preserves_credentials_and_uses_expected_reply_kinds() {
    run(async {
        let mut server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let mut captured = None;
            for operation in 0..6 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let response = match (&request.request, operation) {
                    (Request::RegisterWorker(input), 0) => {
                        Reply::RegisteredWorker(RegisteredWorker {
                            session_id: input.registration.session_id,
                            library_epoch: descriptor.library_epoch,
                            peer_pid: std::process::id(),
                            capabilities: input.registration.capabilities.clone(),
                            expires_at: "2099-01-01T00:00:00Z".parse().unwrap(),
                        })
                    }
                    (Request::RenewWorker(input), 1) => Reply::RegisteredWorker(RegisteredWorker {
                        session_id: input.proof.session_id,
                        library_epoch: descriptor.library_epoch,
                        peer_pid: std::process::id(),
                        capabilities: vec![WorkerCapability::Capture],
                        expires_at: "2099-01-01T00:00:00Z".parse().unwrap(),
                    }),
                    (Request::PrepareCapture(input), 2) => {
                        let lease = CaptureLease {
                            capture_id: input.start.capture_id,
                            meeting_id: input.start.meeting_id,
                            worker_session_id: input.proof.session_id,
                            generation: Uuid::new_v4(),
                            library_epoch: descriptor.library_epoch,
                        };
                        captured = Some(lease.clone());
                        Reply::CaptureLease(lease)
                    }
                    (Request::ReattachCapture(input), 3) => {
                        let lease = captured.clone().unwrap();
                        assert_eq!(lease.capture_id, input.capture_id);
                        Reply::CaptureLease(lease)
                    }
                    (Request::BeginCapture(input), 4) => {
                        assert_eq!(captured.as_ref(), Some(&input.lease));
                        Reply::CaptureBegun(input.lease.clone())
                    }
                    (Request::FinalizeCapture(input), 5) => {
                        Reply::WorkerMutationOutcome(WorkerMutationOutcome {
                            entity_id: input.lease.meeting_id.to_string(),
                            // The semantic lifecycle transition returns a new
                            // opaque revision, not its input capture generation.
                            revision: Uuid::new_v4().to_string(),
                            change_sequence: 7,
                            job_id: Some(Uuid::new_v4().to_string()),
                            dedup_until: "2099-01-01T00:00:00Z".parse().unwrap(),
                        })
                    }
                    _ => panic!("unexpected typed worker request"),
                };
                write_frame(&mut stream, &reply(&request, response))
                    .await
                    .unwrap();
            }
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
        let worker = client
            .register_worker(RegisterWorkerRequest {
                registration: registration.clone(),
                ttl_seconds: 60,
            })
            .await
            .unwrap();
        let proof = WorkerProof {
            session_id: worker.session_id,
            secret: registration.secret.clone(),
            library_epoch: worker.library_epoch,
        };
        let renewed = client
            .renew_worker(RenewWorkerRequest {
                proof: proof.clone(),
                ttl_seconds: 60,
            })
            .await
            .unwrap();
        assert_eq!(renewed.session_id, worker.session_id);
        let start = CaptureStart {
            capture_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            fence: WorkerSecret::random(),
            title: "fixture recording".into(),
            app: None,
            started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
            participants: vec![],
            provider: CaptureSelection {
                provider: CaptureProvider::Local,
                model: Some("local".into()),
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
        assert_eq!(lease.meeting_id, start.meeting_id);
        let attached = client
            .reattach_capture(ReattachCaptureRequest {
                proof: proof.clone(),
                capture_id: lease.capture_id,
                fence: start.fence.clone(),
                ttl_seconds: 60,
            })
            .await
            .unwrap();
        assert_eq!(
            client
                .begin_capture(BeginCaptureRequest {
                    proof: proof.clone(),
                    lease: attached.clone(),
                    fence: start.fence.clone()
                })
                .await
                .unwrap(),
            attached
        );
        let outcome = client
            .finalize_capture(FinalizeCaptureRequest {
                proof,
                lease: attached.clone(),
                fence: start.fence,
            })
            .await
            .unwrap();
        assert_eq!(outcome.entity_id, attached.meeting_id.to_string());
        assert!(client.stream.is_some());
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn forged_worker_registration_identity_discards_the_stream() {
    run(async {
        for variant in ["pid", "session", "epoch", "capabilities"] {
            let server = fake_service(move |mut stream, descriptor| async move {
                accept_hello(&mut stream, &descriptor).await;
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::RegisterWorker(input) = &request.request else {
                    panic!("expected worker registration")
                };
                let mut worker = RegisteredWorker {
                    session_id: input.registration.session_id,
                    library_epoch: descriptor.library_epoch,
                    peer_pid: std::process::id(),
                    capabilities: input.registration.capabilities.clone(),
                    expires_at: "2099-01-01T00:00:00Z".parse().unwrap(),
                };
                match variant {
                    "pid" => worker.peer_pid = worker.peer_pid.wrapping_add(1),
                    "session" => worker.session_id = Uuid::new_v4(),
                    "epoch" => worker.library_epoch = Uuid::new_v4(),
                    "capabilities" => worker.capabilities.clear(),
                    _ => unreachable!(),
                }
                write_frame(
                    &mut stream,
                    &reply(&request, Reply::RegisteredWorker(worker)),
                )
                .await
                .unwrap();
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let request = RegisterWorkerRequest {
                registration: WorkerRegistration::new(vec![WorkerCapability::Capture]),
                ttl_seconds: 60,
            };
            assert!(
                matches!(
                    client.register_worker(request).await,
                    Err(ClientError::Protocol)
                ),
                "{variant}"
            );
            assert_no_reuse(&mut client).await;
        }
    });
}

#[test]
fn forged_capture_lease_identity_discards_the_stream() {
    run(async {
        for variant in ["capture", "meeting", "session", "generation", "epoch"] {
            let server = fake_service(move |mut stream, descriptor| async move {
                accept_hello(&mut stream, &descriptor).await;
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PrepareCapture(input) = &request.request else {
                    panic!("expected prepare capture")
                };
                let mut lease = CaptureLease {
                    capture_id: input.start.capture_id,
                    meeting_id: input.start.meeting_id,
                    worker_session_id: input.proof.session_id,
                    generation: Uuid::new_v4(),
                    library_epoch: descriptor.library_epoch,
                };
                match variant {
                    "capture" => lease.capture_id = Uuid::new_v4(),
                    "meeting" => lease.meeting_id = Uuid::new_v4(),
                    "session" => lease.worker_session_id = Uuid::new_v4(),
                    "generation" => lease.generation = Uuid::nil(),
                    "epoch" => lease.library_epoch = Uuid::new_v4(),
                    _ => unreachable!(),
                }
                write_frame(&mut stream, &reply(&request, Reply::CaptureLease(lease)))
                    .await
                    .unwrap();
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let request = PrepareCaptureRequest {
                proof: WorkerProof {
                    session_id: Uuid::new_v4(),
                    secret: WorkerSecret::random(),
                    library_epoch: server.descriptor.library_epoch,
                },
                start: CaptureStart {
                    capture_id: Uuid::new_v4(),
                    meeting_id: Uuid::new_v4(),
                    fence: WorkerSecret::random(),
                    title: "fixture".into(),
                    app: None,
                    started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
                    participants: vec![],
                    provider: CaptureSelection {
                        provider: CaptureProvider::Local,
                        model: Some("local".into()),
                        language: None,
                    },
                },
            };
            assert!(
                matches!(
                    client.prepare_capture(request).await,
                    Err(ClientError::Protocol)
                ),
                "{variant}"
            );
            assert_no_reuse(&mut client).await;
        }
    });
}

#[test]
fn timeout_after_a_partial_status_frame_discards_the_stream() {
    run(async {
        let server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::GetStatus));
            stream.write_u32(100).await.unwrap();
            stream.write_all(b"{\"").await.unwrap();
            stream.flush().await.unwrap();
            std::future::pending::<()>().await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        client.timeout = Duration::from_millis(20);
        assert!(matches!(client.status().await, Err(ClientError::Timeout)));
        assert_no_reuse(&mut client).await;
    });
}

#[test]
fn externally_cancelling_a_partial_status_frame_discards_the_stream() {
    run(async {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let partial_written = Arc::new(AtomicBool::new(false));
        let partial_received = partial_written.clone();
        let server = fake_service(move |mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::GetStatus));
            stream.write_u32(100).await.unwrap();
            stream.write_all(b"{\"").await.unwrap();
            stream.flush().await.unwrap();
            partial_written.store(true, Ordering::Release);
            std::future::pending::<()>().await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        client.timeout = Duration::from_secs(5);
        let mut in_flight = Box::pin(client.status());
        // Poll the status future until the fake peer has actually emitted a
        // partial frame, rather than cancelling before the request was sent.
        tokio::select! {
            result = &mut in_flight => panic!("partial status unexpectedly completed: {result:?}"),
            _ = async {
                while !partial_received.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            } => {},
        }
        // The outer timeout owns and drops the in-flight status future while
        // the client's separate five-second internal deadline is still pending.
        assert!(tokio::time::timeout(Duration::from_millis(20), in_flight)
            .await
            .is_err());
        assert_no_reuse(&mut client).await;
    });
}

#[test]
fn an_uncorrelated_reply_discards_the_stream() {
    run(async {
        let server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let mut response = reply(&request, Reply::Status(status(&descriptor)));
            response.request_id = Uuid::new_v4();
            write_frame(&mut stream, &response).await.unwrap();
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        assert!(matches!(client.status().await, Err(ClientError::Protocol)));
        assert_no_reuse(&mut client).await;
    });
}

#[test]
fn a_correlated_reply_of_the_wrong_kind_discards_the_stream() {
    run(async {
        let server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::GetStatus));
            // IDs and epoch are valid, but a meeting reply cannot satisfy status.
            write_frame(&mut stream, &reply(&request, Reply::Meeting(None)))
                .await
                .unwrap();
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        assert!(matches!(client.status().await, Err(ClientError::Protocol)));
        assert_no_reuse(&mut client).await;
    });
}

#[test]
fn a_reply_from_another_epoch_requires_resync_and_discards_the_stream() {
    run(async {
        let server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let mut response = reply(&request, Reply::Status(status(&descriptor)));
            response.library_epoch = Uuid::new_v4();
            write_frame(&mut stream, &response).await.unwrap();
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        assert!(matches!(
            client.status().await,
            Err(ClientError::Remote(RpcError {
                code: ErrorCode::ResyncRequired,
                ..
            }))
        ));
        assert_no_reuse(&mut client).await;
    });
}

#[test]
fn early_eof_is_reported_and_a_status_eof_cannot_reuse_the_stream() {
    run(async {
        let hello_eof = fake_service(|mut stream, _| async move {
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::Hello(_)));
            drop(stream);
        });
        assert!(matches!(
            ServiceClient::connect(hello_eof.root.path()).await,
            Err(ClientError::Io(_))
        ));
        let status_eof = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::GetStatus));
            drop(stream);
        });
        let mut client = ServiceClient::connect(status_eof.root.path())
            .await
            .unwrap();
        assert!(matches!(client.status().await, Err(ClientError::Io(_))));
        assert_no_reuse(&mut client).await;
    });
}

#[test]
fn client_independently_rejects_forged_hello_api_schema_build_and_identity() {
    run(async {
        for variant in [
            "api",
            "schema",
            "build",
            "library",
            "epoch",
            "pid",
            "capabilities",
            "context",
        ] {
            let server = fake_service(move |mut stream, descriptor| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(request.request, Request::Hello(_)));
                let mut forged = status(&descriptor);
                match variant {
                    "api" => forged.api.major += 1,
                    "schema" => forged.schema_version = STORAGE_SCHEMA_MAX + 1,
                    "build" => forged.build_id = "unrelated synthetic build".into(),
                    "library" => forged.library_id = Uuid::new_v4(),
                    "epoch" => forged.library_epoch = Uuid::new_v4(),
                    "pid" => forged.process_id = descriptor.process_id.wrapping_add(1),
                    "capabilities" => forged.capabilities.clear(),
                    "context" => forged.development_fixture_only = false,
                    _ => unreachable!(),
                }
                write_frame(&mut stream, &reply(&request, Reply::Hello(forged)))
                    .await
                    .unwrap();
            });
            let error = ServiceClient::connect(server.root.path())
                .await
                .err()
                .expect("forged hello must be rejected");
            if matches!(variant, "epoch" | "pid") {
                assert!(
                    matches!(error, ClientError::Protocol),
                    "{variant}: {error:?}"
                );
            } else {
                assert!(
                    matches!(
                        error,
                        ClientError::Remote(RpcError {
                            code: ErrorCode::UpgradeRequired,
                            ..
                        })
                    ),
                    "{variant}: {error:?}"
                );
            }
        }
    });
}

#[test]
fn malformed_discovery_is_rejected_before_connect_and_never_creates_a_database() {
    run(async {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(ENDPOINT_FILE);
        std::fs::write(&path, vec![b' '; 8193]).unwrap();
        assert!(matches!(
            ServiceClient::connect(root.path()).await,
            Err(ClientError::Discovery)
        ));

        let mut descriptor = EndpointDescriptor {
            discovery_version: DISCOVERY_VERSION,
            api: ApiVersion::CURRENT,
            process_id: std::process::id(),
            library_id: Uuid::new_v4(),
            library_epoch: Uuid::new_v4(),
            build_id: build_id().into(),
            endpoint: "synthetic invalid remote endpoint".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
        assert!(matches!(
            ServiceClient::connect(root.path()).await,
            Err(ClientError::Discovery)
        ));

        descriptor.endpoint =
            session_endpoint_name(root.path(), descriptor.library_epoch.as_bytes()).unwrap();
        #[cfg(windows)]
        {
            descriptor.library_epoch = Uuid::new_v4();
            std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
            assert!(matches!(
                ServiceClient::connect(root.path()).await,
                Err(ClientError::Discovery)
            ));
            assert!(matches!(
                publish_endpoint(root.path(), &descriptor),
                Err(ClientError::Discovery)
            ));
            descriptor.endpoint =
                session_endpoint_name(root.path(), descriptor.library_epoch.as_bytes()).unwrap();
        }
        descriptor.build_id = "different synthetic build".into();
        std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
        assert!(matches!(
            ServiceClient::connect(root.path()).await,
            Err(ClientError::Remote(RpcError {
                code: ErrorCode::UpgradeRequired,
                ..
            }))
        ));
        assert!(!root.path().join("bazza.db").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    });
}

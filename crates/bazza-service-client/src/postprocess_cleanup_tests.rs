#![cfg(any(windows, target_os = "macos"))]
use super::*;
use crate::*;
use bazza_service_api::{
    postprocess::{PostProcessJobLease, PostProcessSourceFence, RenewPostProcessJobRequest},
    postprocess_cleanup::*,
};
use bazza_service_transport::Listener;
use std::{future::Future, time::SystemTime};
use tokio::task::JoinHandle;
use uuid::Uuid;

const BODY: &str = r#"[{"segment_id":7,"text":"Орион \"quoted\""}]"#;

fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), future)
                .await
                .expect("bounded cleanup fake peer");
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
fn envelope(descriptor: &EndpointDescriptor, data: PostProcessCleanupReply) -> Reply {
    Reply::PostProcessCleanup(Box::new(DesktopEnvelope {
        library_id: descriptor.library_id,
        library_epoch: descriptor.library_epoch,
        data,
    }))
}
fn fake<F, Fut>(serve: F) -> Fake
where
    F: FnOnce(PeerStream, EndpointDescriptor) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let epoch = Uuid::new_v4();
    let descriptor = EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: ApiVersion::CURRENT,
        process_id: std::process::id(),
        library_id: Uuid::new_v4(),
        library_epoch: epoch,
        build_id: build_id().into(),
        endpoint: session_endpoint_name(root.path(), epoch.as_bytes()).unwrap(),
    };
    let mut listener = Listener::bind(&descriptor.endpoint).unwrap();
    publish_endpoint(root.path(), &descriptor).unwrap();
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
                    FIXTURE_WORKER_CAPABILITY.into(),
                    FIXTURE_POST_PROCESS_CLEANUP_CAPABILITY.into(),
                ],
            }),
        )
        .await;
        serve(stream, descriptor).await;
    });
    Fake {
        root,
        task: Some(task),
    }
}
fn request(client: &ServiceClient) -> (RegisteredWorker, PostProcessCleanupRequest) {
    let now = SystemTime::now();
    let session = Uuid::new_v4();
    let epoch = client.descriptor.library_epoch;
    let worker = RegisteredWorker {
        session_id: session,
        library_epoch: epoch,
        peer_pid: std::process::id(),
        capabilities: vec![WorkerCapability::PostProcess],
        expires_at: (now + Duration::from_secs(240)).into(),
    };
    let lease = PostProcessJobLease {
        operation_id: Uuid::new_v4(),
        claim_id: Uuid::new_v4(),
        worker_session_id: session,
        library_epoch: epoch,
        job_id: "cleanup-job".into(),
        meeting_id: Uuid::new_v4(),
        job_revision: "job-original".into(),
        input: PostProcessSourceFence {
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            checkpoint_revision: "checkpoint-original".into(),
            lifecycle_revision: "life-original".into(),
            transcript_revision: "transcript-original".into(),
            source_id: Some("captured-source".into()),
        },
        expires_at: (now + Duration::from_secs(60)).into(),
        change_sequence: 10,
        dedup_until: (now + Duration::from_secs(7 * 24 * 60 * 60)).into(),
    };
    let request = PostProcessCleanupRequest {
        proof: WorkerProof {
            session_id: session,
            secret: WorkerSecret::random(),
            library_epoch: epoch,
        },
        lease,
        fence: WorkerSecret::random(),
        operation_id: Uuid::new_v4(),
        operation: PostProcessCleanupOperation::Begin {
            upload_id: Uuid::new_v4(),
            total_bytes: BODY.len() as u64,
            sha256: "a".repeat(64),
        },
    };
    assert!(request.valid());
    (worker, request)
}
fn status(
    request: &PostProcessCleanupRequest,
    total: u64,
    received: u64,
    next: u64,
    state: PostProcessCleanupUploadState,
) -> PostProcessCleanupStatus {
    PostProcessCleanupStatus {
        upload_id: request.operation.upload_id(),
        claim_id: request.lease.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        checkpoint_revision: request.lease.input.checkpoint_revision.clone(),
        state,
        total_bytes: total,
        received_bytes: received,
        next_sequence: next,
        expires_at: (SystemTime::now() + Duration::from_secs(300)).into(),
    }
}
fn receipt(
    request: &PostProcessCleanupRequest,
    outcome: PostProcessCleanupOutcome,
) -> PostProcessCleanupReceipt {
    let mut lease = request.lease.clone();
    lease.operation_id = request.operation_id;
    lease.job_revision = "job-after-cleanup".into();
    lease.input.checkpoint_revision = "checkpoint-after-cleanup".into();
    lease.change_sequence += 1;
    if outcome == PostProcessCleanupOutcome::Applied {
        lease.input.transcript_revision = "transcript-after-cleanup".into();
    }
    PostProcessCleanupReceipt {
        operation_id: request.operation_id,
        upload_id: request.operation.upload_id(),
        lease,
        changed_segments: if outcome == PostProcessCleanupOutcome::Applied {
            1
        } else {
            0
        },
        outcome,
    }
}
fn next(
    request: &PostProcessCleanupRequest,
    operation: PostProcessCleanupOperation,
) -> PostProcessCleanupRequest {
    PostProcessCleanupRequest {
        operation_id: Uuid::new_v4(),
        operation,
        ..request.clone()
    }
}
fn invalid(error: ClientError) {
    assert!(matches!(error,ClientError::Remote(error) if error.code==ErrorCode::InvalidArgument));
}

#[test]
fn all_five_operations_preserve_chunks_and_explicit_renewed_lease_continuation() {
    run(async {
        let split = BODY.find('О').unwrap();
        let mut peer = fake(move |mut stream, d| async move {
            let mut upload = None;
            for index in 0..8 {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                if index == 2 {
                    let Request::RenewPostProcessJob(request) = &frame.request else {
                        panic!("explicit caller renewal expected");
                    };
                    let mut lease = request.lease.clone();
                    lease.operation_id = request.operation_id;
                    lease.job_revision = "renewed-job".into();
                    lease.change_sequence += 1;
                    lease.expires_at =
                        (SystemTime::from(lease.expires_at) + Duration::from_secs(30)).into();
                    send(
                        &mut stream,
                        &frame,
                        Reply::PostProcessRenewed(Box::new(DesktopEnvelope {
                            library_id: d.library_id,
                            library_epoch: d.library_epoch,
                            data: lease,
                        })),
                    )
                    .await;
                    continue;
                }
                let Request::PostProcessCleanup(request) = &frame.request else {
                    panic!("one explicit staged cleanup call");
                };
                assert!(request.valid());
                let value = match (&request.operation, index) {
                    (
                        PostProcessCleanupOperation::Begin {
                            upload_id,
                            total_bytes,
                            sha256,
                        },
                        0,
                    ) => {
                        upload = Some(*upload_id);
                        assert_eq!(*total_bytes, BODY.len() as u64);
                        assert_eq!(sha256, &"a".repeat(64));
                        PostProcessCleanupReply::Upload(status(
                            request,
                            *total_bytes,
                            0,
                            1,
                            PostProcessCleanupUploadState::Open,
                        ))
                    }
                    (
                        PostProcessCleanupOperation::Append {
                            upload_id,
                            sequence,
                            byte_offset,
                            text,
                            sha256,
                        },
                        1 | 3,
                    ) => {
                        assert_eq!(Some(*upload_id), upload);
                        assert_eq!(sha256, &"b".repeat(64));
                        let (expected_sequence, offset, fragment) = if index == 1 {
                            (1, 0, &BODY[..split])
                        } else {
                            (2, split as u64, &BODY[split..])
                        };
                        assert_eq!(*sequence, expected_sequence);
                        assert_eq!(*byte_offset, offset);
                        assert_eq!(text, fragment);
                        assert_eq!(
                            request.lease.job_revision,
                            if index == 1 {
                                "job-original"
                            } else {
                                "renewed-job"
                            }
                        );
                        PostProcessCleanupReply::Upload(status(
                            request,
                            BODY.len() as u64,
                            offset + text.len() as u64,
                            sequence + 1,
                            PostProcessCleanupUploadState::Open,
                        ))
                    }
                    (PostProcessCleanupOperation::Status { upload_id }, 4) => {
                        assert_eq!(Some(*upload_id), upload);
                        PostProcessCleanupReply::Upload(status(
                            request,
                            BODY.len() as u64,
                            BODY.len() as u64,
                            3,
                            PostProcessCleanupUploadState::Open,
                        ))
                    }
                    (PostProcessCleanupOperation::Commit { upload_id }, 5) => {
                        assert_eq!(Some(*upload_id), upload);
                        assert_eq!(request.lease.job_revision, "renewed-job");
                        PostProcessCleanupReply::Committed(receipt(
                            request,
                            PostProcessCleanupOutcome::Applied,
                        ))
                    }
                    (PostProcessCleanupOperation::Begin { total_bytes, .. }, 6) => {
                        assert_eq!(
                            request.lease.input.checkpoint_revision,
                            "checkpoint-after-cleanup"
                        );
                        PostProcessCleanupReply::Upload(status(
                            request,
                            *total_bytes,
                            0,
                            1,
                            PostProcessCleanupUploadState::Open,
                        ))
                    }
                    (PostProcessCleanupOperation::Abort { .. }, 7) => {
                        PostProcessCleanupReply::Upload(status(
                            request,
                            2,
                            0,
                            1,
                            PostProcessCleanupUploadState::Aborted,
                        ))
                    }
                    _ => panic!("unexpected staged operation order"),
                };
                assert!(value.valid_for(request));
                send(&mut stream, &frame, envelope(&d, value)).await;
            }
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, begin) = request(&client);
        let upload = begin.operation.upload_id();
        client
            .post_process_cleanup(&worker, begin.clone())
            .await
            .unwrap();
        let append = next(
            &begin,
            PostProcessCleanupOperation::Append {
                upload_id: upload,
                sequence: 1,
                byte_offset: 0,
                text: BODY[..split].into(),
                sha256: "b".repeat(64),
            },
        );
        client.post_process_cleanup(&worker, append).await.unwrap();
        let renewed = client
            .renew_post_process_job(
                &worker,
                RenewPostProcessJobRequest {
                    proof: begin.proof.clone(),
                    lease: begin.lease.clone(),
                    operation_id: Uuid::new_v4(),
                    fence: begin.fence.clone(),
                    ttl_seconds: 120,
                },
            )
            .await
            .unwrap()
            .data;
        let base = PostProcessCleanupRequest {
            lease: renewed,
            ..begin.clone()
        };
        let append = next(
            &base,
            PostProcessCleanupOperation::Append {
                upload_id: upload,
                sequence: 2,
                byte_offset: split as u64,
                text: BODY[split..].into(),
                sha256: "b".repeat(64),
            },
        );
        client.post_process_cleanup(&worker, append).await.unwrap();
        client
            .post_process_cleanup(
                &worker,
                next(
                    &base,
                    PostProcessCleanupOperation::Status { upload_id: upload },
                ),
            )
            .await
            .unwrap();
        let committed = client
            .post_process_cleanup(
                &worker,
                next(
                    &base,
                    PostProcessCleanupOperation::Commit { upload_id: upload },
                ),
            )
            .await
            .unwrap();
        assert_eq!(committed.library_id, client.descriptor.library_id);
        assert_eq!(committed.library_epoch, client.descriptor.library_epoch);
        let PostProcessCleanupReply::Committed(receipt) = committed.data else {
            panic!("committed result");
        };
        assert_eq!(receipt.outcome, PostProcessCleanupOutcome::Applied);
        assert_eq!(receipt.changed_segments, 1);
        let base = PostProcessCleanupRequest {
            lease: receipt.lease,
            ..base
        };
        let other = Uuid::new_v4();
        client
            .post_process_cleanup(
                &worker,
                next(
                    &base,
                    PostProcessCleanupOperation::Begin {
                        upload_id: other,
                        total_bytes: 2,
                        sha256: "c".repeat(64),
                    },
                ),
            )
            .await
            .unwrap();
        client
            .post_process_cleanup(
                &worker,
                next(
                    &base,
                    PostProcessCleanupOperation::Abort { upload_id: other },
                ),
            )
            .await
            .unwrap();
        peer.finish().await;
    });
}

#[test]
fn invalid_caller_shapes_and_escaped_frame_budget_send_no_wire_and_keep_stream_usable() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessCleanup(request) = &frame.request else {
                panic!("only final valid request may reach wire");
            };
            assert!(matches!(
                request.operation,
                PostProcessCleanupOperation::Begin { .. }
            ));
            let value = PostProcessCleanupReply::Upload(status(
                request,
                BODY.len() as u64,
                0,
                1,
                PostProcessCleanupUploadState::Open,
            ));
            send(&mut stream, &frame, envelope(&d, value)).await;
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, request) = request(&client);
        for fault in 0..5 {
            let mut bad = worker.clone();
            match fault {
                0 => bad.peer_pid = std::process::id().saturating_add(1),
                1 => bad.capabilities = vec![WorkerCapability::Capture],
                2 => bad.expires_at = std::time::UNIX_EPOCH.into(),
                3 => bad.library_epoch = Uuid::new_v4(),
                _ => bad.session_id = Uuid::new_v4(),
            }
            invalid(
                client
                    .post_process_cleanup(&bad, request.clone())
                    .await
                    .unwrap_err(),
            );
        }
        for fault in 0..11 {
            let mut bad = request.clone();
            match fault {
                0 => bad.operation_id = Uuid::nil(),
                1 => bad.proof.session_id = Uuid::new_v4(),
                2 => bad.lease.input.checkpoint_revision = "bad/checkpoint".into(),
                3 => {
                    bad.operation = PostProcessCleanupOperation::Begin {
                        upload_id: Uuid::nil(),
                        total_bytes: 2,
                        sha256: "a".repeat(64),
                    }
                }
                4 => {
                    bad.operation = PostProcessCleanupOperation::Begin {
                        upload_id: Uuid::new_v4(),
                        total_bytes: MAX_POST_PROCESS_CLEANUP_BODY_BYTES + 1,
                        sha256: "a".repeat(64),
                    }
                }
                5 => {
                    bad.operation = PostProcessCleanupOperation::Append {
                        upload_id: Uuid::new_v4(),
                        sequence: 0,
                        byte_offset: 0,
                        text: "x".into(),
                        sha256: "a".repeat(64),
                    }
                }
                6 => {
                    bad.operation = PostProcessCleanupOperation::Append {
                        upload_id: Uuid::new_v4(),
                        sequence: 1,
                        byte_offset: u64::MAX,
                        text: "x".into(),
                        sha256: "a".repeat(64),
                    }
                }
                7 => {
                    bad.operation = PostProcessCleanupOperation::Append {
                        upload_id: Uuid::new_v4(),
                        sequence: 1,
                        byte_offset: 0,
                        text: "\0".repeat(6000),
                        sha256: "a".repeat(64),
                    }
                }
                8 => {
                    bad.operation = PostProcessCleanupOperation::Begin {
                        upload_id: Uuid::new_v4(),
                        total_bytes: 2,
                        sha256: "A".repeat(64),
                    }
                }
                9 => {
                    bad.operation = PostProcessCleanupOperation::Append {
                        upload_id: Uuid::new_v4(),
                        sequence: 1,
                        byte_offset: 0,
                        text: String::new(),
                        sha256: "a".repeat(64),
                    }
                }
                _ => {
                    bad.operation = PostProcessCleanupOperation::Append {
                        upload_id: Uuid::new_v4(),
                        sequence: MAX_POST_PROCESS_CLEANUP_CHUNKS + 1,
                        byte_offset: 0,
                        text: "x".into(),
                        sha256: "a".repeat(64),
                    }
                }
            }
            assert!(!bad.valid());
            invalid(client.post_process_cleanup(&worker, bad).await.unwrap_err());
        }
        client.post_process_cleanup(&worker, request).await.unwrap();
        peer.finish().await;
    });
}

#[test]
fn historical_expired_commit_replay_preserves_original_operation_without_regranting_freshness() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            let mut original = None;
            let mut saved = None;
            for _ in 0..2 {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessCleanup(request) = &frame.request else {
                    panic!("immutable historical commit");
                };
                let value = serde_json::to_value(request).unwrap();
                if let Some(previous) = &original {
                    assert_eq!(&value, previous);
                } else {
                    original = Some(value);
                }
                assert!(SystemTime::from(request.lease.expires_at) < SystemTime::now());
                let reply = saved
                    .get_or_insert_with(|| {
                        PostProcessCleanupReply::Committed(receipt(
                            request,
                            PostProcessCleanupOutcome::Noop,
                        ))
                    })
                    .clone();
                send(&mut stream, &frame, envelope(&d, reply)).await;
            }
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, mut request) = request(&client);
        request.lease.expires_at = (SystemTime::now() - Duration::from_secs(30)).into();
        request.operation = PostProcessCleanupOperation::Commit {
            upload_id: request.operation.upload_id(),
        };
        assert!(request.valid());
        let first = client
            .post_process_cleanup(&worker, request.clone())
            .await
            .unwrap();
        let second = client.post_process_cleanup(&worker, request).await.unwrap();
        assert_eq!(first.data, second.data);
        let PostProcessCleanupReply::Committed(receipt) = first.data else {
            panic!("historical receipt");
        };
        assert!(SystemTime::from(receipt.lease.expires_at) < SystemTime::now());
        assert_eq!(receipt.outcome, PostProcessCleanupOutcome::Noop);
        assert_eq!(receipt.changed_segments, 0);
        peer.finish().await;
    });
}

#[test]
fn forged_upload_identity_progress_expiry_and_closed_body_replies_poison_connection() {
    run(async {
        for fault in 0..15 {
            let mut peer = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessCleanup(request) = &frame.request else {
                    panic!("staged append");
                };
                let mut value = status(request, 100, 20, 2, PostProcessCleanupUploadState::Open);
                match fault {
                    0 => value.checkpoint_revision = "foreign-checkpoint".into(),
                    1 => value.claim_id = Uuid::new_v4(),
                    2 => value.upload_id = Uuid::new_v4(),
                    3 => value.meeting_id = Uuid::new_v4(),
                    4 => value.total_bytes = MAX_POST_PROCESS_CLEANUP_BODY_BYTES + 1,
                    5 => value.received_bytes = 101,
                    6 => value.next_sequence = 0,
                    7 => value.next_sequence = 1,
                    8 => value.received_bytes = 0,
                    9 => value.expires_at = (SystemTime::now() + Duration::from_secs(7200)).into(),
                    13 => value.job_id = "\0".repeat(11000),
                    _ => {}
                }
                let mut reply = envelope(&d, PostProcessCleanupReply::Upload(value));
                if fault == 10 {
                    let Reply::PostProcessCleanup(value) = &mut reply else {
                        unreachable!()
                    };
                    value.library_id = Uuid::new_v4();
                }
                if fault == 11 {
                    let Reply::PostProcessCleanup(value) = &mut reply else {
                        unreachable!()
                    };
                    value.library_epoch = Uuid::new_v4();
                }
                if fault == 14 {
                    reply = Reply::ShutdownAccepted;
                }
                if fault == 12 {
                    let mut raw = serde_json::to_value(ReplyEnvelope {
                        request_id: frame.request_id,
                        library_epoch: frame.library_epoch,
                        reply,
                    })
                    .unwrap();
                    raw["reply"]["data"]["data"]["data"]["body"] =
                        serde_json::json!("private body must never be in status");
                    write_frame(&mut stream, &raw).await.unwrap();
                } else {
                    send(&mut stream, &frame, reply).await;
                }
                assert!(
                    read_frame::<RequestEnvelope>(&mut stream).await.is_err(),
                    "invalid reply must close the stream"
                );
            });
            let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
            let (worker, base) = request(&client);
            let request = next(
                &base,
                PostProcessCleanupOperation::Append {
                    upload_id: base.operation.upload_id(),
                    sequence: 1,
                    byte_offset: 0,
                    text: "piece".into(),
                    sha256: "a".repeat(64),
                },
            );
            assert!(
                client.post_process_cleanup(&worker, request).await.is_err(),
                "fault {fault}"
            );
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            peer.finish().await;
        }
    });
}

#[test]
fn forged_commit_claim_checkpoint_cas_noop_and_retention_are_rejected_and_poisoned() {
    run(async {
        for fault in 0..11 {
            let mut peer = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessCleanup(request) = &frame.request else {
                    panic!("staged commit");
                };
                let mut value = receipt(request, PostProcessCleanupOutcome::Noop);
                match fault {
                    0 => value.operation_id = Uuid::new_v4(),
                    1 => value.lease.claim_id = Uuid::new_v4(),
                    2 => {
                        value.lease.input.checkpoint_revision =
                            request.lease.input.checkpoint_revision.clone()
                    }
                    3 => value.lease.job_revision = request.lease.job_revision.clone(),
                    4 => value.changed_segments = 1,
                    5 => value.lease.input.transcript_revision = "changed-on-noop".into(),
                    6 => {
                        value.lease.expires_at = (SystemTime::from(request.lease.expires_at)
                            + Duration::from_secs(1))
                        .into()
                    }
                    7 => {
                        value.lease.dedup_until =
                            (SystemTime::now() + Duration::from_secs(30 * 24 * 60 * 60)).into()
                    }
                    8 => value.lease.change_sequence = request.lease.change_sequence,
                    9 => {
                        value.outcome = PostProcessCleanupOutcome::Applied;
                        value.changed_segments = 0;
                    }
                    _ => {
                        value.outcome = PostProcessCleanupOutcome::Applied;
                        value.changed_segments = 1;
                    }
                }
                send(
                    &mut stream,
                    &frame,
                    envelope(&d, PostProcessCleanupReply::Committed(value)),
                )
                .await;
                assert!(read_frame::<RequestEnvelope>(&mut stream).await.is_err());
            });
            let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
            let (worker, base) = request(&client);
            let request = next(
                &base,
                PostProcessCleanupOperation::Commit {
                    upload_id: base.operation.upload_id(),
                },
            );
            assert!(
                matches!(
                    client.post_process_cleanup(&worker, request).await,
                    Err(ClientError::Protocol)
                ),
                "fault {fault}"
            );
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            peer.finish().await;
        }
    });
}

#[test]
fn conflict_is_propagated_without_implicit_refresh_or_retry_and_preserves_original_arguments() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            let first: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            send(
                &mut stream,
                &first,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            let second: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                serde_json::to_value(&first.request).unwrap(),
                serde_json::to_value(&second.request).unwrap()
            );
            let Request::PostProcessCleanup(request) = &second.request else {
                panic!("no implicit bootstrap/status/renew");
            };
            send(
                &mut stream,
                &second,
                envelope(
                    &d,
                    PostProcessCleanupReply::Committed(receipt(
                        request,
                        PostProcessCleanupOutcome::Noop,
                    )),
                ),
            )
            .await;
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, base) = request(&client);
        let request = next(
            &base,
            PostProcessCleanupOperation::Commit {
                upload_id: base.operation.upload_id(),
            },
        );
        assert!(
            matches!(client.post_process_cleanup(&worker,request.clone()).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        client.post_process_cleanup(&worker, request).await.unwrap();
        peer.finish().await;
    });
}

#[test]
fn wrong_cleanup_subreply_or_open_abort_status_poison_the_connection() {
    run(async {
        for fault in 0..2 {
            let mut peer = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessCleanup(request) = &frame.request else {
                    panic!("typed cleanup operation");
                };
                let value = if fault == 0 {
                    PostProcessCleanupReply::Committed(receipt(
                        request,
                        PostProcessCleanupOutcome::Noop,
                    ))
                } else {
                    PostProcessCleanupReply::Upload(status(
                        request,
                        100,
                        20,
                        2,
                        PostProcessCleanupUploadState::Open,
                    ))
                };
                send(&mut stream, &frame, envelope(&d, value)).await;
                assert!(read_frame::<RequestEnvelope>(&mut stream).await.is_err());
            });
            let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
            let (worker, mut request) = request(&client);
            if fault == 1 {
                request.operation = PostProcessCleanupOperation::Abort {
                    upload_id: request.operation.upload_id(),
                };
            }
            assert!(matches!(
                client.post_process_cleanup(&worker, request).await,
                Err(ClientError::Protocol)
            ));
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            peer.finish().await;
        }
    });
}

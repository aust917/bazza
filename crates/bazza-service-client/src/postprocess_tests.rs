#![cfg(any(windows, target_os = "macos"))]
use crate::*;
use bazza_service_api::{desktop::*, postprocess::*, worker::CaptureProvider};
use bazza_service_transport::Listener;

use std::future::Future;
use tokio::task::JoinHandle;
use uuid::Uuid;

fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(4), future)
                .await
                .expect("bounded private post-process fake-peer test");
        });
}
struct Fake {
    root: tempfile::TempDir,
    task: JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn descriptor(root: &Path) -> EndpointDescriptor {
    let epoch = Uuid::new_v4();
    EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: ApiVersion::CURRENT,
        process_id: std::process::id(),
        library_id: Uuid::new_v4(),
        library_epoch: epoch,
        build_id: build_id().into(),
        endpoint: session_endpoint_name(root, epoch.as_bytes()).unwrap(),
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
async fn hello(stream: &mut PeerStream, d: &EndpointDescriptor) {
    let request: RequestEnvelope = read_frame(stream).await.unwrap();
    assert!(matches!(request.request, Request::Hello(_)));
    send(
        stream,
        &request,
        Reply::Hello(ServiceStatus {
            api: ApiVersion::CURRENT,
            schema_version: STORAGE_SCHEMA_MAX,
            build_id: build_id().into(),
            library_id: d.library_id,
            library_epoch: d.library_epoch,
            process_id: d.process_id,
            authentication: "local_os_user".into(),
            development_fixture_only: true,
            capabilities: vec![
                FIXTURE_STATUS_CAPABILITY.into(),
                FIXTURE_METADATA_CAPABILITY.into(),
                FIXTURE_DESKTOP_CAPABILITY.into(),
                FIXTURE_WORKER_CAPABILITY.into(),
                FIXTURE_POST_PROCESS_CAPABILITY.into(),
            ],
        }),
    )
    .await;
}
fn fake<F, Fut>(serve: F) -> Fake
where
    F: FnOnce(PeerStream, EndpointDescriptor) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let d = descriptor(root.path());
    let mut listener = Listener::bind(&d.endpoint).unwrap();
    publish_endpoint(root.path(), &d).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.process_id, std::process::id());
        hello(&mut stream, &d).await;
        serve(stream, d).await;
    });
    Fake { root, task }
}
fn envelope<T>(d: &EndpointDescriptor, data: T) -> DesktopEnvelope<T> {
    DesktopEnvelope {
        library_id: d.library_id,
        library_epoch: d.library_epoch,
        data,
    }
}

fn registration(client: &ServiceClient) -> (RegisteredWorker, WorkerProof) {
    let worker = RegisteredWorker {
        session_id: Uuid::new_v4(),
        library_epoch: client.descriptor.library_epoch,
        peer_pid: std::process::id(),
        capabilities: vec![WorkerCapability::PostProcess],
        expires_at: (std::time::SystemTime::now() + Duration::from_secs(240)).into(),
    };
    let proof = WorkerProof {
        session_id: worker.session_id,
        secret: WorkerSecret::random(),
        library_epoch: worker.library_epoch,
    };
    (worker, proof)
}
fn job() -> PostProcessJob {
    PostProcessJob {
        job_id: "job-1".into(),
        meeting_id: Uuid::new_v4(),
        revision: "original-job".into(),
        stage: PostProcessStage::Captured,
        input: PostProcessSourceFence {
            checkpoint_revision: "checkpoint-initial".into(),
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            lifecycle_revision: "finalized-life".into(),
            transcript_revision: "captured-raw".into(),
            source_id: Some("captured-source".into()),
        },
        provider: CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("selected-model".into()),
            language: Some("ru".into()),
        },
    }
}
fn claim(proof: WorkerProof) -> ClaimPostProcessJobRequest {
    ClaimPostProcessJobRequest {
        proof,
        job: job(),
        claim_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        ttl_seconds: 30,
    }
}
fn lease(request: &ClaimPostProcessJobRequest) -> PostProcessJobLease {
    PostProcessJobLease {
        operation_id: request.operation_id,
        claim_id: request.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.job.job_id.clone(),
        meeting_id: request.job.meeting_id,
        job_revision: "claimed-job".into(),
        input: request.job.input.clone(),
        expires_at: (std::time::SystemTime::now() + Duration::from_secs(30)).into(),
        change_sequence: 9,
        dedup_until: (std::time::SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60)).into(),
    }
}
fn failed(request: &FailPostProcessJobRequest) -> PostProcessFailureReceipt {
    PostProcessFailureReceipt {
        operation_id: request.operation_id,
        claim_id: request.lease.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        job_revision: "failed-job".into(),
        lifecycle_revision: "failed-life".into(),
        error_code: request.error_code,
        change_sequence: request.lease.change_sequence + 2,
        dedup_until: (std::time::SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60)).into(),
    }
}

#[test]
fn all_post_process_calls_keep_original_descriptors_and_return_identity_snapshots() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessJobs(request) = &frame.request else {
                panic!("expected bounded list")
            };
            let page = PostProcessJobPage {
                library_scope: "a".repeat(32),
                snapshot_sequence: 8,
                eligible_at: std::time::SystemTime::now().into(),
                items: vec![job()],
                next: None,
            };
            assert!(page.valid_for(request));
            send(
                &mut stream,
                &frame,
                Reply::PostProcessJobs(Box::new(envelope(&d, page))),
            )
            .await;
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::ClaimPostProcessJob(request) = &frame.request else {
                panic!("expected claim")
            };
            assert_eq!(request.job.revision, "original-job");
            let claimed = lease(request);
            send(
                &mut stream,
                &frame,
                Reply::PostProcessClaimed(Box::new(envelope(&d, claimed))),
            )
            .await;
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::RenewPostProcessJob(request) = &frame.request else {
                panic!("expected renew")
            };
            let mut renewed = request.lease.clone();
            renewed.operation_id = request.operation_id;
            renewed.job_revision = "renewed-job".into();
            renewed.change_sequence += 1;
            send(
                &mut stream,
                &frame,
                Reply::PostProcessRenewed(Box::new(envelope(&d, renewed))),
            )
            .await;
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::FailPostProcessJob(request) = &frame.request else {
                panic!("expected fail")
            };
            let receipt = failed(request);
            send(
                &mut stream,
                &frame,
                Reply::PostProcessFailed(Box::new(envelope(&d, receipt))),
            )
            .await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, proof) = registration(&client);
        let page = client
            .post_process_jobs(&worker, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap();
        let mut request = claim(proof.clone());
        request.job = page.data.items[0].clone();
        let fence = request.fence.clone();
        let claimed = client
            .claim_post_process_job(&worker, request)
            .await
            .unwrap();
        assert_eq!(claimed.library_id, page.library_id);
        assert_eq!(claimed.library_epoch, page.library_epoch);
        let renewed = client
            .renew_post_process_job(
                &worker,
                RenewPostProcessJobRequest {
                    proof: proof.clone(),
                    lease: claimed.data,
                    operation_id: Uuid::new_v4(),
                    fence: fence.clone(),
                    ttl_seconds: 30,
                },
            )
            .await
            .unwrap();
        let receipt = client
            .fail_post_process_job(
                &worker,
                FailPostProcessJobRequest {
                    proof,
                    lease: renewed.data,
                    operation_id: Uuid::new_v4(),
                    fence,
                    error_code: PostProcessFailureCode::PostProcessFailed,
                },
            )
            .await
            .unwrap();
        assert_eq!(receipt.data.lifecycle_revision, "failed-life");
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn forged_post_process_claim_identity_nonce_source_and_ttl_poison_the_connection() {
    run(async {
        for fault in 0..12 {
            let mut server = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::ClaimPostProcessJob(request) = &frame.request else {
                    panic!("claim")
                };
                let mut value = envelope(&d, lease(request));
                match fault {
                    0 => value.library_id = Uuid::new_v4(),
                    1 => value.library_epoch = Uuid::new_v4(),
                    2 => value.data.operation_id = Uuid::new_v4(),
                    3 => value.data.claim_id = Uuid::new_v4(),
                    4 => value.data.worker_session_id = Uuid::new_v4(),
                    5 => value.data.job_id = "another-job".into(),
                    6 => value.data.meeting_id = Uuid::new_v4(),
                    7 => value.data.input.transcript_revision = "newer-input".into(),
                    8 => value.data.job_revision = request.job.revision.clone(),
                    9 => {
                        value.data.expires_at =
                            (std::time::SystemTime::now() + Duration::from_secs(301)).into()
                    }
                    10 => value.data.change_sequence = 0,
                    _ => {}
                }
                let reply = if fault == 11 {
                    Reply::PostProcessRenewed(Box::new(value))
                } else {
                    Reply::PostProcessClaimed(Box::new(value))
                };
                send(&mut stream, &frame, reply).await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let (worker, proof) = registration(&client);
            assert!(
                matches!(
                    client.claim_post_process_job(&worker, claim(proof)).await,
                    Err(ClientError::Protocol)
                ),
                "fault{fault}"
            );
            assert!(client.stream.is_none());
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn worker_job_pages_reject_foreign_terminal_scope_bad_progress_and_budgets() {
    run(async {
        for fault in 0..7 {
            let mut server = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessJobs(request) = &frame.request else {
                    panic!("list")
                };
                let cursor = request.cursor.as_ref().unwrap();
                let mut page = PostProcessJobPage {
                    library_scope: cursor.library_scope.clone(),
                    snapshot_sequence: cursor.snapshot_sequence,
                    eligible_at: cursor.eligible_at,
                    items: vec![job()],
                    next: None,
                };
                match fault {
                    0 => page.library_scope = "c".repeat(32),
                    1 => page.snapshot_sequence += 1,
                    2 => page.eligible_at = std::time::SystemTime::now().into(),
                    3 => page.items.push(page.items[0].clone()),
                    4 => page.items[0].job_id = cursor.after_job_id.clone(),
                    5 => {
                        page.next = Some(PostProcessJobCursor {
                            after_job_id: "skipped-job".into(),
                            ..cursor.clone()
                        })
                    }
                    _ => {
                        page.items = (0..10)
                            .map(|n| {
                                let mut item = job();
                                item.job_id = format!("job-{n:03}");
                                item
                            })
                            .collect()
                    }
                }
                send(
                    &mut stream,
                    &frame,
                    Reply::PostProcessJobs(Box::new(envelope(&d, page))),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let (worker, proof) = registration(&client);
            let mut request = PostProcessJobsRequest::new(proof.clone());
            request.cursor = Some(PostProcessJobCursor {
                library_scope: "a".repeat(32),
                library_epoch: proof.library_epoch,
                worker_session_id: proof.session_id,
                snapshot_sequence: 8,
                eligible_at: (std::time::SystemTime::now() - Duration::from_secs(1)).into(),
                after_job_id: "job-0".into(),
                query_hash: "b".repeat(64),
            });
            if fault == 6 {
                request.max_bytes = 1024;
            }
            assert!(
                matches!(
                    client.post_process_jobs(&worker, request).await,
                    Err(ClientError::Protocol)
                ),
                "fault{fault}"
            );
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn renew_and_failure_replies_bind_the_original_operation_and_fences() {
    run(async {
        for fault in 0..4 {
            let mut server = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let reply = match &frame.request {
                    Request::RenewPostProcessJob(request) => {
                        let mut value = request.lease.clone();
                        value.operation_id = request.operation_id;
                        value.job_revision = "renewed-job".into();
                        value.change_sequence += 1;
                        if fault == 0 {
                            value.input.source_id = Some("foreign-source".into());
                        } else {
                            value.operation_id = Uuid::new_v4();
                        }
                        Reply::PostProcessRenewed(Box::new(envelope(&d, value)))
                    }
                    Request::FailPostProcessJob(request) => {
                        let mut value = failed(request);
                        if fault == 2 {
                            value.error_code = PostProcessFailureCode::PostProcessInputUnavailable;
                        } else {
                            value.lifecycle_revision =
                                request.lease.input.lifecycle_revision.clone();
                        }
                        Reply::PostProcessFailed(Box::new(envelope(&d, value)))
                    }
                    _ => panic!("renew/fail"),
                };
                send(&mut stream, &frame, reply).await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let (worker, proof) = registration(&client);
            let claim = claim(proof.clone());
            let lease = lease(&claim);
            let result = if fault < 2 {
                client
                    .renew_post_process_job(
                        &worker,
                        RenewPostProcessJobRequest {
                            proof,
                            lease,
                            operation_id: Uuid::new_v4(),
                            fence: claim.fence,
                            ttl_seconds: 30,
                        },
                    )
                    .await
                    .map(|_| ())
            } else {
                client
                    .fail_post_process_job(
                        &worker,
                        FailPostProcessJobRequest {
                            proof,
                            lease,
                            operation_id: Uuid::new_v4(),
                            fence: claim.fence,
                            error_code: PostProcessFailureCode::PostProcessFailed,
                        },
                    )
                    .await
                    .map(|_| ())
            };
            assert!(matches!(result, Err(ClientError::Protocol)));
            assert!(client.stream.is_none());
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn invalid_worker_or_old_epoch_cursor_is_rejected_without_any_wire_call() {
    run(async {
        let mut server = fake(|mut stream, _| async move {
            assert!(tokio::time::timeout(
                Duration::from_millis(120),
                read_frame::<RequestEnvelope>(&mut stream)
            )
            .await
            .is_err());
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, proof) = registration(&client);
        let mut request = PostProcessJobsRequest::new(proof.clone());
        request.cursor = Some(PostProcessJobCursor {
            library_scope: "a".repeat(32),
            library_epoch: Uuid::new_v4(),
            worker_session_id: proof.session_id,
            snapshot_sequence: 1,
            eligible_at: std::time::SystemTime::now().into(),
            after_job_id: "job-0".into(),
            query_hash: "b".repeat(64),
        });
        assert!(
            matches!(client.post_process_jobs(&worker,request).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        let mut wrong = worker.clone();
        wrong.capabilities = vec![WorkerCapability::Capture];
        assert!(
            matches!(client.claim_post_process_job(&wrong,claim(proof)).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        assert!(client.stream.is_some());
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn conflict_keeps_the_original_expected_revision_nonce_and_fence_without_implicit_retry() {
    run(async {
        let mut server = fake(|mut stream, _| async move {
            let first: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::ClaimPostProcessJob(request) = &first.request else {
                panic!("claim")
            };
            let original = serde_json::to_value(request).unwrap();
            assert_eq!(request.job.revision, "original-job");
            send(
                &mut stream,
                &first,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            let second: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::ClaimPostProcessJob(request) = &second.request else {
                panic!("no implicit list/refetch")
            };
            assert_eq!(serde_json::to_value(request).unwrap(), original);
            send(
                &mut stream,
                &second,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            assert!(tokio::time::timeout(
                Duration::from_millis(120),
                read_frame::<RequestEnvelope>(&mut stream)
            )
            .await
            .is_err());
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let (worker, proof) = registration(&client);
        let request = claim(proof);
        for _ in 0..2 {
            assert!(
                matches!(client.claim_post_process_job(&worker,request.clone()).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
            );
        }
        (&mut server.task).await.unwrap();
    });
}

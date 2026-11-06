#![cfg(any(windows, target_os = "macos"))]
use crate::*;
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::{PostProcessJobLease, PostProcessSourceFence},
    postprocess_result::*,
};
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
            tokio::time::timeout(Duration::from_secs(6), future)
                .await
                .expect("bounded auto-title fake peer");
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
fn status(d: &EndpointDescriptor) -> ServiceStatus {
    ServiceStatus {
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
            FIXTURE_WORKER_CAPABILITY.into(),
            FIXTURE_POST_PROCESS_RESULT_CAPABILITY.into(),
        ],
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
fn envelope(d: &EndpointDescriptor, data: PostProcessAutoTitleReceipt) -> Reply {
    Reply::PostProcessAutoTitle(Box::new(DesktopEnvelope {
        library_id: d.library_id,
        library_epoch: d.library_epoch,
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
    let d = EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: ApiVersion::CURRENT,
        process_id: std::process::id(),
        library_id: Uuid::new_v4(),
        library_epoch: epoch,
        build_id: build_id().into(),
        endpoint: session_endpoint_name(root.path(), epoch.as_bytes()).unwrap(),
    };
    let mut listener = Listener::bind(&d.endpoint).unwrap();
    publish_endpoint(root.path(), &d).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.process_id, std::process::id());
        let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
        assert!(matches!(request.request, Request::Hello(_)));
        send(&mut stream, &request, Reply::Hello(status(&d))).await;
        serve(stream, d).await;
    });
    Fake {
        root,
        task: Some(task),
    }
}
fn request(client: &ServiceClient) -> (RegisteredWorker, CommitPostProcessAutoTitleRequest) {
    let epoch = client.descriptor.library_epoch;
    let session = Uuid::new_v4();
    let worker = RegisteredWorker {
        session_id: session,
        library_epoch: epoch,
        peer_pid: std::process::id(),
        capabilities: vec![WorkerCapability::PostProcess],
        expires_at: (std::time::SystemTime::now() + Duration::from_secs(240)).into(),
    };
    let proof = WorkerProof {
        session_id: session,
        secret: WorkerSecret::random(),
        library_epoch: epoch,
    };
    let lease = PostProcessJobLease {
        operation_id: Uuid::new_v4(),
        claim_id: Uuid::new_v4(),
        worker_session_id: session,
        library_epoch: epoch,
        job_id: "job-auto-title".into(),
        meeting_id: Uuid::new_v4(),
        job_revision: "claimed-job-1".into(),
        input: PostProcessSourceFence {
            checkpoint_revision: "checkpoint-initial".into(),
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            lifecycle_revision: "finalized-life".into(),
            transcript_revision: "captured-text".into(),
            source_id: Some("captured-source".into()),
        },
        expires_at: (std::time::SystemTime::now() + Duration::from_secs(60)).into(),
        change_sequence: 9,
        dedup_until: (std::time::SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60)).into(),
    };
    (
        worker,
        CommitPostProcessAutoTitleRequest {
            proof,
            lease,
            fence: WorkerSecret::random(),
            operation_id: Uuid::new_v4(),
            expected_metadata_revision: "metadata-read-1".into(),
            title: "  Орион 日本語 🦉  ".into(),
        },
    )
}
fn receipt(request: &CommitPostProcessAutoTitleRequest) -> PostProcessAutoTitleReceipt {
    PostProcessAutoTitleReceipt {
        operation_id: request.operation_id,
        claim_id: request.lease.claim_id,
        worker_session_id: request.proof.session_id,
        library_epoch: request.proof.library_epoch,
        job_id: request.lease.job_id.clone(),
        meeting_id: request.lease.meeting_id,
        metadata_revision: "metadata-auto-2".into(),
        change_sequence: 10,
        dedup_until: (std::time::SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60)).into(),
    }
}

#[test]
fn historical_receipt_replays_keep_original_metadata_nonce_title_and_expired_lease() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            let mut saved = None;
            let mut committed: Option<PostProcessAutoTitleReceipt> = None;
            for _ in 0..2 {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::CommitPostProcessAutoTitle(request) = &frame.request else {
                    panic!("one explicit auto-title operation");
                };
                assert!(request.valid());
                assert_eq!(request.expected_metadata_revision, "metadata-read-1");
                assert_eq!(request.title, "  Орион 日本語 🦉  ");
                assert!(
                    std::time::SystemTime::from(request.lease.expires_at)
                        < std::time::SystemTime::now()
                );
                let value = serde_json::to_value(request).unwrap();
                if let Some(original) = &saved {
                    assert_eq!(&value, original);
                } else {
                    saved = Some(value);
                }
                let historical = committed.get_or_insert_with(|| receipt(request)).clone();
                send(&mut stream, &frame, envelope(&d, historical)).await;
            }
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, mut request) = request(&client);
        request.lease.expires_at = (std::time::SystemTime::now() - Duration::from_secs(30)).into();
        let debug = format!("{request:?}");
        assert!(!debug.contains(&request.title));
        assert!(!debug.contains(request.fence.expose()));
        assert!(!debug.contains(request.proof.secret.expose()));
        let first = client
            .commit_post_process_auto_title(&worker, request.clone())
            .await
            .unwrap();
        let second = client
            .commit_post_process_auto_title(&worker, request)
            .await
            .unwrap();
        assert_eq!(first.data, second.data);
        assert_eq!(first.library_id, client.descriptor.library_id);
        assert_eq!(first.library_epoch, client.descriptor.library_epoch);
        assert!(serde_json::to_value(first.data)
            .unwrap()
            .get("title")
            .is_none());
        peer.finish().await;
    });
}

#[test]
fn invalid_registration_and_request_are_rejected_locally_without_poisoning_stream() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::CommitPostProcessAutoTitle(request) = &frame.request else {
                panic!("valid operation after local rejections");
            };
            assert!(request.valid());
            send(&mut stream, &frame, envelope(&d, receipt(request))).await;
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, mut request) = request(&client);
        for mode in 0..5 {
            let mut invalid = worker.clone();
            match mode {
                0 => invalid.peer_pid = std::process::id().saturating_add(1),
                1 => invalid.capabilities = vec![WorkerCapability::Capture],
                2 => invalid.expires_at = std::time::UNIX_EPOCH.into(),
                3 => invalid.library_epoch = Uuid::new_v4(),
                _ => invalid.session_id = Uuid::new_v4(),
            }
            assert!(
                matches!(client.commit_post_process_auto_title(&invalid,request.clone()).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
            );
        }
        for mode in 0..6 {
            let mut invalid = request.clone();
            match mode {
                0 => invalid.title = " \t\n".into(),
                1 => invalid.title = "bad\0title".into(),
                2 => {
                    invalid.title =
                        format!("{}x", "я".repeat(MAX_POST_PROCESS_AUTO_TITLE_BYTES / 2))
                }
                3 => invalid.expected_metadata_revision = String::new(),
                4 => invalid.operation_id = Uuid::nil(),
                _ => invalid.proof.library_epoch = Uuid::new_v4(),
            }
            assert!(
                matches!(client.commit_post_process_auto_title(&worker,invalid).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
            );
        }
        request.title = "я".repeat(MAX_POST_PROCESS_AUTO_TITLE_BYTES / 2);
        assert_eq!(request.title.len(), MAX_POST_PROCESS_AUTO_TITLE_BYTES);
        assert!(client
            .commit_post_process_auto_title(&worker, request)
            .await
            .is_ok());
        peer.finish().await;
    });
}

#[test]
fn forged_receipt_identity_nonce_revision_sequence_and_wrong_kind_poison_the_client() {
    run(async {
        for mode in 0..16 {
            let mut peer = fake(move |mut stream, d| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::CommitPostProcessAutoTitle(request) = &frame.request else {
                    panic!("auto-title request");
                };
                let mut data = receipt(request);
                match mode {
                    0 => data.operation_id = Uuid::new_v4(),
                    1 => data.claim_id = Uuid::new_v4(),
                    2 => data.worker_session_id = Uuid::new_v4(),
                    3 => data.library_epoch = Uuid::new_v4(),
                    4 => data.job_id = "another-job".into(),
                    5 => data.meeting_id = Uuid::new_v4(),
                    6 => data.metadata_revision = String::new(),
                    7 => data.change_sequence = 0,
                    11 => data.metadata_revision = request.expected_metadata_revision.clone(),
                    12 => data.change_sequence = request.lease.change_sequence,
                    13 => data.change_sequence = 9_007_199_254_740_992,
                    14 => {
                        data.dedup_until = (std::time::SystemTime::now()
                            + Duration::from_secs(8 * 24 * 60 * 60))
                        .into()
                    }
                    15 => data.dedup_until = std::time::UNIX_EPOCH.into(),
                    _ => {}
                }
                let mut reply = envelope(&d, data);
                if mode == 8 {
                    if let Reply::PostProcessAutoTitle(value) = &mut reply {
                        value.library_id = Uuid::new_v4();
                    }
                }
                if mode == 9 {
                    if let Reply::PostProcessAutoTitle(value) = &mut reply {
                        value.library_epoch = Uuid::new_v4();
                    }
                }
                if mode == 10 {
                    reply = Reply::Status(status(&d));
                }
                send(&mut stream, &frame, reply).await;
            });
            let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
            let (worker, request) = request(&client);
            assert!(matches!(
                client
                    .commit_post_process_auto_title(&worker, request)
                    .await,
                Err(ClientError::Protocol)
            ));
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            peer.finish().await;
        }
    });
}

#[test]
fn remote_conflict_does_not_retry_or_fetch_current_metadata() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::CommitPostProcessAutoTitle(request) = &frame.request else {
                panic!("auto-title request");
            };
            assert_eq!(request.expected_metadata_revision, "metadata-read-1");
            send(
                &mut stream,
                &frame,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(
                matches!(frame.request, Request::GetStatus),
                "only an explicit caller status request follows a conflict"
            );
            send(&mut stream, &frame, Reply::Status(status(&d))).await;
        });
        let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
        let (worker, request) = request(&client);
        assert!(
            matches!(client.commit_post_process_auto_title(&worker,request).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        assert!(client.status().await.is_ok());
        peer.finish().await;
    });
}

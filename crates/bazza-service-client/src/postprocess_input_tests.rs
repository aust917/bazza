#![cfg(any(windows, target_os = "macos"))]
use super::*;
use crate::*;
use bazza_service_api::{
    content::*,
    postprocess::{PostProcessJobLease, PostProcessSourceFence},
    postprocess_input::FIXTURE_POST_PROCESS_INPUT_CAPABILITY,
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
            tokio::time::timeout(Duration::from_secs(5), future)
                .await
                .expect("bounded private post-process input fake-peer test");
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
                    FIXTURE_POST_PROCESS_INPUT_CAPABILITY.into(),
                ],
            }),
        )
        .await;
        serve(stream, descriptor).await;
    });
    Fake { root, task }
}

fn envelope<T>(descriptor: &EndpointDescriptor, data: T) -> DesktopEnvelope<T> {
    DesktopEnvelope {
        library_id: descriptor.library_id,
        library_epoch: descriptor.library_epoch,
        data,
    }
}

fn request(client: &ServiceClient) -> (RegisteredWorker, PostProcessTranscriptRequest) {
    let now = std::time::SystemTime::now();
    let worker = RegisteredWorker {
        session_id: Uuid::new_v4(),
        library_epoch: client.descriptor.library_epoch,
        peer_pid: std::process::id(),
        capabilities: vec![WorkerCapability::PostProcess],
        expires_at: (now + Duration::from_secs(240)).into(),
    };
    let meeting = Uuid::new_v4();
    let request = PostProcessTranscriptRequest {
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
            job_id: "processing-job".into(),
            meeting_id: meeting,
            job_revision: "claimed-job".into(),
            input: PostProcessSourceFence {
                checkpoint_revision: "checkpoint-initial".into(),
                capture_id: Uuid::new_v4(),
                capture_generation: Uuid::new_v4(),
                lifecycle_revision: "finalized-lifecycle".into(),
                transcript_revision: "original-transcript".into(),
                source_id: Some("captured-source".into()),
            },
            expires_at: (now + Duration::from_secs(120)).into(),
            change_sequence: 9,
            dedup_until: (now + Duration::from_secs(7 * 24 * 60 * 60)).into(),
        },
        fence: WorkerSecret::random(),
        page: DesktopTranscriptRequest {
            meeting_id: meeting.to_string(),
            source_id: None,
            expected_revision: "original-transcript".into(),
            representation: DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 1,
            max_bytes: 4096,
        },
    };
    assert!(request.valid());
    (worker, request)
}

fn page(request: &PostProcessTranscriptRequest) -> DesktopTranscriptPage {
    DesktopTranscriptPage {
        library_scope: "a".repeat(32),
        state: DesktopTranscriptState {
            meeting_id: request.page.meeting_id.clone(),
            revision: request.page.expected_revision.clone(),
            current_source_id: request.lease.input.source_id.clone(),
            document_state: if request.lease.input.source_id.is_some() {
                DesktopDocumentState::Present
            } else {
                DesktopDocumentState::Absent
            },
            deletion_epoch: 0,
        },
        source_id: request.lease.input.source_id.clone(),
        representation: request.page.representation,
        items: if request.lease.input.source_id.is_some() {
            vec![DesktopTranscriptFragment {
                segment_index: 0,
                segment_id: 1,
                start_ms: 0,
                end_ms: 100,
                speaker: Some("Анна".into()),
                text: "原\0😀".into(),
                byte_offset: 0,
                total_bytes: 8,
                has_clean_text: true,
                clean_text_manual: true,
                effective_origin: DesktopEffectiveOrigin::Manual,
            }]
        } else {
            Vec::new()
        },
        next: None,
    }
}

fn cursor(page: &DesktopTranscriptPage, offset: u64) -> DesktopTranscriptCursor {
    DesktopTranscriptCursor {
        library_scope: page.library_scope.clone(),
        meeting_id: page.state.meeting_id.clone(),
        revision: page.state.revision.clone(),
        current_source_id: page.state.current_source_id.clone(),
        source_id: page.source_id.clone(),
        representation: page.representation,
        segment_index: 0,
        byte_offset: offset,
        query_hash: "b".repeat(64),
    }
}

#[test]
fn explicit_utf8_pages_preserve_raw_nul_and_the_original_claim_without_collection() {
    run(async {
        let mut server = fake(|mut stream, descriptor| async move {
            let first: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessTranscript(request) = &first.request else {
                panic!("post-process transcript")
            };
            assert!(request.valid());
            assert_eq!(request.page.source_id, None);
            let original =
                serde_json::to_value((&request.proof, &request.lease, &request.fence)).unwrap();
            let mut value = page(request);
            value.items[0].text = "原".into();
            value.next = Some(cursor(&value, 3));
            send(
                &mut stream,
                &first,
                Reply::PostProcessTranscript(Box::new(envelope(&descriptor, value))),
            )
            .await;
            let second: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessTranscript(request) = &second.request else {
                panic!("explicit continuation")
            };
            assert_eq!(
                serde_json::to_value((&request.proof, &request.lease, &request.fence)).unwrap(),
                original
            );
            assert_eq!(request.page.cursor.as_ref().unwrap().byte_offset, 3);
            let mut value = page(request);
            value.items[0].text = "\0😀".into();
            value.items[0].byte_offset = 3;
            send(
                &mut stream,
                &second,
                Reply::PostProcessTranscript(Box::new(envelope(&descriptor, value))),
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
        let (worker, original) = request(&client);
        let first = client
            .post_process_transcript(&worker, original.clone())
            .await
            .unwrap();
        assert_eq!(first.library_id, client.descriptor.library_id);
        assert_eq!(first.data.items[0].text, "原");
        let mut continuation = original;
        continuation.page.cursor = first.data.next;
        let last = client
            .post_process_transcript(&worker, continuation)
            .await
            .unwrap();
        assert_eq!(last.data.items[0].text, "\0😀");
        assert!(last.data.next.is_none());
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn effective_manual_empty_and_absent_input_return_exact_terminal_states() {
    run(async {
        for absent in [false, true] {
            let mut server = fake(move |mut stream, descriptor| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessTranscript(request) = &frame.request else {
                    panic!("post-process transcript")
                };
                let mut value = page(request);
                if !absent {
                    value.items[0].text.clear();
                    value.items[0].total_bytes = 0;
                }
                send(
                    &mut stream,
                    &frame,
                    Reply::PostProcessTranscript(Box::new(envelope(&descriptor, value))),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let (worker, mut request) = request(&client);
            request.page.representation = DesktopTranscriptRepresentation::Effective;
            if absent {
                request.lease.input.source_id = None;
            }
            let value = client
                .post_process_transcript(&worker, request)
                .await
                .unwrap();
            assert!(value.data.next.is_none());
            if absent {
                assert_eq!(
                    value.data.state.document_state,
                    DesktopDocumentState::Absent
                );
                assert!(value.data.items.is_empty());
                assert!(value.data.source_id.is_none());
            } else {
                assert_eq!(value.data.items.len(), 1);
                assert!(value.data.items[0].text.is_empty());
                assert!(value.data.items[0].clean_text_manual);
            }
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn foreign_sources_absent_forgery_bad_progress_and_budget_poison_the_stream() {
    run(async {
        for fault in 0..9 {
            let mut server = fake(move |mut stream, descriptor| async move {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::PostProcessTranscript(request) = &frame.request else {
                    panic!("post-process transcript")
                };
                let mut value = page(request);
                match fault {
                    0 => {
                        value.state.current_source_id = Some("other-current".into());
                        value.source_id = value.state.current_source_id.clone();
                    }
                    1 => value.source_id = Some("archive".into()),
                    2 => value.state.document_state = DesktopDocumentState::Deleted,
                    3 => value.state.revision = "refreshed-revision".into(),
                    4 => value.state.meeting_id = Uuid::new_v4().to_string(),
                    5 => {
                        value.items[0].text = "x".repeat(request.page.max_bytes as usize);
                        value.items[0].total_bytes = value.items[0].text.len() as u64;
                    }
                    6 => {
                        value.items[0].text = "原".into();
                        value.next = Some(cursor(&value, 2));
                    }
                    7 => value.representation = DesktopTranscriptRepresentation::Effective,
                    _ => {}
                }
                let mut value = envelope(&descriptor, value);
                if fault == 8 {
                    value.library_id = Uuid::new_v4();
                }
                send(
                    &mut stream,
                    &frame,
                    Reply::PostProcessTranscript(Box::new(value)),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let (worker, mut request) = request(&client);
            if fault == 2 {
                request.lease.input.source_id = None;
            }
            assert!(
                matches!(
                    client.post_process_transcript(&worker, request).await,
                    Err(ClientError::Protocol)
                ),
                "fault {fault}"
            );
            assert!(client.stream.is_none());
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn expired_foreign_or_unregistered_input_is_rejected_before_any_wire_call() {
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
        let (worker, original) = request(&client);
        for fault in 0..8 {
            let mut worker = worker.clone();
            let mut request = original.clone();
            match fault {
                0 => worker.peer_pid = 0,
                1 => worker.library_epoch = Uuid::new_v4(),
                2 => worker.capabilities = vec![WorkerCapability::Capture],
                3 => {
                    worker.expires_at =
                        (std::time::SystemTime::now() - Duration::from_secs(1)).into()
                }
                4 => {
                    request.lease.expires_at =
                        (std::time::SystemTime::now() - Duration::from_secs(1)).into()
                }
                5 => request.proof.session_id = Uuid::new_v4(),
                6 => request.page.expected_revision = "refreshed-source".into(),
                _ => {
                    request.page.cursor = Some(cursor(&page(&request), 3));
                    request.page.cursor.as_mut().unwrap().current_source_id =
                        Some("foreign-current".into());
                }
            }
            assert!(
                matches!(
                    client.post_process_transcript(&worker, request).await,
                    Err(ClientError::Remote(error)) if error.code == ErrorCode::InvalidArgument
                ),
                "fault {fault}"
            );
            assert!(client.stream.is_some());
        }
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn conflict_preserves_original_request_and_does_not_refetch_or_retry_implicitly() {
    run(async {
        let mut server = fake(|mut stream, _| async move {
            let first: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessTranscript(request) = &first.request else {
                panic!("post-process transcript")
            };
            let original = serde_json::to_value(request).unwrap();
            send(
                &mut stream,
                &first,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            let second: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessTranscript(request) = &second.request else {
                panic!("explicit unchanged retry")
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
        let (worker, original) = request(&client);
        for _ in 0..2 {
            assert!(matches!(
                client.post_process_transcript(&worker, original.clone()).await,
                Err(ClientError::Remote(error)) if error.code == ErrorCode::Conflict
            ));
            assert!(client.stream.is_some());
        }
        (&mut server.task).await.unwrap();
    });
}

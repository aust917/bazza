#![cfg(any(windows, target_os = "macos"))]
use crate::*;
use bazza_service_api::{content::*, desktop::*};
use bazza_service_transport::Listener;
use repository::{
    RepositorySummaryRequest, RepositoryTranscriptRequest, SummaryContinuation,
    TranscriptContinuation,
};
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
                .expect("bounded private content fake-peer test");
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
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
        assert!(matches!(request.request, Request::Hello(_)));
        send(
            &mut stream,
            &request,
            Reply::Hello(ServiceStatus {
                api: ApiVersion::CURRENT,
                schema_version: STORAGE_SCHEMA_MAX,
                build_id: build_id().into(),
                library_id: d.library_id,
                library_epoch: d.library_epoch,
                process_id: d.process_id,
                authentication: "local_os_user".into(),
                capabilities: vec![
                    FIXTURE_STATUS_CAPABILITY.into(),
                    FIXTURE_METADATA_CAPABILITY.into(),
                    FIXTURE_DESKTOP_CAPABILITY.into(),
                    FIXTURE_CONTENT_CAPABILITY.into(),
                ],
                development_fixture_only: true,
            }),
        )
        .await;
        serve(stream, d).await;
    });
    Fake { root, task }
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
fn envelope<T>(d: &EndpointDescriptor, data: T) -> DesktopEnvelope<T> {
    DesktopEnvelope {
        library_id: d.library_id,
        library_epoch: d.library_epoch,
        data,
    }
}
async fn bootstrap(stream: &mut PeerStream, d: &EndpointDescriptor) {
    let request: RequestEnvelope = read_frame(stream).await.unwrap();
    assert!(matches!(request.request, Request::DesktopBootstrap));
    send(
        stream,
        &request,
        Reply::DesktopBootstrap(envelope(
            d,
            DesktopBootstrap {
                preferences: DesktopPreferences {
                    revision: "preferences-1".into(),
                    ui_locale: DesktopLocale::Ru,
                    close_to_tray: true,
                    hotkey: "Ctrl+R".into(),
                },
                change_sequence: 0,
                cursor_scope: "a".repeat(32),
            },
        )),
    )
    .await;
}
fn transcript_request() -> DesktopTranscriptRequest {
    DesktopTranscriptRequest {
        meeting_id: "legacy-meeting".into(),
        source_id: None,
        expected_revision: "transcript-1".into(),
        representation: DesktopTranscriptRepresentation::Raw,
        cursor: None,
        limit: 1,
        max_bytes: 4096,
    }
}
fn transcript() -> DesktopTranscriptPage {
    DesktopTranscriptPage {
        library_scope: "a".repeat(32),
        state: DesktopTranscriptState {
            meeting_id: "legacy-meeting".into(),
            revision: "transcript-1".into(),
            current_source_id: Some("legacy-source".into()),
            document_state: DesktopDocumentState::Present,
            deletion_epoch: 0,
        },
        source_id: Some("legacy-source".into()),
        representation: DesktopTranscriptRepresentation::Raw,
        items: vec![DesktopTranscriptFragment {
            segment_index: 0,
            segment_id: 1,
            start_ms: 0,
            end_ms: 100,
            speaker: Some("Sam".into()),
            text: "原\0文".into(),
            byte_offset: 0,
            total_bytes: 7,
            has_clean_text: true,
            clean_text_manual: true,
            effective_origin: DesktopEffectiveOrigin::Manual,
        }],
        next: None,
    }
}
fn transcript_cursor(
    page: &DesktopTranscriptPage,
    index: u64,
    offset: u64,
) -> DesktopTranscriptCursor {
    DesktopTranscriptCursor {
        library_scope: page.library_scope.clone(),
        meeting_id: page.state.meeting_id.clone(),
        revision: page.state.revision.clone(),
        current_source_id: page.state.current_source_id.clone(),
        source_id: page.source_id.clone(),
        representation: page.representation,
        segment_index: index,
        byte_offset: offset,
        query_hash: "b".repeat(64),
    }
}
fn summary_request() -> DesktopSummaryRequest {
    DesktopSummaryRequest {
        meeting_id: "legacy-meeting".into(),
        expected_revision: "summary-1".into(),
        field: DesktopSummaryField::Markdown,
        cursor: None,
        max_bytes: 4096,
    }
}
fn summary() -> DesktopSummaryPage {
    DesktopSummaryPage {
        library_scope: "a".repeat(32),
        state: DesktopSummaryState {
            meeting_id: "legacy-meeting".into(),
            revision: "summary-1".into(),
            document_state: DesktopDocumentState::Present,
            deletion_epoch: 0,
            automatic_generation_suppressed: false,
            origin: "manual".into(),
            auto_replace: false,
            source_label: Some("Editor".into()),
            source_revision: Some("transcript-1".into()),
            source_missing: false,
        },
        field: DesktopSummaryField::Markdown,
        text: "原文".into(),
        byte_offset: 0,
        total_bytes: 6,
        next: None,
    }
}
fn summary_cursor(page: &DesktopSummaryPage, offset: u64) -> DesktopSummaryCursor {
    DesktopSummaryCursor {
        library_scope: page.library_scope.clone(),
        meeting_id: page.state.meeting_id.clone(),
        revision: page.state.revision.clone(),
        field: page.field,
        byte_offset: offset,
        query_hash: "c".repeat(64),
    }
}

#[test]
fn facade_content_pages_keep_identity_raw_bytes_and_manual_empty_values() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            for index in 0..5 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let reply = match index {
                    0 => {
                        assert!(
                            matches!(request.request,Request::DesktopTranscript(ref r) if r.representation==DesktopTranscriptRepresentation::Raw)
                        );
                        let mut page = transcript();
                        page.items[0].text = "🦀".repeat(100);
                        page.items[0].total_bytes = 200_000;
                        page.next = Some(transcript_cursor(&page, 0, 400));
                        Reply::DesktopTranscript(Box::new(envelope(&d, page)))
                    }
                    1 => {
                        assert!(
                            matches!(request.request,Request::DesktopTranscript(ref r) if r.cursor.as_ref().unwrap().byte_offset==400)
                        );
                        let mut page = transcript();
                        page.items[0].byte_offset = 400;
                        page.items[0].text = "🦀".repeat(100);
                        page.items[0].total_bytes = 200_000;
                        page.next = Some(transcript_cursor(&page, 0, 800));
                        Reply::DesktopTranscript(Box::new(envelope(&d, page)))
                    }
                    2 => {
                        assert!(
                            matches!(request.request,Request::DesktopTranscript(ref r) if r.representation==DesktopTranscriptRepresentation::Effective && r.cursor.is_none())
                        );
                        let mut page = transcript();
                        page.representation = DesktopTranscriptRepresentation::Effective;
                        page.items[0].text.clear();
                        page.items[0].total_bytes = 0;
                        Reply::DesktopTranscript(Box::new(envelope(&d, page)))
                    }
                    3 => {
                        assert!(
                            matches!(request.request,Request::DesktopSummary(ref r) if r.field==DesktopSummaryField::StructuredJson)
                        );
                        let mut page = summary();
                        page.field = DesktopSummaryField::StructuredJson;
                        page.text = "{\"текст\":\"".into();
                        page.total_bytes = page.text.len() as u64 + 5;
                        page.next = Some(summary_cursor(&page, page.text.len() as u64));
                        Reply::DesktopSummary(Box::new(envelope(&d, page)))
                    }
                    _ => {
                        let Request::DesktopSummary(ref r) = request.request else {
                            panic!("expected summary continuation")
                        };
                        let mut page = summary();
                        page.field = DesktopSummaryField::StructuredJson;
                        page.byte_offset = r.cursor.as_ref().unwrap().byte_offset;
                        page.text = "文\"}".into();
                        page.total_bytes = page.byte_offset + page.text.len() as u64;
                        Reply::DesktopSummary(Box::new(envelope(&d, page)))
                    }
                };
                send(&mut stream, &request, reply).await;
            }
        });
        let repo = RepositoryClient::connect(server.root.path()).await.unwrap();
        let identity = repo.identity().await;
        let mut request = RepositoryTranscriptRequest::new(
            "legacy-meeting".into(),
            "transcript-1".into(),
            DesktopTranscriptRepresentation::Raw,
        );
        request.max_bytes = 4096;
        request.limit = 1;
        let first = repo.transcript(request.clone()).await.unwrap();
        assert_eq!(first.identity, identity);
        assert_eq!(first.items[0].text.len(), 400);
        request.cursor = first.next;
        let next = repo.transcript(request.clone()).await.unwrap();
        assert_eq!(next.items[0].byte_offset, 400);
        assert_eq!(next.next.unwrap().cursor.byte_offset, 800);
        request.cursor = None;
        request.representation = DesktopTranscriptRepresentation::Effective;
        let empty = repo.transcript(request).await.unwrap();
        assert!(empty.items[0].text.is_empty());
        assert!(empty.items[0].has_clean_text);
        assert!(empty.items[0].clean_text_manual);
        assert!(empty.next.is_none());
        assert_eq!(empty.identity, identity);
        let mut request = RepositorySummaryRequest::new(
            "legacy-meeting".into(),
            "summary-1".into(),
            DesktopSummaryField::StructuredJson,
        );
        request.max_bytes = 4096;
        let first = repo.summary(request.clone()).await.unwrap();
        request.cursor = first.next;
        let last = repo.summary(request).await.unwrap();
        assert!(last.next.is_none());
        assert_eq!(last.identity, identity);
        let assembled = first.text + &last.text;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&assembled).unwrap()["текст"],
            "文"
        );
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn forged_transcript_pages_poison_the_connection() {
    run(async {
        for variant in 0..10 {
            let mut server = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(request.request, Request::DesktopTranscript(_)));
                let mut page = transcript();
                match variant {
                    0 => page.state.revision = "other-revision".into(),
                    1 => page.source_id = Some("other-source".into()),
                    2 => page.items[0].byte_offset = 1,
                    3 => page.items[0].total_bytes = 100,
                    4 => page.items.push(page.items[0].clone()),
                    5 => {
                        page.items[0].text = "\\".repeat(2000);
                        page.items[0].total_bytes = 2000;
                    }
                    6 => page.next = Some(transcript_cursor(&page, 0, 1)),
                    7 => page.library_scope = "not-a-scope".into(),
                    8 => {
                        page.items[0].clean_text_manual = false;
                        page.items[0].effective_origin = DesktopEffectiveOrigin::Manual;
                    }
                    _ => page.items[0].segment_index = 1,
                }
                send(
                    &mut stream,
                    &request,
                    Reply::DesktopTranscript(Box::new(envelope(&d, page))),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            assert!(
                matches!(
                    client.desktop_transcript(transcript_request()).await,
                    Err(ClientError::Protocol)
                ),
                "variant {variant}"
            );
            assert!(client.stream.is_none());
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn forged_summary_pages_poison_the_connection() {
    run(async {
        for variant in 0..9 {
            let mut server = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(request.request, Request::DesktopSummary(_)));
                let mut page = summary();
                match variant {
                    0 => page.state.revision = "other-revision".into(),
                    1 => page.field = DesktopSummaryField::StructuredJson,
                    2 => page.byte_offset = 1,
                    3 => page.total_bytes = 100,
                    4 => page.total_bytes = 1,
                    5 => {
                        page.text = "\"".repeat(2000);
                        page.total_bytes = 2000;
                    }
                    6 => page.next = Some(summary_cursor(&page, 1)),
                    7 => page.library_scope = "not-a-scope".into(),
                    _ => page.state.document_state = DesktopDocumentState::Deleted,
                }
                send(
                    &mut stream,
                    &request,
                    Reply::DesktopSummary(Box::new(envelope(&d, page))),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            assert!(
                matches!(
                    client.desktop_summary(summary_request()).await,
                    Err(ClientError::Protocol)
                ),
                "variant {variant}"
            );
            assert!(client.stream.is_none());
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn facade_rejects_foreign_scope_even_for_terminal_content_pages() {
    run(async {
        for is_summary in [false, true] {
            let mut server = fake(move |mut stream, d| async move {
                bootstrap(&mut stream, &d).await;
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let reply = if is_summary {
                    let mut page = summary();
                    page.library_scope = "d".repeat(32);
                    Reply::DesktopSummary(Box::new(envelope(&d, page)))
                } else {
                    let mut page = transcript();
                    page.library_scope = "d".repeat(32);
                    Reply::DesktopTranscript(Box::new(envelope(&d, page)))
                };
                send(&mut stream, &request, reply).await;
            });
            let repo = RepositoryClient::connect(server.root.path()).await.unwrap();
            let result = if is_summary {
                repo.summary(RepositorySummaryRequest::new(
                    "legacy-meeting".into(),
                    "summary-1".into(),
                    DesktopSummaryField::Markdown,
                ))
                .await
                .map(|_| ())
            } else {
                repo.transcript(RepositoryTranscriptRequest::new(
                    "legacy-meeting".into(),
                    "transcript-1".into(),
                    DesktopTranscriptRepresentation::Raw,
                ))
                .await
                .map(|_| ())
            };
            assert!(
                matches!(result,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
            );
            assert!(matches!(
                repo.preferences().await,
                Err(ClientError::Protocol)
            ));
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn stale_content_cursors_stop_before_wire_and_conflicts_do_not_refetch_or_retry() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            for is_summary in [false, true] {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                match &request.request {
                    Request::DesktopTranscript(r) if !is_summary => {
                        assert_eq!(r.expected_revision, "transcript-1")
                    }
                    Request::DesktopSummary(r) if is_summary => {
                        assert_eq!(r.expected_revision, "summary-1")
                    }
                    _ => panic!("unexpected refetch, retry or stale-cursor request"),
                }
                send(
                    &mut stream,
                    &request,
                    Reply::Error(RpcError::new(ErrorCode::Conflict)),
                )
                .await;
            }
        });
        let repo = RepositoryClient::connect(server.root.path()).await.unwrap();
        let identity = repo.identity().await;
        let mut tr = RepositoryTranscriptRequest::new(
            "legacy-meeting".into(),
            "transcript-1".into(),
            DesktopTranscriptRepresentation::Raw,
        );
        tr.cursor = Some(TranscriptContinuation {
            library_id: identity.library_id,
            library_epoch: Uuid::new_v4(),
            cursor: transcript_cursor(&transcript(), 0, 1),
        });
        assert!(
            matches!(repo.transcript(tr.clone()).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        tr.cursor = None;
        assert!(
            matches!(repo.transcript(tr).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        let mut sm = RepositorySummaryRequest::new(
            "legacy-meeting".into(),
            "summary-1".into(),
            DesktopSummaryField::Markdown,
        );
        sm.cursor = Some(SummaryContinuation {
            library_id: Uuid::new_v4(),
            library_epoch: identity.library_epoch,
            cursor: summary_cursor(&summary(), 1),
        });
        assert!(
            matches!(repo.summary(sm.clone()).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        sm.cursor = None;
        assert!(
            matches!(repo.summary(sm).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        (&mut server.task).await.unwrap();
    });
}

#![cfg(any(windows, target_os = "macos"))]
use crate::*;
use bazza_service_api::{desktop::*, library::*};
use bazza_service_transport::Listener;
use repository::LibraryContinuation;
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
                .expect("bounded private library fake-peer test");
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
                FIXTURE_LIBRARY_CAPABILITY.into(),
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
fn library_reply(d: &EndpointDescriptor, data: DesktopLibraryReply) -> Reply {
    Reply::DesktopLibrary(Box::new(envelope(d, data)))
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
                change_sequence: 7,
                cursor_scope: "a".repeat(32),
            },
        )),
    )
    .await;
}
fn folder(id: &str, name: &str) -> DesktopLibraryItem {
    DesktopLibraryItem::Folder {
        id: id.into(),
        name: name.into(),
        parent_id: None,
        default_template_id: None,
        meeting_count: 2,
        revision: "folder-read-1".into(),
    }
}
fn tag(id: &str, name: &str) -> DesktopLibraryItem {
    DesktopLibraryItem::Tag {
        id: id.into(),
        name: name.into(),
        meeting_count: 1,
        revision: "tag-read-1".into(),
    }
}
fn page() -> DesktopLibraryPage {
    DesktopLibraryPage {
        library_scope: "a".repeat(32),
        snapshot_sequence: 7,
        items: vec![folder("folder_legacy", "Folder"), tag("tag_legacy", "Tag")],
        next: None,
    }
}
fn cursor(offset: u64) -> DesktopLibraryCursor {
    DesktopLibraryCursor {
        library_scope: "a".repeat(32),
        snapshot_sequence: 7,
        query_hash: "b".repeat(64),
        offset,
    }
}
fn list(cursor: Option<DesktopLibraryCursor>, limit: u16, max_bytes: u32) -> DesktopLibraryRequest {
    DesktopLibraryRequest::List {
        cursor,
        limit,
        max_bytes,
    }
}
fn rename(key: &str) -> DesktopLibraryRequest {
    DesktopLibraryRequest::FolderRename {
        folder_id: "folder_legacy".into(),
        name: "Edited folder".into(),
        expected_revision: "folder-read-1".into(),
        idempotency_key: key.into(),
    }
}
fn receipt(request: &DesktopLibraryRequest, id: &str) -> DesktopLibraryMutationReply {
    DesktopLibraryMutationReply {
        operation: request.operation().unwrap(),
        outcome: DesktopMutationOutcome {
            entity_id: id.into(),
            revision: "committed-2".into(),
            change_sequence: 8,
            job_id: None,
            dedup_until: "2099-01-01T00:00:00Z".parse().unwrap(),
        },
    }
}
fn operations() -> Vec<DesktopLibraryRequest> {
    vec![
        DesktopLibraryRequest::FolderCreate {
            name: "Created folder".into(),
            parent_id: Some("folder_legacy".into()),
            idempotency_key: "folder-create".into(),
        },
        rename("folder-rename"),
        DesktopLibraryRequest::FolderMove {
            folder_id: "folder_legacy".into(),
            parent_id: None,
            expected_revision: "folder-read-1".into(),
            idempotency_key: "folder-move".into(),
        },
        DesktopLibraryRequest::FolderDelete {
            folder_id: "folder_legacy".into(),
            expected_revision: "folder-read-1".into(),
            idempotency_key: "folder-delete".into(),
        },
        DesktopLibraryRequest::FolderSetTemplate {
            folder_id: "folder_legacy".into(),
            template_id: Some("custom-template".into()),
            expected_revision: "folder-read-1".into(),
            idempotency_key: "folder-template".into(),
        },
        DesktopLibraryRequest::FolderUpdate {
            folder_id: "folder_legacy".into(),
            name: "Updated folder".into(),
            parent_id: None,
            default_template_id: None,
            expected_revision: "folder-read-1".into(),
            idempotency_key: "folder-update".into(),
        },
        DesktopLibraryRequest::TagCreate {
            name: "Created tag".into(),
            idempotency_key: "tag-create".into(),
        },
        DesktopLibraryRequest::TagRename {
            tag_id: "tag_legacy".into(),
            name: "Edited tag".into(),
            expected_revision: "tag-read-1".into(),
            idempotency_key: "tag-rename".into(),
        },
        DesktopLibraryRequest::TagDelete {
            tag_id: "tag_legacy".into(),
            expected_revision: "tag-read-1".into(),
            idempotency_key: "tag-delete".into(),
        },
        DesktopLibraryRequest::MeetingMove {
            meeting_id: "meeting-legacy".into(),
            folder_id: Some("folder_legacy".into()),
            expected_revision: "metadata-1".into(),
            idempotency_key: "meeting-move".into(),
        },
        DesktopLibraryRequest::MeetingTag {
            meeting_id: "meeting-legacy".into(),
            tag_id: "tag_legacy".into(),
            expected_revision: "metadata-1".into(),
            idempotency_key: "meeting-tag".into(),
        },
        DesktopLibraryRequest::MeetingUntag {
            meeting_id: "meeting-legacy".into(),
            tag_id: "tag_legacy".into(),
            expected_revision: "metadata-1".into(),
            idempotency_key: "meeting-untag".into(),
        },
        DesktopLibraryRequest::MeetingTagNamed {
            meeting_id: "meeting-legacy".into(),
            name: "New named tag".into(),
            expected_revision: "metadata-1".into(),
            idempotency_key: "meeting-tag-named".into(),
        },
    ]
}

#[test]
fn all_closed_library_operations_and_bounded_pages_use_the_typed_private_channel() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            for index in 0..2 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::DesktopLibrary(DesktopLibraryRequest::List { cursor: input, .. }) =
                    &request.request
                else {
                    panic!("expected list page")
                };
                let mut data = page();
                if index == 0 {
                    assert!(input.is_none());
                    data.next = Some(cursor(2));
                } else {
                    assert_eq!(input.as_ref().unwrap().offset, 2);
                    data.items = vec![tag("last-tag", "Zoo")];
                }
                send(
                    &mut stream,
                    &request,
                    library_reply(&d, DesktopLibraryReply::Page(data)),
                )
                .await;
            }
            for expected in operations() {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::DesktopLibrary(ref input) = request.request else {
                    panic!("expected private library mutation")
                };
                assert_eq!(
                    serde_json::to_value(input).unwrap(),
                    serde_json::to_value(&expected).unwrap()
                );
                let id = input
                    .target_id()
                    .unwrap_or(match input.operation().unwrap() {
                        DesktopLibraryOperation::FolderCreate => "created-folder",
                        _ => "created-tag",
                    });
                send(
                    &mut stream,
                    &request,
                    library_reply(&d, DesktopLibraryReply::Mutation(receipt(input, id))),
                )
                .await;
            }
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let first = client.desktop_library(list(None, 2, 4096)).await.unwrap();
        assert_eq!(first.library_id, client.descriptor.library_id);
        let DesktopLibraryReply::Page(first) = first.data else {
            panic!("expected first page")
        };
        assert_eq!(first.items.len(), 2);
        let last = client
            .desktop_library(list(first.next, 2, 4096))
            .await
            .unwrap();
        let DesktopLibraryReply::Page(last) = last.data else {
            panic!("expected terminal page")
        };
        assert!(last.next.is_none());
        assert_eq!(last.items.len(), 1);
        for request in operations() {
            let operation = request.operation().unwrap();
            let target = request.target_id().map(str::to_owned);
            let reply = client.desktop_library(request).await.unwrap();
            let DesktopLibraryReply::Mutation(reply) = reply.data else {
                panic!("expected typed receipt")
            };
            assert_eq!(reply.operation, operation);
            if let Some(id) = target {
                assert_eq!(reply.outcome.entity_id, id);
            }
            assert!(reply.outcome.job_id.is_none());
        }
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn forged_suboperations_targets_receipts_and_envelope_identity_poison_the_stream() {
    run(async {
        for variant in 0..10 {
            let mut server = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::DesktopLibrary(ref input) = request.request else {
                    panic!("expected library mutation")
                };
                let mut body = receipt(input, input.target_id().unwrap());
                match variant {
                    0 => body.operation = DesktopLibraryOperation::TagRename,
                    1 | 9 => body.outcome.entity_id = "tag_legacy".into(),
                    5 => body.outcome.job_id = Some("unexpected-job".into()),
                    6 => body.outcome.revision.clear(),
                    7 => body.outcome.change_sequence = 0,
                    _ => {}
                }
                let mut data = envelope(&d, DesktopLibraryReply::Mutation(body.clone()));
                if variant == 2 {
                    data.library_id = Uuid::new_v4();
                }
                if variant == 3 {
                    data.library_epoch = Uuid::new_v4();
                }
                if variant == 4 {
                    data.data = DesktopLibraryReply::Page(page());
                }
                let reply = if variant == 8 {
                    Reply::DesktopMutation(envelope(&d, body.outcome))
                } else {
                    Reply::DesktopLibrary(Box::new(data))
                };
                send(&mut stream, &request, reply).await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let request = if variant == 9 {
                operations().pop().unwrap()
            } else {
                rename("synthetic-key")
            };
            assert!(
                matches!(
                    client.desktop_library(request).await,
                    Err(ClientError::Protocol)
                ),
                "variant {variant}"
            );
            assert!(client.stream.is_none());
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn forged_page_counts_ids_order_scope_query_and_cursor_progress_are_rejected() {
    run(async {
        for variant in 0..17 {
            let mut server = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(
                    request.request,
                    Request::DesktopLibrary(DesktopLibraryRequest::List { .. })
                ));
                let mut data = page();
                data.items = vec![folder("folder-a", "A")];
                data.next = Some(cursor(4));
                match variant {
                    0 => data.next.as_mut().unwrap().offset = 3,
                    1 => data.next.as_mut().unwrap().offset = 5,
                    2 => data.snapshot_sequence = 8,
                    3 => data.library_scope = "d".repeat(32),
                    4 => data.next.as_mut().unwrap().query_hash = "c".repeat(64),
                    5 => data.next.as_mut().unwrap().library_scope = "d".repeat(32),
                    6 => data.next.as_mut().unwrap().snapshot_sequence = 8,
                    7 => data.next.as_mut().unwrap().query_hash = "invalid-hash".into(),
                    8 => {
                        data.items = vec![folder("a", "A"), folder("b", "B"), tag("c", "C")];
                        data.next.as_mut().unwrap().offset = 6;
                    }
                    9 => {
                        data.items = vec![folder("same-id", "A"), folder("same-id", "B")];
                        data.next.as_mut().unwrap().offset = 5;
                    }
                    10 => {
                        data.items = vec![folder("b", "B"), folder("a", "A")];
                        data.next.as_mut().unwrap().offset = 5;
                    }
                    11 => {
                        data.items.clear();
                        data.next = None;
                    }
                    12 => data.items = vec![tag("escaped-tag", &"\u{1}".repeat(300))],
                    14 => {
                        if let DesktopLibraryItem::Folder { meeting_count, .. } = &mut data.items[0]
                        {
                            *meeting_count = -1;
                        }
                    }
                    15 => data.items = vec![folder("../outside", "A")],
                    16 => data.library_scope = "invalid-scope".into(),
                    _ => {}
                }
                let mut reply = envelope(&d, DesktopLibraryReply::Page(data));
                if variant == 13 {
                    reply.library_id = Uuid::new_v4();
                }
                send(
                    &mut stream,
                    &request,
                    Reply::DesktopLibrary(Box::new(reply)),
                )
                .await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let request = list(Some(cursor(3)), 2, if variant == 12 { 1024 } else { 4096 });
            assert!(
                matches!(
                    client.desktop_library(request).await,
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
fn serialized_page_budget_includes_the_outer_tag_even_when_bare_page_fits() {
    run(async {
        // Find a small escaped-text boundary fixture: all item fields and bare
        // page fit, but the serde {kind:"page",data:...} wrapper crosses 1024.
        let data = (0..=512)
            .find_map(|length| {
                let mut page = page();
                page.items = vec![tag("boundary-tag", &"\u{1}".repeat(length))];
                let bare = serde_json::to_vec(&page).unwrap().len();
                let tagged = serde_json::to_vec(&DesktopLibraryReply::Page(page.clone()))
                    .unwrap()
                    .len();
                (bare <= 1024 && tagged > 1024).then_some(page)
            })
            .expect("escaped tag boundary must exist within the 512-byte name bound");
        assert!(data.items.iter().all(DesktopLibraryItem::valid));
        assert!(serde_json::to_vec(&data).unwrap().len() <= 1024);
        assert!(
            serde_json::to_vec(&DesktopLibraryReply::Page(data.clone()))
                .unwrap()
                .len()
                > 1024
        );
        let mut server = fake(move |mut stream, d| async move {
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            send(
                &mut stream,
                &request,
                library_reply(&d, DesktopLibraryReply::Page(data)),
            )
            .await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        assert!(matches!(
            client.desktop_library(list(None, 1, 1024)).await,
            Err(ClientError::Protocol)
        ));
        assert!(client.stream.is_none());
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn facade_rejects_foreign_scope_even_on_terminal_pages_and_keeps_no_fallback() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let mut page = page();
            page.library_scope = "d".repeat(32);
            assert!(page.next.is_none());
            send(
                &mut stream,
                &request,
                library_reply(&d, DesktopLibraryReply::Page(page)),
            )
            .await;
        });
        let repo = RepositoryClient::connect(server.root.path()).await.unwrap();
        assert!(
            matches!(repo.library_page(None,2,4096).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        assert!(matches!(
            repo.preferences().await,
            Err(ClientError::Protocol)
        ));
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn reconnect_changes_epoch_and_invalid_caller_continuations_never_reach_the_wire() {
    run(async {
        let root = tempfile::tempdir().unwrap();
        let initial = descriptor(root.path());
        let mut next = initial.clone();
        next.library_epoch = Uuid::new_v4();
        next.endpoint = session_endpoint_name(root.path(), next.library_epoch.as_bytes()).unwrap();
        let mut listener = Listener::bind(&initial.endpoint).unwrap();
        publish_endpoint(root.path(), &initial).unwrap();
        let next_server = next.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            hello(&mut stream, &initial).await;
            bootstrap(&mut stream, &initial).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let mut first = page();
            first.next = Some(cursor(2));
            send(
                &mut stream,
                &request,
                library_reply(&initial, DesktopLibraryReply::Page(first)),
            )
            .await;
            drop(stream);
            drop(listener);
            let mut listener = Listener::bind(&next_server.endpoint).unwrap();
            ready_tx.send(()).unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            hello(&mut stream, &next_server).await;
            bootstrap(&mut stream, &next_server).await;
            // Invalid caller inputs must all be rejected locally. The first
            // request after reconnect must be this explicit fresh list.
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(
                request.request,
                Request::DesktopLibrary(DesktopLibraryRequest::List {
                    cursor: None,
                    limit: 2,
                    max_bytes: 4096
                })
            ));
            send(
                &mut stream,
                &request,
                library_reply(&next_server, DesktopLibraryReply::Page(page())),
            )
            .await;
        });
        let mut server = Fake { root, task };
        let repo = RepositoryClient::connect(server.root.path()).await.unwrap();
        let old = repo.library_page(None, 2, 4096).await.unwrap();
        let old_cursor = old.next.unwrap();
        ready_rx.await.unwrap();
        publish_endpoint(server.root.path(), &next).unwrap();
        let identity = repo.reconnect().await.unwrap();
        assert_ne!(identity.library_epoch, old.identity.library_epoch);
        assert!(
            matches!(repo.library_page(Some(old_cursor.clone()),2,4096).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        let current = LibraryContinuation {
            library_id: identity.library_id,
            library_epoch: identity.library_epoch,
            cursor: old_cursor.cursor,
        };
        for variant in 0..5 {
            let mut invalid = current.clone();
            match variant {
                0 => invalid.library_id = Uuid::nil(),
                1 => invalid.cursor.library_scope = "d".repeat(32),
                2 => invalid.cursor.query_hash = "invalid".into(),
                3 => invalid.cursor.snapshot_sequence = -1,
                _ => invalid.cursor.offset = u64::MAX,
            }
            let expected = if variant < 2 {
                ErrorCode::ResyncRequired
            } else {
                ErrorCode::InvalidArgument
            };
            assert!(
                matches!(repo.library_page(Some(invalid),2,4096).await,Err(ClientError::Remote(error)) if error.code==expected),
                "variant {variant}"
            );
        }
        assert!(
            matches!(repo.library_page(None,0,4096).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        assert!(
            matches!(repo.library_page(None,2,MAX_DESKTOP_DATA_BYTES+1).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        assert!(
            matches!(repo.library_mutate(list(None,2,4096)).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        let mut invalid = rename("invalid-revision");
        if let DesktopLibraryRequest::FolderRename {
            expected_revision, ..
        } = &mut invalid
        {
            expected_revision.clear();
        }
        assert!(
            matches!(repo.library_mutate(invalid).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        let last = repo.library_page(None, 2, 4096).await.unwrap();
        assert_eq!(last.identity, identity);
        assert!(last.next.is_none());
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn conflicting_cloned_facade_preserves_read_revision_and_key_without_refetch_or_retry() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(
                request.request,
                Request::DesktopLibrary(DesktopLibraryRequest::List { .. })
            ));
            send(
                &mut stream,
                &request,
                library_reply(&d, DesktopLibraryReply::Page(page())),
            )
            .await;
            for expected_key in ["window-a", "window-b", "window-a"] {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::DesktopLibrary(ref input) = request.request else {
                    panic!("implicit metadata refetch instead of caller mutation")
                };
                assert_eq!(
                    serde_json::to_value(input).unwrap(),
                    serde_json::to_value(rename(expected_key)).unwrap()
                );
                let reply = if expected_key == "window-b" {
                    Reply::Error(RpcError::new(ErrorCode::Conflict))
                } else {
                    library_reply(
                        &d,
                        DesktopLibraryReply::Mutation(receipt(input, "folder_legacy")),
                    )
                };
                send(&mut stream, &request, reply).await;
            }
        });
        let repo = RepositoryClient::connect(server.root.path()).await.unwrap();
        let other = repo.clone();
        let read = repo.library_page(None, 2, 4096).await.unwrap();
        let DesktopLibraryItem::Folder { revision, .. } = &read.items[0] else {
            panic!("expected folder revision")
        };
        assert_eq!(revision, "folder-read-1");
        let original = rename("window-a");
        let first = repo.library_mutate(original.clone()).await.unwrap();
        assert_eq!(first.identity, read.identity);
        assert_eq!(first.data.outcome.revision, "committed-2");
        assert!(
            matches!(other.library_mutate(rename("window-b")).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        // Only the caller requests this exact immutable idempotency replay.
        let replay = other.library_mutate(original).await.unwrap();
        assert_eq!(replay.identity, first.identity);
        assert_eq!(
            serde_json::to_value(replay.data).unwrap(),
            serde_json::to_value(first.data).unwrap()
        );
        (&mut server.task).await.unwrap();
    });
}

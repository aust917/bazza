#![cfg(any(windows, target_os = "macos"))]
use crate::*;
use bazza_service_api::{chats::*, desktop::*};
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
            tokio::time::timeout(Duration::from_secs(8), future)
                .await
                .expect("bounded private chats fake-peer test");
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
fn envelope(d: &EndpointDescriptor, data: DesktopChatsReply) -> Reply {
    Reply::DesktopChats(Box::new(DesktopEnvelope {
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
                development_fixture_only: true,
                capabilities: vec![
                    FIXTURE_STATUS_CAPABILITY.into(),
                    FIXTURE_METADATA_CAPABILITY.into(),
                    FIXTURE_DESKTOP_CAPABILITY.into(),
                    FIXTURE_CHATS_CAPABILITY.into(),
                ],
            }),
        )
        .await;
        serve(stream, d).await;
    });
    Fake {
        root,
        task: Some(task),
    }
}
async fn bootstrap(stream: &mut PeerStream, d: &EndpointDescriptor) {
    let request: RequestEnvelope = read_frame(stream).await.unwrap();
    assert!(matches!(request.request, Request::DesktopBootstrap));
    send(
        stream,
        &request,
        Reply::DesktopBootstrap(DesktopEnvelope {
            library_id: d.library_id,
            library_epoch: d.library_epoch,
            data: DesktopBootstrap {
                preferences: DesktopPreferences {
                    revision: "preferences-1".into(),
                    ui_locale: DesktopLocale::Ru,
                    close_to_tray: true,
                    hotkey: "Ctrl+R".into(),
                },
                change_sequence: 1,
                cursor_scope: "a".repeat(32),
            },
        }),
    )
    .await;
}

fn header() -> DesktopChatHeader {
    let title = " Chat \0🦀";
    let kind = "meeting\0 произвольный";
    DesktopChatHeader {
        id: "legacy / чат".into(),
        rowid: i64::MAX.to_string(),
        generation: "chat-generation-1".into(),
        metadata_revision: "chat-metadata-1".into(),
        messages_revision: "chat-messages-1".into(),
        erasure_revision: "chat-erasure-1".into(),
        title_preview: title.into(),
        scope_kind_preview: kind.into(),
        scope_id_preview: None,
        title_bytes: title.len() as u64,
        scope_kind_bytes: kind.len() as u64,
        scope_id_bytes: None,
        created_at: "2026-11-02T08:00:00+05:00".parse().unwrap(),
    }
}
fn message() -> DesktopChatMessageHeader {
    let role = "custom / system\0 роль";
    DesktopChatMessageHeader {
        id: "legacy / message".into(),
        rowid: i64::MAX.to_string(),
        chat_id: header().id,
        generation: "message-generation-1".into(),
        revision: "message-1".into(),
        role_preview: role.into(),
        role_bytes: role.len() as u64,
        content_bytes: 8_000_000,
        citations_bytes: 8_000_000,
        created_at: "2026-11-02T03:00:00Z".parse().unwrap(),
    }
}
fn list(cursor: Option<DesktopChatListCursor>) -> DesktopChatsRequest {
    DesktopChatsRequest::List {
        scope: DesktopChatScope::AllCatalog,
        limit: 2,
        max_bytes: 4096,
        cursor,
    }
}
fn describe() -> DesktopChatsRequest {
    DesktopChatsRequest::Describe {
        chat_id: header().id,
    }
}
fn messages(cursor: Option<DesktopChatMessagesCursor>) -> DesktopChatsRequest {
    DesktopChatsRequest::Messages {
        chat_id: header().id,
        expected_generation: header().generation,
        expected_messages_revision: header().messages_revision,
        limit: 2,
        max_bytes: 4096,
        cursor,
    }
}
fn chat_field(field: DesktopChatField) -> DesktopChatsRequest {
    DesktopChatsRequest::ReadChatField {
        chat_id: header().id,
        expected_generation: header().generation,
        expected_metadata_revision: header().metadata_revision,
        field,
        cursor: None,
        max_bytes: 4096,
    }
}
fn message_field(field: DesktopChatMessageField) -> DesktopChatsRequest {
    DesktopChatsRequest::ReadMessageField {
        chat_id: header().id,
        message_id: message().id,
        expected_chat_generation: header().generation,
        expected_generation: message().generation,
        expected_revision: message().revision,
        field,
        cursor: None,
        max_bytes: 4096,
    }
}
fn list_page(next: bool) -> DesktopChatsReply {
    DesktopChatsReply::List(DesktopChatListPage {
        library_scope: "a".repeat(32),
        catalog_revision: "catalog-1".into(),
        items: vec![header()],
        next: next.then(|| DesktopChatListCursor {
            library_scope: "a".repeat(32),
            catalog_revision: "catalog-1".into(),
            query_hash: "b".repeat(64),
            after_rowid: header().rowid,
        }),
    })
}
fn messages_page(next: bool) -> DesktopChatsReply {
    DesktopChatsReply::Messages(DesktopChatMessagesPage {
        library_scope: "a".repeat(32),
        chat_id: header().id,
        generation: header().generation,
        messages_revision: header().messages_revision,
        items: vec![message()],
        next: next.then(|| DesktopChatMessagesCursor {
            library_scope: "a".repeat(32),
            chat_id: header().id,
            generation: header().generation,
            messages_revision: header().messages_revision,
            query_hash: "c".repeat(64),
            after_rowid: message().rowid,
        }),
    })
}
fn chat_page(
    request: &DesktopChatsRequest,
    text: &str,
    total_bytes: u64,
    present: bool,
) -> DesktopChatsReply {
    let DesktopChatsRequest::ReadChatField {
        chat_id,
        expected_generation,
        expected_metadata_revision,
        field,
        cursor,
        max_bytes,
    } = request
    else {
        panic!("expected chat field request")
    };
    let byte_offset = cursor.as_ref().map_or(0, |cursor| cursor.byte_offset);
    let end = byte_offset + text.len() as u64;
    DesktopChatsReply::ChatField(DesktopChatFieldPage {
        library_scope: "a".repeat(32),
        chat_id: chat_id.clone(),
        generation: expected_generation.clone(),
        metadata_revision: expected_metadata_revision.clone(),
        field: *field,
        byte_offset,
        total_bytes,
        present,
        text: text.into(),
        next: (end < total_bytes).then(|| DesktopChatFieldCursor {
            library_scope: "a".repeat(32),
            chat_id: chat_id.clone(),
            generation: expected_generation.clone(),
            metadata_revision: expected_metadata_revision.clone(),
            field: *field,
            byte_offset: end,
            total_bytes,
            present,
            max_bytes: *max_bytes,
        }),
    })
}
fn message_page(request: &DesktopChatsRequest, text: &str, total_bytes: u64) -> DesktopChatsReply {
    let DesktopChatsRequest::ReadMessageField {
        chat_id,
        message_id,
        expected_chat_generation,
        expected_generation,
        expected_revision,
        field,
        cursor,
        max_bytes,
    } = request
    else {
        panic!("expected message field request")
    };
    let byte_offset = cursor.as_ref().map_or(0, |cursor| cursor.byte_offset);
    let end = byte_offset + text.len() as u64;
    DesktopChatsReply::MessageField(DesktopChatMessageFieldPage {
        library_scope: "a".repeat(32),
        chat_id: chat_id.clone(),
        message_id: message_id.clone(),
        chat_generation: expected_chat_generation.clone(),
        generation: expected_generation.clone(),
        revision: expected_revision.clone(),
        field: *field,
        byte_offset,
        total_bytes,
        text: text.into(),
        next: (end < total_bytes).then(|| DesktopChatMessageFieldCursor {
            library_scope: "a".repeat(32),
            chat_id: chat_id.clone(),
            message_id: message_id.clone(),
            chat_generation: expected_chat_generation.clone(),
            generation: expected_generation.clone(),
            revision: expected_revision.clone(),
            field: *field,
            byte_offset: end,
            total_bytes,
            max_bytes: *max_bytes,
        }),
    })
}

#[test]
fn five_read_operations_preserve_legacy_text_scope_nulls_and_original_row_order() {
    run(async {
        let mut absent = header();
        absent.id = "scope-null".into();
        let mut empty = header();
        empty.id = "scope-empty".into();
        empty.rowid = i64::MIN.to_string();
        empty.scope_id_preview = Some(String::new());
        empty.scope_id_bytes = Some(0);
        let mut absent_field = chat_field(DesktopChatField::ScopeId);
        let DesktopChatsRequest::ReadChatField { chat_id, .. } = &mut absent_field else {
            unreachable!()
        };
        *chat_id = absent.id.clone();
        let mut empty_field = chat_field(DesktopChatField::ScopeId);
        let DesktopChatsRequest::ReadChatField { chat_id, .. } = &mut empty_field else {
            unreachable!()
        };
        *chat_id = empty.id.clone();
        let mut rows = list_page(false);
        let DesktopChatsReply::List(page) = &mut rows else {
            unreachable!()
        };
        page.items = vec![absent.clone(), empty.clone()];
        let role_request = message_field(DesktopChatMessageField::Role);
        let role = message().role_preview;
        let citations_request = message_field(DesktopChatMessageField::CitationsStoredJson);
        let citations = "{not-json\0 [ arbitrary citations";
        let exact = |scope_id| DesktopChatsRequest::List {
            scope: DesktopChatScope::Exact {
                kind: header().scope_kind_preview,
                scope_id,
            },
            limit: 2,
            max_bytes: 4096,
            cursor: None,
        };
        let mut absent_list = list_page(false);
        let DesktopChatsReply::List(page) = &mut absent_list else {
            unreachable!()
        };
        page.items = vec![absent.clone()];
        let mut empty_list = list_page(false);
        let DesktopChatsReply::List(page) = &mut empty_list else {
            unreachable!()
        };
        page.items = vec![empty.clone()];
        let cases = vec![
            (list(None), rows),
            (describe(), DesktopChatsReply::Header(header())),
            (messages(None), messages_page(false)),
            (absent_field.clone(), chat_page(&absent_field, "", 0, false)),
            (empty_field.clone(), chat_page(&empty_field, "", 0, true)),
            (
                role_request.clone(),
                message_page(&role_request, &role, role.len() as u64),
            ),
            (
                citations_request.clone(),
                message_page(&citations_request, citations, citations.len() as u64),
            ),
            (exact(None), absent_list),
            (exact(Some(String::new())), empty_list),
        ];
        let served = cases.clone();
        let mut server = fake(move |mut stream, d| async move {
            for (expected, reply) in served {
                assert!(expected.valid());
                assert!(reply.valid_for(&expected));
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(
                    matches!(&request.request, Request::DesktopChats(actual) if **actual == expected)
                );
                send(&mut stream, &request, envelope(&d, reply)).await;
            }
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        for (request, expected) in cases {
            let got = client.desktop_chats(request).await.unwrap();
            assert_eq!(got.data, expected);
        }
        server.finish().await;
    });
}

#[test]
fn facade_continuations_pin_query_identity_and_utf8_boundaries_without_assembling_fields() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            for index in 0..9 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::DesktopChats(ref actual) = request.request else {
                    panic!("closed chats request")
                };
                let reply = match index {
                    0 => {
                        assert_eq!(**actual, list(None));
                        list_page(true)
                    }
                    1 => {
                        assert!(
                            matches!(actual.as_ref(), DesktopChatsRequest::List { cursor: Some(cursor), .. } if cursor.after_rowid==i64::MAX.to_string())
                        );
                        let mut page = list_page(false);
                        let DesktopChatsReply::List(page) = &mut page else {
                            unreachable!()
                        };
                        page.items[0].id = "next chat".into();
                        page.items[0].rowid = i64::MIN.to_string();
                        DesktopChatsReply::List(page.clone())
                    }
                    2 => {
                        assert_eq!(**actual, messages(None));
                        messages_page(true)
                    }
                    3 => {
                        assert!(
                            matches!(actual.as_ref(), DesktopChatsRequest::Messages { cursor: Some(cursor), .. } if cursor.after_rowid==i64::MAX.to_string())
                        );
                        let mut page = messages_page(false);
                        let DesktopChatsReply::Messages(page) = &mut page else {
                            unreachable!()
                        };
                        page.items[0].id = "next message".into();
                        page.items[0].rowid = i64::MIN.to_string();
                        DesktopChatsReply::Messages(page.clone())
                    }
                    4 | 5 => {
                        assert!(
                            matches!(actual.as_ref(), DesktopChatsRequest::ReadChatField { field: DesktopChatField::Title, cursor, .. } if cursor.as_ref().map_or(0, |cursor|cursor.byte_offset)==if index==4 {0}else{500})
                        );
                        chat_page(actual, &"🦀\0".repeat(100), 8_000_000, true)
                    }
                    6 | 7 => {
                        assert!(
                            matches!(actual.as_ref(), DesktopChatsRequest::ReadMessageField { field: DesktopChatMessageField::CitationsStoredJson, cursor, .. } if cursor.as_ref().map_or(0, |cursor|cursor.byte_offset)==if index==6 {0}else{720})
                        );
                        message_page(actual, &"invalid\0{".repeat(80), 8_000_000)
                    }
                    8 => {
                        assert_eq!(**actual, describe());
                        DesktopChatsReply::Header(header())
                    }
                    _ => unreachable!(),
                };
                assert!(reply.valid_for(actual));
                send(&mut stream, &request, envelope(&d, reply)).await;
            }
        });
        let repository = RepositoryClient::connect(server.root.path()).await.unwrap();
        assert_eq!(
            repository.root(),
            server.root.path().canonicalize().unwrap()
        );
        let identity = repository.identity().await;
        let first = repository.chats(list(None)).await.unwrap();
        assert_eq!(first.identity, identity);
        let continuation = first.next.unwrap();
        let encoded = serde_json::to_value(&continuation).unwrap();
        for fault in 0..10 {
            let mut value = encoded.clone();
            match fault {
                0 => value["identity"]["library_id"] = serde_json::json!(Uuid::new_v4()),
                1 => value["identity"]["library_epoch"] = serde_json::json!(Uuid::new_v4()),
                2 => value["identity"]["cursor_scope"] = serde_json::json!("d".repeat(32)),
                3 => {
                    value["request"]["data"]["cursor"]["library_scope"] =
                        serde_json::json!("d".repeat(32))
                }
                4 => {
                    value["request"]["data"]["cursor"]["catalog_revision"] =
                        serde_json::json!("other-catalog")
                }
                5 => {
                    value["request"]["data"]["cursor"]["query_hash"] =
                        serde_json::json!("d".repeat(64))
                }
                6 => value["request"]["data"]["cursor"]["after_rowid"] = serde_json::json!("-7"),
                7 => value["request"]["data"]["limit"] = serde_json::json!(1),
                8 => {
                    value["initial"]["data"]["scope"] =
                        serde_json::json!({"mode":"exact","kind":"all","scope_id":null})
                }
                9 => value["request"]["data"]["max_bytes"] = serde_json::json!(2048),
                _ => unreachable!(),
            }
            let forged = serde_json::from_value(value).unwrap();
            assert!(
                matches!(repository.continue_chats(forged).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
            );
        }
        let DesktopChatsReply::List(first_data) = first.data else {
            unreachable!()
        };
        assert!(
            matches!(repository.chats(list(first_data.next)).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
        );
        let second = repository.continue_chats(continuation).await.unwrap();
        assert!(second.next.is_none());
        let DesktopChatsReply::List(second) = second.data else {
            unreachable!()
        };
        assert_eq!(second.items[0].rowid, i64::MIN.to_string());

        let first = repository.chats(messages(None)).await.unwrap();
        let next = first.next.unwrap();
        for fault in 0..4 {
            let mut value = serde_json::to_value(&next).unwrap();
            let key = ["generation", "messages_revision", "query_hash", "chat_id"][fault];
            value["request"]["data"]["cursor"][key] = serde_json::json!(if fault == 2 {
                "d".repeat(64)
            } else {
                "changed".into()
            });
            assert!(
                matches!(repository.continue_chats(serde_json::from_value(value).unwrap()).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
            );
        }
        let second = repository.continue_chats(next).await.unwrap();
        assert!(second.next.is_none());
        let DesktopChatsReply::Messages(second) = second.data else {
            unreachable!()
        };
        assert_eq!(second.items[0].rowid, i64::MIN.to_string());

        let first = repository
            .chats(chat_field(DesktopChatField::Title))
            .await
            .unwrap();
        let next = first.next.unwrap();
        assert!(!format!("{next:?}").contains('🦀'));
        for fault in 0..5 {
            let mut value = serde_json::to_value(&next).unwrap();
            match fault {
                0 => {
                    // 501 is in the middle of the next UTF-8 crab. Alter both
                    // raw cursors; the actual prior fragment boundary still wins.
                    value["request"]["data"]["cursor"]["byte_offset"] = serde_json::json!(501);
                    value["boundary"]["data"]["cursor"]["byte_offset"] = serde_json::json!(501);
                }
                1 => {
                    value["request"]["data"]["cursor"]["total_bytes"] =
                        serde_json::json!(9_000_000);
                    value["boundary"]["data"]["cursor"]["total_bytes"] =
                        serde_json::json!(9_000_000);
                }
                2 => {
                    value["request"]["data"]["cursor"]["present"] = serde_json::json!(false);
                    value["boundary"]["data"]["cursor"]["present"] = serde_json::json!(false);
                }
                3 => value["initial"]["data"]["field"] = serde_json::json!("scope_kind"),
                4 => value["boundary"]["data"]["fragment_bytes"] = serde_json::json!(0),
                _ => unreachable!(),
            }
            assert!(
                matches!(repository.continue_chats(serde_json::from_value(value).unwrap()).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
            );
        }
        let second = repository.continue_chats(next).await.unwrap();
        assert!(
            second.next.is_some(),
            "never collect the remaining giant field"
        );
        let DesktopChatsReply::ChatField(page) = second.data else {
            unreachable!()
        };
        assert_eq!(page.byte_offset, 500);
        assert_eq!(page.text, "🦀\0".repeat(100));

        let first = repository
            .chats(message_field(DesktopChatMessageField::CitationsStoredJson))
            .await
            .unwrap();
        let next = first.next.unwrap();
        let mut invalid = serde_json::to_value(&next).unwrap();
        invalid["request"]["data"]["cursor"]["total_bytes"] = serde_json::json!(9_000_000);
        invalid["boundary"]["data"]["cursor"]["total_bytes"] = serde_json::json!(9_000_000);
        assert!(
            matches!(repository.continue_chats(serde_json::from_value(invalid).unwrap()).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        let second = repository.continue_chats(next).await.unwrap();
        assert!(second.next.is_some());
        let DesktopChatsReply::MessageField(page) = second.data else {
            unreachable!()
        };
        assert_eq!(page.byte_offset, 720);
        assert_eq!(page.text, "invalid\0{".repeat(80));
        assert_eq!(second.identity, identity);
        assert!(repository.chats(describe()).await.unwrap().next.is_none());
        server.finish().await;
    });
}

#[test]
fn invalid_caller_shapes_send_no_wire_and_keep_connection_usable() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(
                matches!(&request.request, Request::DesktopChats(actual) if **actual==describe())
            );
            send(
                &mut stream,
                &request,
                envelope(&d, DesktopChatsReply::Header(header())),
            )
            .await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let mut invalid = Vec::new();
        for (key, value) in [
            ("limit", serde_json::json!(0)),
            ("limit", serde_json::json!(101)),
            ("max_bytes", serde_json::json!(1023)),
            ("max_bytes", serde_json::json!(65_025)),
        ] {
            let mut encoded = serde_json::to_value(list(None)).unwrap();
            encoded["data"][key] = value;
            invalid.push(serde_json::from_value(encoded).unwrap());
        }
        invalid.push(DesktopChatsRequest::List {
            scope: DesktopChatScope::Exact {
                kind: "\0".repeat(11_000),
                scope_id: None,
            },
            limit: 1,
            max_bytes: 4096,
            cursor: None,
        });
        invalid.push(DesktopChatsRequest::Describe {
            chat_id: String::new(),
        });
        let mut request = messages(None);
        let DesktopChatsRequest::Messages {
            expected_generation,
            ..
        } = &mut request
        else {
            unreachable!()
        };
        expected_generation.clear();
        invalid.push(request);
        let DesktopChatsReply::List(page) = list_page(true) else {
            unreachable!()
        };
        let mut cursor = page.next.unwrap();
        cursor.after_rowid = "-0".into();
        invalid.push(list(Some(cursor)));
        let mut request = chat_field(DesktopChatField::Title);
        let DesktopChatsRequest::ReadChatField {
            expected_metadata_revision,
            ..
        } = &mut request
        else {
            unreachable!()
        };
        expected_metadata_revision.clear();
        invalid.push(request);
        let mut request = message_field(DesktopChatMessageField::Content);
        let DesktopChatsRequest::ReadMessageField {
            expected_revision, ..
        } = &mut request
        else {
            unreachable!()
        };
        expected_revision.clear();
        invalid.push(request);
        for request in invalid {
            assert!(!request.valid());
            assert!(
                matches!(client.desktop_chats(request).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
            );
        }
        assert!(matches!(
            client.desktop_chats(describe()).await.unwrap().data,
            DesktopChatsReply::Header(_)
        ));
        server.finish().await;
    });
}

async fn rejects_and_poisons(request: DesktopChatsRequest, data: DesktopChatsReply, fault: u8) {
    assert!(request.valid());
    let expected = request.clone();
    let mut server = fake(move |mut stream, d| async move {
        let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
        assert!(matches!(&request.request, Request::DesktopChats(actual) if **actual==expected));
        let mut reply = envelope(&d, data);
        match fault {
            0 => {}
            1 => {
                let Reply::DesktopChats(value) = &mut reply else {
                    unreachable!()
                };
                value.library_id = Uuid::new_v4();
            }
            2 => {
                let Reply::DesktopChats(value) = &mut reply else {
                    unreachable!()
                };
                value.library_epoch = Uuid::new_v4();
            }
            3 => reply = Reply::ShutdownAccepted,
            _ => unreachable!(),
        }
        send(&mut stream, &request, reply).await;
        let closed: Result<RequestEnvelope, _> = read_frame(&mut stream).await;
        assert!(closed.is_err(), "invalid reply must poison, not retry");
    });
    let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
    assert!(matches!(
        client.desktop_chats(request).await,
        Err(ClientError::Protocol)
    ));
    assert!(matches!(client.status().await, Err(ClientError::Protocol)));
    server.finish().await;
}

#[test]
fn forged_chat_pages_headers_envelopes_and_coarse_reply_kinds_poison_the_stream() {
    run(async {
        for fault in 1..=3 {
            rejects_and_poisons(describe(), DesktopChatsReply::Header(header()), fault).await;
        }
        rejects_and_poisons(list(None), DesktopChatsReply::Header(header()), 0).await;
        for fault in 0..13 {
            let mut reply = list_page(true);
            let DesktopChatsReply::List(page) = &mut reply else {
                unreachable!()
            };
            match fault {
                0 => page.library_scope = "bad".into(),
                1 => page.catalog_revision.clear(),
                2 => page.items.push(page.items[0].clone()),
                3 => {
                    let mut duplicate = page.items[0].clone();
                    duplicate.id = "different-id-same-row".into();
                    page.items.push(duplicate);
                }
                4 => page.items[0].title_preview = "x".repeat(257),
                5 => page.items[0].title_bytes = 0,
                6 => page.items[0].rowid = "01".into(),
                7 => page.next.as_mut().unwrap().after_rowid = "-7".into(),
                8 => page.next.as_mut().unwrap().catalog_revision = "other".into(),
                9 => page.next.as_mut().unwrap().query_hash = "?".repeat(64),
                10 => page.next.as_mut().unwrap().library_scope = "d".repeat(32),
                11 => page.items.clear(),
                12 => page.items[0].scope_id_bytes = Some(0),
                _ => unreachable!(),
            }
            assert!(!reply.valid_for(&list(None)));
            rejects_and_poisons(list(None), reply, 0).await;
        }
        let mut reply = list_page(false);
        let DesktopChatsReply::List(page) = &mut reply else {
            unreachable!()
        };
        page.items[0].title_preview = "\0".repeat(256);
        page.items[0].title_bytes = 256;
        let mut request = list(None);
        let DesktopChatsRequest::List { max_bytes, .. } = &mut request else {
            unreachable!()
        };
        *max_bytes = 1024;
        assert!(!reply.valid_for(&request));
        rejects_and_poisons(request, reply, 0).await;
        let DesktopChatsReply::List(first) = list_page(true) else {
            unreachable!()
        };
        let continued = list(first.next);
        for fault in 0..3 {
            let mut reply = list_page(fault == 0);
            let DesktopChatsReply::List(page) = &mut reply else {
                unreachable!()
            };
            match fault {
                0 => {
                    page.items[0].rowid = "-7".into();
                    page.next.as_mut().unwrap().after_rowid = "-7".into();
                    page.next.as_mut().unwrap().query_hash = "d".repeat(64);
                }
                1 => page.items.clear(),
                // Same-row terminal pages are also a stall, even without next.
                2 => {}
                _ => unreachable!(),
            }
            assert!(!reply.valid_for(&continued));
            rejects_and_poisons(continued.clone(), reply, 0).await;
        }
    });
}

#[test]
fn forged_message_page_pins_targets_rows_and_previews_poison_the_stream() {
    run(async {
        for fault in 0..12 {
            let mut reply = messages_page(true);
            let DesktopChatsReply::Messages(page) = &mut reply else {
                unreachable!()
            };
            match fault {
                0 => page.chat_id = "other".into(),
                1 => page.generation = "other".into(),
                2 => page.messages_revision = "other".into(),
                3 => page.items[0].chat_id = "other".into(),
                4 => page.items[0].role_preview = "x".repeat(129),
                5 => page.items[0].role_bytes = 0,
                6 => page.items.push(page.items[0].clone()),
                7 => page.next.as_mut().unwrap().after_rowid = "-7".into(),
                8 => page.next.as_mut().unwrap().generation = "other".into(),
                9 => page.next.as_mut().unwrap().messages_revision = "other".into(),
                10 => page.next.as_mut().unwrap().query_hash = "?".repeat(64),
                11 => page.items.clear(),
                _ => unreachable!(),
            }
            assert!(!reply.valid_for(&messages(None)));
            rejects_and_poisons(messages(None), reply, 0).await;
        }
        let DesktopChatsReply::Messages(first) = messages_page(true) else {
            unreachable!()
        };
        let request = messages(first.next);
        let mut reply = messages_page(true);
        let DesktopChatsReply::Messages(page) = &mut reply else {
            unreachable!()
        };
        page.items[0].rowid = "-7".into();
        page.next.as_mut().unwrap().after_rowid = "-7".into();
        page.next.as_mut().unwrap().query_hash = "d".repeat(64);
        assert!(!reply.valid_for(&request));
        rejects_and_poisons(request, reply, 0).await;
        let DesktopChatsReply::Messages(first) = messages_page(true) else {
            unreachable!()
        };
        let request = messages(first.next);
        let mut reply = messages_page(false);
        let DesktopChatsReply::Messages(page) = &mut reply else {
            unreachable!()
        };
        page.items.clear();
        assert!(!reply.valid_for(&request));
        rejects_and_poisons(request, reply, 0).await;
    });
}

#[test]
fn forged_fragment_byte_progress_and_json_escaping_budget_poison_the_stream() {
    run(async {
        let chat_request = chat_field(DesktopChatField::Title);
        for fault in 0..19 {
            let mut reply = chat_page(&chat_request, "🦀\0", 20, true);
            let DesktopChatsReply::ChatField(page) = &mut reply else {
                unreachable!()
            };
            match fault {
                0 => page.chat_id = "other".into(),
                1 => page.generation = "other".into(),
                2 => page.metadata_revision = "other".into(),
                3 => page.field = DesktopChatField::ScopeKind,
                4 => page.byte_offset = 1,
                5 => page.total_bytes = 3,
                6 => page.text.clear(),
                7 => page.present = false,
                8 => page.next = None,
                9 => page.next.as_mut().unwrap().byte_offset += 1,
                10 => page.next.as_mut().unwrap().total_bytes += 1,
                11 => page.next.as_mut().unwrap().present = false,
                12 => page.next.as_mut().unwrap().max_bytes = 2048,
                13 => page.next.as_mut().unwrap().library_scope = "d".repeat(32),
                14 => page.next.as_mut().unwrap().generation = "other".into(),
                15 => page.next.as_mut().unwrap().metadata_revision = "other".into(),
                16 => page.next.as_mut().unwrap().chat_id = "other".into(),
                17 => page.total_bytes = 9_007_199_254_740_992,
                18 => {
                    page.text = "\0".repeat(800);
                    page.total_bytes = 800;
                    page.next = None;
                }
                _ => unreachable!(),
            }
            assert!(!reply.valid_for(&chat_request));
            rejects_and_poisons(chat_request.clone(), reply, 0).await;
        }
        let message_request = message_field(DesktopChatMessageField::CitationsStoredJson);
        for fault in 0..18 {
            let mut reply = message_page(&message_request, "raw\0{", 20);
            let DesktopChatsReply::MessageField(page) = &mut reply else {
                unreachable!()
            };
            match fault {
                0 => page.chat_id = "other".into(),
                1 => page.message_id = "other".into(),
                2 => page.chat_generation = "other".into(),
                3 => page.generation = "other".into(),
                4 => page.revision = "other".into(),
                5 => page.field = DesktopChatMessageField::Content,
                6 => page.byte_offset = 1,
                7 => page.total_bytes = 3,
                8 => page.text.clear(),
                9 => page.next = None,
                10 => page.next.as_mut().unwrap().byte_offset += 1,
                11 => page.next.as_mut().unwrap().total_bytes += 1,
                12 => page.next.as_mut().unwrap().max_bytes = 2048,
                13 => page.next.as_mut().unwrap().library_scope = "d".repeat(32),
                14 => page.next.as_mut().unwrap().chat_generation = "other".into(),
                15 => page.next.as_mut().unwrap().revision = "other".into(),
                16 => page.next.as_mut().unwrap().message_id = "other".into(),
                17 => {
                    page.text = "\0".repeat(800);
                    page.total_bytes = 800;
                    page.next = None;
                }
                _ => unreachable!(),
            }
            assert!(!reply.valid_for(&message_request));
            rejects_and_poisons(message_request.clone(), reply, 0).await;
        }
    });
}

#[test]
fn facade_rejects_foreign_persistent_scope_and_discards_the_connection() {
    run(async {
        for field_read in [false, true] {
            let request = if field_read {
                message_field(DesktopChatMessageField::Content)
            } else {
                list(None)
            };
            let expected = request.clone();
            let mut server = fake(move |mut stream, d| async move {
                bootstrap(&mut stream, &d).await;
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(
                    matches!(&request.request, Request::DesktopChats(actual) if **actual==expected)
                );
                let mut reply = if field_read {
                    message_page(&expected, "raw", 3)
                } else {
                    list_page(false)
                };
                match &mut reply {
                    DesktopChatsReply::List(page) => page.library_scope = "d".repeat(32),
                    DesktopChatsReply::MessageField(page) => page.library_scope = "d".repeat(32),
                    _ => unreachable!(),
                }
                assert!(
                    reply.valid_for(&expected),
                    "the facade alone knows persistent bootstrap scope"
                );
                send(&mut stream, &request, envelope(&d, reply)).await;
                let closed: Result<RequestEnvelope, _> = read_frame(&mut stream).await;
                assert!(closed.is_err());
            });
            let repository = RepositoryClient::connect(server.root.path()).await.unwrap();
            assert!(
                matches!(repository.chats(request).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
            );
            assert!(matches!(
                repository.chats(describe()).await,
                Err(ClientError::Protocol)
            ));
            server.finish().await;
        }
    });
}

#[test]
fn stale_read_error_has_no_implicit_refetch_rebase_or_retry() {
    run(async {
        let original = message_field(DesktopChatMessageField::CitationsStoredJson);
        let expected = original.clone();
        let mut server = fake(move |mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            for attempt in 0..2 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(
                    matches!(&request.request, Request::DesktopChats(actual) if **actual==expected)
                );
                let reply = if attempt == 0 {
                    Reply::Error(RpcError::new(ErrorCode::ResyncRequired))
                } else {
                    envelope(&d, message_page(&expected, "raw-invalid\0{", 13))
                };
                send(&mut stream, &request, reply).await;
            }
        });
        let repository = RepositoryClient::connect(server.root.path()).await.unwrap();
        assert!(
            matches!(repository.chats(original.clone()).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        let page = repository.chats(original).await.unwrap();
        let DesktopChatsReply::MessageField(page) = page.data else {
            unreachable!()
        };
        assert_eq!(page.text, "raw-invalid\0{");
        assert_eq!(page.revision, "message-1");
        server.finish().await;
    });
}

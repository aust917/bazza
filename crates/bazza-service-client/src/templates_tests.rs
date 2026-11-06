#![cfg(any(windows, target_os = "macos"))]
use crate::*;
use bazza_service_api::{desktop::*, templates::*};
use bazza_service_transport::Listener;
use repository::RepositoryTemplateReadRequest;
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
                .expect("bounded template fake peer");
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
fn envelope(d: &EndpointDescriptor, data: DesktopTemplatesReply) -> Reply {
    Reply::DesktopTemplates(Box::new(DesktopEnvelope {
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
                    FIXTURE_TEMPLATES_CAPABILITY.into(),
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
fn header() -> TemplateHeader {
    TemplateHeader {
        id: "SQL / шаблон".into(),
        revision: "template-1".into(),
        generation: "generation-1".into(),
        state: TemplateDocumentState::Present,
        builtin: false,
        name_preview: "Name".into(),
        description_preview: String::new(),
        name_bytes: 4,
        description_bytes: 0,
        body_bytes: 6,
    }
}
fn field(first: bool) -> TemplateFieldPage {
    let h = header();
    TemplateFieldPage {
        library_scope: "a".repeat(32),
        header: h.clone(),
        field: TemplateField::Body,
        text: if first { "я\0" } else { "尾" }.into(),
        byte_offset: if first { 0 } else { 3 },
        total_bytes: 6,
        next: first.then(|| TemplateReadCursor {
            library_scope: "a".repeat(32),
            template_id: h.id,
            revision: h.revision,
            generation: h.generation,
            field: TemplateField::Body,
            byte_offset: 3,
            max_bytes: 2048,
        }),
    }
}

#[test]
fn wrong_operation_identity_budget_and_cursor_progress_poison_the_connection() {
    run(async {
        for mode in 0..5 {
            let mut peer = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(request.request, Request::DesktopTemplates(_)));
                let data = match mode {
                    0 => {
                        let mut h = header();
                        h.id = "another SQL identity".into();
                        DesktopTemplatesReply::Describe(h)
                    }
                    2 => {
                        let mut p = field(false);
                        p.text = "\0".repeat(2000);
                        p.byte_offset = 0;
                        p.total_bytes = 2000;
                        p.header.body_bytes = 2000;
                        DesktopTemplatesReply::ReadField(p)
                    }
                    3 => DesktopTemplatesReply::List(TemplateListPage {
                        library_scope: "a".repeat(32),
                        catalog_revision: "catalog-1".into(),
                        items: vec![header()],
                        next: Some(TemplateListCursor {
                            library_scope: "a".repeat(32),
                            catalog_revision: "catalog-1".into(),
                            query_hash: "b".repeat(64),
                            offset: 2,
                        }),
                    }),
                    4 => DesktopTemplatesReply::List(TemplateListPage {
                        library_scope: "a".repeat(32),
                        catalog_revision: "catalog-1".into(),
                        items: vec![],
                        next: None,
                    }),
                    _ => DesktopTemplatesReply::Describe(header()),
                };
                let mut reply = envelope(&d, data);
                if mode == 1 {
                    if let Reply::DesktopTemplates(value) = &mut reply {
                        value.library_id = Uuid::new_v4();
                    }
                }
                send(&mut stream, &request, reply).await;
            });
            let mut client = ServiceClient::connect(peer.root.path()).await.unwrap();
            let request = match mode {
                2 => DesktopTemplatesRequest::ReadField(TemplateReadRequest {
                    template_id: header().id,
                    expected_revision: header().revision,
                    field: TemplateField::Body,
                    cursor: None,
                    max_bytes: 2048,
                }),
                3 => DesktopTemplatesRequest::List(TemplateListRequest {
                    cursor: None,
                    limit: 1,
                    max_bytes: 2048,
                }),
                _ => DesktopTemplatesRequest::Describe {
                    template_id: header().id,
                },
            };
            assert!(matches!(
                client.desktop_templates(request).await,
                Err(ClientError::Protocol)
            ));
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            peer.finish().await;
        }
    });
}

#[test]
fn field_continuations_are_explicit_and_bind_facade_library_epoch_and_scope() {
    run(async {
        let mut peer = fake(|mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            for first in [true, false] {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::DesktopTemplates(r) = &request.request else {
                    panic!("templates request");
                };
                let DesktopTemplatesRequest::ReadField(r) = r.as_ref() else {
                    panic!("field request");
                };
                assert_eq!(r.template_id, header().id);
                assert_eq!(r.expected_revision, header().revision);
                assert_eq!(
                    r.cursor.as_ref().map(|cursor| cursor.byte_offset),
                    (!first).then_some(3)
                );
                send(
                    &mut stream,
                    &request,
                    envelope(&d, DesktopTemplatesReply::ReadField(field(first))),
                )
                .await;
            }
        });
        let repository = RepositoryClient::connect(peer.root.path()).await.unwrap();
        let request = RepositoryTemplateReadRequest {
            template_id: header().id,
            expected_revision: header().revision,
            field: TemplateField::Body,
            cursor: None,
            max_bytes: 2048,
        };
        let first = repository.template_field(request.clone()).await.unwrap();
        assert_eq!(first.text, "я\0");
        assert_eq!(first.identity, repository.identity().await);
        let cursor = first.next.unwrap();
        for mode in 0..3 {
            let mut invalid = cursor.clone();
            match mode {
                0 => invalid.library_id = Uuid::new_v4(),
                1 => invalid.library_epoch = Uuid::new_v4(),
                _ => invalid.cursor.library_scope = "f".repeat(32),
            }
            let mut r = request.clone();
            r.cursor = Some(invalid);
            assert!(
                matches!(repository.template_field(r).await, Err(ClientError::Remote(error)) if error.code == ErrorCode::ResyncRequired)
            );
        }
        let mut next = request;
        next.cursor = Some(cursor);
        let last = repository.template_field(next).await.unwrap();
        assert_eq!(last.text, "尾");
        assert_eq!(last.byte_offset, 3);
        assert!(last.next.is_none());
        peer.finish().await;
    });
}

#[test]
fn invalid_local_request_preserves_stream_and_commit_conflict_is_not_retried() {
    run(async {
        let upload_id = Uuid::new_v4();
        let mut peer = fake(move |mut stream, d| async move {
            bootstrap(&mut stream, &d).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::DesktopTemplates(r) = &request.request else {
                panic!("templates request");
            };
            let DesktopTemplatesRequest::CommitWrite(r) = r.as_ref() else {
                panic!("commit request");
            };
            assert_eq!(r.upload_id, upload_id);
            assert_eq!(r.idempotency_key, "original-commit-key");
            send(
                &mut stream,
                &request,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(
                matches!(&request.request, Request::DesktopTemplates(r) if matches!(r.as_ref(), DesktopTemplatesRequest::Describe { .. }))
            );
            send(
                &mut stream,
                &request,
                envelope(&d, DesktopTemplatesReply::Describe(header())),
            )
            .await;
        });
        let repository = RepositoryClient::connect(peer.root.path()).await.unwrap();
        assert!(
            matches!(repository.template_describe(String::new()).await, Err(ClientError::Remote(error)) if error.code == ErrorCode::InvalidArgument)
        );
        assert!(
            matches!(repository.template_write_commit(TemplateUploadRequest { upload_id, idempotency_key: "original-commit-key".into() }).await, Err(ClientError::Remote(error)) if error.code == ErrorCode::Conflict)
        );
        let described = repository.template_describe(header().id).await.unwrap();
        assert_eq!(described.identity, repository.identity().await);
        let DesktopTemplatesReply::Describe(h) = described.data else {
            panic!("header");
        };
        assert_eq!(h, header());
        peer.finish().await;
    });
}

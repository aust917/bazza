#![cfg(any(windows, target_os = "macos"))]
#[path = "configuration_tests.rs"]
mod configuration_tests;

use crate::*;
use bazza_service_api::desktop::*;
use bazza_service_transport::Listener;
use std::future::Future;
use uuid::Uuid;

fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(4), future)
                .await
                .expect("bounded private desktop fake-peer test");
        });
}
fn descriptor(root: &Path) -> EndpointDescriptor {
    let library_epoch = Uuid::new_v4();
    EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: ApiVersion::CURRENT,
        process_id: std::process::id(),
        library_id: Uuid::new_v4(),
        library_epoch,
        build_id: build_id().into(),
        endpoint: session_endpoint_name(root, library_epoch.as_bytes()).unwrap(),
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
        capabilities: vec![
            FIXTURE_STATUS_CAPABILITY.into(),
            FIXTURE_METADATA_CAPABILITY.into(),
            FIXTURE_DESKTOP_CAPABILITY.into(),
        ],
        development_fixture_only: true,
    }
}
async fn hello(stream: &mut PeerStream, d: &EndpointDescriptor) {
    let request: RequestEnvelope = read_frame(stream).await.unwrap();
    assert!(matches!(request.request, Request::Hello(_)));
    send(stream, &request, Reply::Hello(status(d))).await;
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
fn preferences() -> DesktopPreferences {
    DesktopPreferences {
        revision: "preferences-1".into(),
        ui_locale: DesktopLocale::Ru,
        close_to_tray: true,
        hotkey: "CmdOrCtrl+Shift+R".into(),
    }
}
fn bootstrap() -> DesktopBootstrap {
    DesktopBootstrap {
        preferences: preferences(),
        change_sequence: 0,
        cursor_scope: "a".repeat(32),
    }
}
fn meeting() -> DesktopMeeting {
    DesktopMeeting {
        id: "legacy-card".into(),
        title: "Original".into(),
        app: None,
        started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        ended_at: None,
        duration_ms: 0,
        has_video: false,
        status: "ready".into(),
        folder_id: None,
        created_at: "2026-11-02T00:00:00Z".parse().unwrap(),
        error: None,
        error_truncated: false,
        title_auto: false,
        participants: vec![],
        calendar_uid: None,
        content_kind: "text".into(),
        metadata_revision: "metadata-1".into(),
        lifecycle_revision: "lifecycle-1".into(),
        transcript_revision: "transcript-1".into(),
        transcript_state: "absent".into(),
        summary_revision: "summary-1".into(),
        summary_state: "absent".into(),
        media: DesktopMedia {
            audio: false,
            video: false,
        },
    }
}
fn mutation(entity: &str, revision: &str, sequence: i64) -> DesktopMutationOutcome {
    DesktopMutationOutcome {
        entity_id: entity.into(),
        revision: revision.into(),
        change_sequence: sequence,
        job_id: None,
        dedup_until: "2099-01-01T00:00:00Z".parse().unwrap(),
    }
}
fn update(key: &str) -> UpdateDesktopPreferencesRequest {
    UpdateDesktopPreferencesRequest {
        expected_revision: "preferences-1".into(),
        idempotency_key: key.into(),
        patch: DesktopPreferencesPatch {
            ui_locale: Some(DesktopLocale::En),
            ..Default::default()
        },
    }
}
fn page() -> DesktopMeetingPage {
    DesktopMeetingPage {
        items: vec![meeting()],
        next: None,
        snapshot_sequence: 0,
    }
}
fn change_page() -> DesktopChangePage {
    DesktopChangePage {
        items: vec![DesktopChange {
            sequence: 1,
            entity_kind: "desktop_preferences".into(),
            entity_id: "desktop_preferences".into(),
            revision: "preferences-2".into(),
            actor: "desktop".into(),
        }],
        next: DesktopChangeCursor {
            library_scope: "a".repeat(32),
            after: 1,
        },
        has_more: false,
        current_sequence: 1,
    }
}

#[test]
fn all_desktop_methods_preserve_typed_revisions_and_reply_identity() {
    run(async {
        let root = tempfile::tempdir().unwrap();
        let d = descriptor(root.path());
        publish_endpoint(root.path(), &d).unwrap();
        let mut listener = Listener::bind(&d.endpoint).unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            hello(&mut stream, &d).await;
            for step in 0..7 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let reply = match (&request.request, step) {
                    (Request::DesktopBootstrap, 0) => {
                        Reply::DesktopBootstrap(envelope(&d, bootstrap()))
                    }
                    (Request::DesktopPreferences, 1) => {
                        Reply::DesktopPreferences(envelope(&d, preferences()))
                    }
                    (Request::UpdateDesktopPreferences(input), 2) => {
                        assert_eq!(input.expected_revision, "preferences-1");
                        Reply::DesktopMutation(envelope(
                            &d,
                            mutation("desktop_preferences", "preferences-2", 1),
                        ))
                    }
                    (Request::DesktopMeetings(_), 3) => {
                        Reply::DesktopMeetings(envelope(&d, page()))
                    }
                    (Request::DesktopMeeting(input), 4) => {
                        assert_eq!(input.meeting_id, "legacy-card");
                        Reply::DesktopMeeting(Box::new(envelope(&d, Some(meeting()))))
                    }
                    (Request::DesktopChanges(_), 5) => {
                        Reply::DesktopChanges(envelope(&d, change_page()))
                    }
                    (Request::RenameDesktopMeeting(input), 6) => {
                        assert_eq!(input.expected_revision, "metadata-1");
                        Reply::DesktopMutation(envelope(
                            &d,
                            mutation("legacy-card", "metadata-2", 2),
                        ))
                    }
                    _ => panic!("unexpected private desktop request"),
                };
                send(&mut stream, &request, reply).await;
            }
        });
        let mut client = ServiceClient::connect(root.path()).await.unwrap();
        assert_eq!(
            client
                .desktop_bootstrap()
                .await
                .unwrap()
                .data
                .preferences
                .ui_locale,
            DesktopLocale::Ru
        );
        assert_eq!(
            client.desktop_preferences().await.unwrap().data.revision,
            "preferences-1"
        );
        assert_eq!(
            client
                .update_desktop_preferences(update("update"))
                .await
                .unwrap()
                .data
                .revision,
            "preferences-2"
        );
        assert_eq!(
            client
                .desktop_meetings(Default::default())
                .await
                .unwrap()
                .data
                .items
                .len(),
            1
        );
        assert_eq!(
            client
                .desktop_meeting(DesktopMeetingLookup {
                    meeting_id: "legacy-card".into()
                })
                .await
                .unwrap()
                .data
                .unwrap()
                .metadata_revision,
            "metadata-1"
        );
        assert_eq!(
            client
                .desktop_changes(Default::default())
                .await
                .unwrap()
                .data
                .items[0]
                .sequence,
            1
        );
        assert_eq!(
            client
                .rename_desktop_meeting(RenameDesktopMeetingRequest {
                    meeting_id: "legacy-card".into(),
                    expected_revision: "metadata-1".into(),
                    idempotency_key: "rename".into(),
                    title: "Renamed".into()
                })
                .await
                .unwrap()
                .data
                .revision,
            "metadata-2"
        );
        task.await.unwrap();
    });
}

#[test]
fn malformed_desktop_identity_budget_count_cursor_and_change_gap_poison_the_stream() {
    run(async {
        for variant in 0..6 {
            let root = tempfile::tempdir().unwrap();
            let d = descriptor(root.path());
            publish_endpoint(root.path(), &d).unwrap();
            let mut listener = Listener::bind(&d.endpoint).unwrap();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                hello(&mut stream, &d).await;
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let reply = match variant {
                    0..=2 => {
                        let mut value = envelope(&d, bootstrap());
                        match variant {
                            0 => value.library_id = Uuid::new_v4(),
                            1 => value.library_epoch = Uuid::new_v4(),
                            _ => value.data.preferences.hotkey = "x".repeat(70_000),
                        }
                        Reply::DesktopBootstrap(value)
                    }
                    3 => {
                        let mut value = page();
                        value.items.push(meeting());
                        Reply::DesktopMeetings(envelope(&d, value))
                    }
                    4 => {
                        let mut value = page();
                        value.next = Some(DesktopCursor {
                            library_scope: "a".repeat(32),
                            snapshot_sequence: 0,
                            query_hash: "b".repeat(64),
                            offset: 2,
                        });
                        Reply::DesktopMeetings(envelope(&d, value))
                    }
                    _ => {
                        let mut value = change_page();
                        value.items[0].sequence = 2;
                        value.next.after = 2;
                        value.current_sequence = 2;
                        Reply::DesktopChanges(envelope(&d, value))
                    }
                };
                send(&mut stream, &request, reply).await;
            });
            let mut client = ServiceClient::connect(root.path()).await.unwrap();
            let result = match variant {
                0..=2 => client.desktop_bootstrap().await.map(|_| ()),
                3 | 4 => client
                    .desktop_meetings(DesktopMeetingRequest {
                        limit: 1,
                        ..Default::default()
                    })
                    .await
                    .map(|_| ()),
                _ => client.desktop_changes(Default::default()).await.map(|_| ()),
            };
            assert!(
                matches!(result, Err(ClientError::Protocol)),
                "variant {variant}"
            );
            assert!(client.stream.is_none());
            assert!(matches!(client.status().await, Err(ClientError::Protocol)));
            task.await.unwrap();
        }
    });
}

#[test]
fn cloned_repository_calls_keep_caller_revisions_and_never_retry_conflicts() {
    run(async {
        let root = tempfile::tempdir().unwrap();
        let d = descriptor(root.path());
        publish_endpoint(root.path(), &d).unwrap();
        let mut listener = Listener::bind(&d.endpoint).unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            hello(&mut stream, &d).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::DesktopBootstrap));
            send(
                &mut stream,
                &request,
                Reply::DesktopBootstrap(envelope(&d, bootstrap())),
            )
            .await;
            for step in 0..2 {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::UpdateDesktopPreferences(input) = &request.request else {
                    panic!("client must not fetch a new revision or invent a retry")
                };
                assert_eq!(input.expected_revision, "preferences-1");
                let reply = if step == 0 {
                    Reply::DesktopMutation(envelope(
                        &d,
                        mutation("desktop_preferences", "preferences-2", 1),
                    ))
                } else {
                    Reply::Error(RpcError::new(ErrorCode::Conflict))
                };
                send(&mut stream, &request, reply).await;
            }
        });
        let repository = RepositoryClient::connect(root.path()).await.unwrap();
        let expected_identity = repository.identity().await;
        let other = repository.clone();
        let (a, b) = tokio::join!(
            repository.update_preferences(update("writer-a")),
            other.update_preferences(update("writer-b"))
        );
        assert_eq!([&a, &b].iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!([&a,&b].iter().filter(|r|matches!(r,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)).count(),1);
        let committed = a.as_ref().ok().or_else(|| b.as_ref().ok()).unwrap();
        assert_eq!(committed.identity, expected_identity);
        assert_eq!(committed.data.revision, "preferences-2");
        task.await.unwrap();
    });
}

#[test]
fn explicit_reconnect_binds_library_and_epoch_and_old_cursors_do_not_reach_the_wire() {
    run(async {
        let root = tempfile::tempdir().unwrap();
        let initial = descriptor(root.path());
        let mut next = initial.clone();
        next.library_epoch = Uuid::new_v4();
        next.endpoint = session_endpoint_name(root.path(), next.library_epoch.as_bytes()).unwrap();
        let mut foreign = next.clone();
        foreign.library_id = Uuid::new_v4();
        foreign.library_epoch = Uuid::new_v4();
        foreign.endpoint =
            session_endpoint_name(root.path(), foreign.library_epoch.as_bytes()).unwrap();
        publish_endpoint(root.path(), &initial).unwrap();
        let mut listener = Listener::bind(&initial.endpoint).unwrap();
        let all = [initial.clone(), next.clone(), foreign.clone()];
        let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(async move {
            for (index, d) in all.iter().enumerate() {
                let (mut stream, _) = listener.accept().await.unwrap();
                hello(&mut stream, d).await;
                if index == 2 {
                    continue;
                }
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(request.request, Request::DesktopBootstrap));
                send(
                    &mut stream,
                    &request,
                    Reply::DesktopBootstrap(envelope(d, bootstrap())),
                )
                .await;
                if index == 0 {
                    let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                    assert!(matches!(request.request, Request::DesktopMeetings(_)));
                    let mut value = page();
                    value.next = Some(DesktopCursor {
                        library_scope: "a".repeat(32),
                        snapshot_sequence: 0,
                        query_hash: "b".repeat(64),
                        offset: 1,
                    });
                    send(
                        &mut stream,
                        &request,
                        Reply::DesktopMeetings(envelope(d, value)),
                    )
                    .await;
                }
                drop(stream);
                drop(listener);
                listener = Listener::bind(&all[index + 1].endpoint).unwrap();
                ready_tx.send(()).await.unwrap();
            }
        });
        let repository = RepositoryClient::connect(root.path()).await.unwrap();
        let page = repository
            .meetings(Default::default(), None, 1, MAX_DESKTOP_DATA_BYTES)
            .await
            .unwrap();
        let cursor = page.next.unwrap();
        ready_rx.recv().await.unwrap();
        publish_endpoint(root.path(), &next).unwrap();
        let identity = repository.reconnect().await.unwrap();
        assert_eq!(identity.library_epoch, next.library_epoch);
        assert!(
            matches!(repository.meetings(Default::default(),Some(cursor),1,MAX_DESKTOP_DATA_BYTES).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        ready_rx.recv().await.unwrap();
        publish_endpoint(root.path(), &foreign).unwrap();
        assert!(
            matches!(repository.reconnect().await,Err(ClientError::Remote(error)) if error.code==ErrorCode::ResyncRequired)
        );
        assert!(matches!(
            repository.preferences().await,
            Err(ClientError::Protocol)
        ));
        task.await.unwrap();
    });
}

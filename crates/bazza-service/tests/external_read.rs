//! OS-verified local IPC on an isolated fixture. No installed app, real
//! library, credentials file, or public permission is involved.
#![cfg(any(windows, target_os = "macos"))]

use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::{
    external_read::{
        ExternalHello, ExternalReadGrantIssued, ExternalReply, ExternalReplyEnvelope,
        ExternalRequest, ExternalRequestEnvelope, IssueExternalReadGrant, ListExternalReadGrants,
        RevokeExternalReadGrant,
    },
    fixture_read::{FixtureReadRequest, FixtureReadTool},
    rpc::{
        unix_ms, ApiVersion, ClientHello, EndpointDescriptor, ErrorCode, Reply, ReplyEnvelope,
        Request, RequestEnvelope, STORAGE_SCHEMA_MAX,
    },
    worker::WorkerSecret,
};
use bazza_service_client::{read_endpoint, ServiceClient};
use bazza_service_transport::{
    connect, external_session_endpoint_name, read_frame, write_frame, PeerStream,
};
use std::{path::Path, time::Duration};
use tokio::{task::JoinHandle, time::sleep};
use uuid::Uuid;

const IDLE: Duration = Duration::from_secs(20);
const CALL_MS: u64 = 4_000;

fn fixture(root: &Path) {
    bazza_service_test_support::create_v8(root).unwrap();
    drop(Repository::open_fixture(root, MigrationPolicy::Upgrade).unwrap());
}

async fn start(root: &Path, external: bool) -> (JoinHandle<()>, EndpointDescriptor) {
    let path = root.to_owned();
    let host = tokio::spawn(async move {
        if external {
            bazza_service::serve_fixture_with_external_read(&path, IDLE)
                .await
                .unwrap();
        } else {
            bazza_service::serve_fixture(&path, IDLE).await.unwrap();
        }
    });
    loop {
        if let Ok(descriptor) = read_endpoint(root) {
            return (host, descriptor);
        }
        assert!(
            !host.is_finished(),
            "fixture service stopped before discovery"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

fn client_hello(descriptor: &EndpointDescriptor) -> ClientHello {
    ClientHello {
        api: ApiVersion::CURRENT,
        schema_min: STORAGE_SCHEMA_MAX,
        schema_max: STORAGE_SCHEMA_MAX,
        build_id: descriptor.build_id.clone(),
        library_id: descriptor.library_id,
    }
}

fn private_request(descriptor: &EndpointDescriptor, request: Request) -> RequestEnvelope {
    RequestEnvelope {
        request_id: Uuid::new_v4(),
        library_epoch: descriptor.library_epoch,
        deadline_unix_ms: unix_ms() + CALL_MS,
        request,
    }
}

async fn application_call(
    stream: &mut PeerStream,
    descriptor: &EndpointDescriptor,
    request: Request,
) -> Reply {
    let frame = private_request(descriptor, request);
    write_frame(stream, &frame).await.unwrap();
    let reply: ReplyEnvelope = read_frame(stream).await.unwrap();
    assert_eq!(reply.request_id, frame.request_id);
    assert_eq!(reply.library_epoch, frame.library_epoch);
    reply.reply
}

async fn connect_application(descriptor: &EndpointDescriptor) -> PeerStream {
    let mut stream = connect(&descriptor.endpoint, descriptor.process_id)
        .await
        .unwrap();
    assert!(matches!(
        application_call(
            &mut stream,
            descriptor,
            Request::Hello(client_hello(descriptor))
        )
        .await,
        Reply::Hello(_)
    ));
    stream
}

async fn external_call(
    stream: &mut PeerStream,
    descriptor: &EndpointDescriptor,
    request: ExternalRequest,
) -> ExternalReply {
    let frame = ExternalRequestEnvelope {
        request_id: Uuid::new_v4(),
        library_epoch: descriptor.library_epoch,
        deadline_unix_ms: unix_ms() + CALL_MS,
        request,
    };
    write_frame(stream, &frame).await.unwrap();
    let reply: ExternalReplyEnvelope = read_frame(stream).await.unwrap();
    assert_eq!(reply.request_id, frame.request_id);
    assert_eq!(reply.library_epoch, frame.library_epoch);
    reply.reply
}

fn status() -> ExternalRequest {
    ExternalRequest::Read(FixtureReadRequest {
        tool: FixtureReadTool::Status,
        arguments: serde_json::json!({}),
    })
}

fn hello(descriptor: &EndpointDescriptor, grant: &ExternalReadGrantIssued) -> ExternalRequest {
    ExternalRequest::Hello(ExternalHello {
        client: client_hello(descriptor),
        connection_id: grant.connection_id,
        secret: grant.secret.clone(),
    })
}

async fn pair(
    application: &mut PeerStream,
    descriptor: &EndpointDescriptor,
    client_label: &str,
) -> ExternalReadGrantIssued {
    let Reply::ExternalReadGrantIssued(grant) = application_call(
        application,
        descriptor,
        Request::IssueExternalReadGrant(IssueExternalReadGrant {
            client_label: client_label.into(),
        }),
    )
    .await
    else {
        panic!("explicit pairing did not issue a grant");
    };
    assert_eq!(grant.library_id, descriptor.library_id);
    grant
}

#[tokio::test]
async fn external_pipe_rejects_private_frames_and_checks_revocation_on_live_sessions() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("Synthetic external MCP");
        fixture(&root);
        let (host, descriptor) = start(&root, true).await;
        let endpoint = external_session_endpoint_name(&root, descriptor.library_epoch.as_bytes())
            .unwrap();
        assert_ne!(endpoint, descriptor.endpoint);
        let mut application = connect_application(&descriptor).await;
        assert!(matches!(
            application_call(
                &mut application,
                &descriptor,
                Request::FixtureRead(Box::new(FixtureReadRequest {
                    tool: FixtureReadTool::Status,
                    arguments: serde_json::json!({}),
                })),
            )
            .await,
            Reply::Error(error) if error.code == ErrorCode::PermissionDenied
        ));
        let grant = pair(&mut application, &descriptor, "MCP fixture primary").await;
        let second = pair(&mut application, &descriptor, "MCP fixture second").await;
        let mut anonymous = connect(&endpoint, descriptor.process_id).await.unwrap();
        assert!(matches!(
            external_call(&mut anonymous, &descriptor, status()).await,
            ExternalReply::Error(error) if error.code == ErrorCode::AuthRequired
        ));
        drop(anonymous);

        let mut denied = connect(&endpoint, descriptor.process_id).await.unwrap();
        assert!(matches!(
            external_call(
                &mut denied,
                &descriptor,
                ExternalRequest::Hello(ExternalHello {
                    client: client_hello(&descriptor),
                    connection_id: grant.connection_id,
                    secret: WorkerSecret::random(),
                }),
            )
            .await,
            ExternalReply::Error(error) if error.code == ErrorCode::PermissionDenied
        ));
        drop(denied);

        let mut client = connect(&endpoint, descriptor.process_id).await.unwrap();
        assert!(matches!(
            external_call(&mut client, &descriptor, hello(&descriptor, &grant)).await,
            ExternalReply::Hello(accepted)
                if accepted.connection_id == grant.connection_id
                   && accepted.library_id == descriptor.library_id
        ));
        let read = external_call(
            &mut client,
            &descriptor,
            ExternalRequest::Read(FixtureReadRequest {
                tool: FixtureReadTool::GetMeeting,
                arguments: serde_json::json!({"meeting_id": bazza_service_test_support::READY_MEETING_ID}),
            }),
        )
        .await;
        assert!(matches!(
            read,
            ExternalReply::Read(reply)
                if reply.output["data"]["title"] == "Орион / Budget review"
        ));

        for private in [
            Request::Shutdown,
            Request::ListExternalReadGrants(ListExternalReadGrants { after: None, limit: 1 }),
        ] {
            let mut restricted = connect(&endpoint, descriptor.process_id).await.unwrap();
            assert!(matches!(
                external_call(&mut restricted, &descriptor, hello(&descriptor, &second)).await,
                ExternalReply::Hello(_)
            ));
            write_frame(&mut restricted, &private_request(&descriptor, private))
                .await
                .unwrap();
            assert!(read_frame::<ExternalReplyEnvelope>(&mut restricted)
                .await
                .is_err());
        }

        assert!(matches!(
            application_call(
                &mut application,
                &descriptor,
                Request::RevokeExternalReadGrant(RevokeExternalReadGrant {
                    connection_id: grant.connection_id,
                }),
            )
            .await,
            Reply::ExternalReadGrantRevoked(revoked)
                if revoked.connection_id == grant.connection_id
                   && revoked.library_id == descriptor.library_id
        ));
        assert!(matches!(
            application_call(
                &mut application,
                &descriptor,
                Request::ListExternalReadGrants(ListExternalReadGrants { after: None, limit: 51 }),
            )
            .await,
            Reply::Error(error) if error.code == ErrorCode::InvalidArgument
        ));
        let mut admin = ServiceClient::connect(&root).await.unwrap();
        let first = admin.list_fixture_external_read_grants(ListExternalReadGrants {
            after: None,
            limit: 1,
        }).await.unwrap();
        assert_eq!(first.library_id, descriptor.library_id);
        assert_eq!(first.grants.len(), 1);
        assert_eq!(first.next_after, Some(first.grants[0].connection_id));
        let last = admin.list_fixture_external_read_grants(ListExternalReadGrants {
            after: first.next_after,
            limit: 1,
        }).await.unwrap();
        assert_eq!(last.grants.len(), 1);
        assert!(last.next_after.is_none());
        let rows = [&first.grants[0], &last.grants[0]];
        assert!(rows[0].connection_id < rows[1].connection_id);
        let revoked = rows.iter().find(|row| row.connection_id == grant.connection_id).unwrap();
        assert_eq!(revoked.client_label, "MCP fixture primary");
        assert!(revoked.revoked_at.is_some());
        let active = rows.iter().find(|row| row.connection_id == second.connection_id).unwrap();
        assert_eq!(active.client_label, "MCP fixture second");
        assert!(active.revoked_at.is_none());
        for page in [&first, &last] {
            let output = serde_json::to_value(page).unwrap();
            let keys = output["grants"][0].as_object().unwrap();
            assert_eq!(keys.len(), 5);
            assert!(keys.contains_key("connection_id"));
            assert!(keys.contains_key("client_label"));
            assert!(keys.contains_key("permission"));
            assert!(keys.contains_key("created_at"));
            assert!(keys.contains_key("revoked_at"));
            let wire = output.to_string();
            assert!(!wire.contains(grant.secret.expose()));
            assert!(!wire.contains(second.secret.expose()));
        }
        drop(admin);
        assert!(matches!(
            external_call(&mut client, &descriptor, status()).await,
            ExternalReply::Error(error) if error.code == ErrorCode::PermissionDenied
        ));
        let mut unaffected = connect(&endpoint, descriptor.process_id).await.unwrap();
        assert!(matches!(
            external_call(&mut unaffected, &descriptor, hello(&descriptor, &second)).await,
            ExternalReply::Hello(_)
        ));
        assert!(matches!(
            external_call(&mut unaffected, &descriptor, status()).await,
            ExternalReply::Read(reply) if reply.output.get("data").is_some()
        ));
        drop(unaffected);
        assert!(matches!(
            application_call(&mut application, &descriptor, Request::Shutdown).await,
            Reply::ShutdownAccepted
        ));
        host.await.unwrap();
    })
    .await
    .expect("bounded external fixture IPC test");
}

#[tokio::test]
async fn ordinary_fixture_service_disables_external_pairing_and_listener() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("Private fixture");
        fixture(&root);
        let (host, descriptor) = start(&root, false).await;
        let endpoint =
            external_session_endpoint_name(&root, descriptor.library_epoch.as_bytes()).unwrap();
        let external = tokio::time::timeout(
            Duration::from_millis(150),
            connect(&endpoint, descriptor.process_id),
        )
        .await;
        assert!(!matches!(external, Ok(Ok(_))));
        let mut application = connect_application(&descriptor).await;
        assert!(matches!(
            application_call(
                &mut application,
                &descriptor,
                Request::IssueExternalReadGrant(IssueExternalReadGrant {
                    client_label: "Disabled fixture grant".into(),
                }),
            )
            .await,
            Reply::Error(error) if error.code == ErrorCode::PermissionDenied
        ));
        assert!(matches!(
            application_call(
                &mut application,
                &descriptor,
                Request::ListExternalReadGrants(ListExternalReadGrants { after: None, limit: 1 }),
            )
            .await,
            Reply::Error(error) if error.code == ErrorCode::PermissionDenied
        ));
        assert!(matches!(
            application_call(&mut application, &descriptor, Request::Shutdown).await,
            Reply::ShutdownAccepted
        ));
        host.await.unwrap();
    })
    .await
    .expect("bounded private-only fixture IPC test");
}

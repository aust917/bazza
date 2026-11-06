#![cfg(any(windows, target_os = "macos"))]

//! Real service-process/private-IPC tests against disposable synthetic libraries.
//! Three clients stand in for an application and two adapters; these are not
//! production GUI cutover, MCP grants, capture, or native inference acceptance.

use bazza_service_api::rpc::*;
use bazza_service_client::{
    publish_endpoint, read_endpoint, ClientError, ServiceClient, ENDPOINT_FILE,
};
use bazza_service_test_support::{create_v8, DAMAGED_MEETING_ID, MARKER_FILE, READY_MEETING_ID};
use bazza_service_transport::{connect, read_frame, write_frame, PeerStream, MAX_FRAME_BYTES};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    time::{sleep, timeout, Instant},
};
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(8);
const START_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("synthetic-library");
        create_v8(&root).unwrap();
        Self {
            _temporary: temporary,
            root,
        }
    }

    async fn migrated() -> Self {
        let fixture = Self::new();
        let output = cli("migrate-fixture", &fixture.root, &[]).await;
        assert!(
            output.status.success(),
            "explicit fixture migration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }
}

struct OwnedService {
    child: Child,
}

impl OwnedService {
    fn spawn(root: &Path) -> Self {
        let mut command = command("serve-fixture", root);
        command
            .args(["--idle-timeout-ms", "10000"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        Self {
            child: command.spawn().unwrap(),
        }
    }

    async fn ready(&mut self, root: &Path) -> ServiceClient {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Ok(client) = ServiceClient::connect(root).await {
                return client;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                use std::io::Read;
                let mut stderr = String::new();
                if let Some(mut pipe) = self.child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                panic!("owned service exited before readiness ({status}): {stderr}");
            }
            assert!(
                Instant::now() < deadline,
                "owned service readiness deadline"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    async fn exited(&mut self) -> ExitStatus {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned service did not exit")
    }

    fn crash(&mut self) {
        // The owned Child handle is the authority; no PID from discovery is killed.
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for OwnedService {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn command(action: &str, root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-service"));
    command
        .arg(action)
        .arg("--library")
        .arg(root)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // No visible console for test helpers.
    }
    command
}

async fn cli(action: &str, root: &Path, extra: &[&str]) -> Output {
    let mut command = tokio::process::Command::from(command(action, root));
    command.args(extra).kill_on_drop(true);
    timeout(START_TIMEOUT, command.output())
        .await
        .expect("bounded CLI deadline")
        .unwrap()
}

async fn raw(root: &Path) -> (EndpointDescriptor, PeerStream) {
    let descriptor = read_endpoint(root).unwrap();
    let stream = timeout(
        Duration::from_secs(1),
        connect(&descriptor.endpoint, descriptor.process_id),
    )
    .await
    .unwrap()
    .unwrap();
    (descriptor, stream)
}

fn envelope(descriptor: &EndpointDescriptor, request: Request) -> RequestEnvelope {
    RequestEnvelope {
        request_id: Uuid::new_v4(),
        library_epoch: descriptor.library_epoch,
        deadline_unix_ms: unix_ms() + 1000,
        request,
    }
}

fn hello(descriptor: &EndpointDescriptor) -> ClientHello {
    ClientHello {
        api: ApiVersion::CURRENT,
        schema_min: 1,
        schema_max: i64::MAX,
        build_id: build_id().into(),
        library_id: descriptor.library_id,
    }
}

async fn exchange(stream: &mut PeerStream, request: &RequestEnvelope) -> ReplyEnvelope {
    timeout(Duration::from_secs(1), async {
        write_frame(stream, request).await.unwrap();
        read_frame(stream).await.unwrap()
    })
    .await
    .expect("bounded raw RPC deadline")
}

fn assert_error(reply: &ReplyEnvelope, request_id: Uuid, code: ErrorCode) {
    assert_eq!(reply.request_id, request_id);
    match &reply.reply {
        Reply::Error(error) => assert_eq!(error.code, code),
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

async fn authenticated_stream(root: &Path) -> (EndpointDescriptor, PeerStream) {
    let (descriptor, mut stream) = raw(root).await;
    let request = envelope(&descriptor, Request::Hello(hello(&descriptor)));
    let reply = exchange(&mut stream, &request).await;
    assert_eq!(reply.request_id, request.request_id);
    assert!(matches!(reply.reply, Reply::Hello(_)));
    (descriptor, stream)
}

async fn shutdown(service: &mut OwnedService, mut client: ServiceClient) {
    client.shutdown().await.unwrap();
    drop(client);
    assert!(service.exited().await.success());
}

#[tokio::test(flavor = "current_thread")]
async fn connection_capacity_is_bounded_and_recovers_after_a_peer_disconnects() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let mut clients = vec![service.ready(&fixture.root).await];
        for _ in 1..MAX_CONNECTIONS {
            clients.push(ServiceClient::connect(&fixture.root).await.unwrap());
        }
        assert!(ServiceClient::connect(&fixture.root).await.is_err());
        clients.pop();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(mut replacement) = ServiceClient::connect(&fixture.root).await {
                assert_eq!(
                    replacement.status().await.unwrap().library_id,
                    clients[0].status().await.unwrap().library_id
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "a disconnected peer must release capacity"
            );
            sleep(Duration::from_millis(10)).await;
        }
        let client = clients.remove(0);
        drop(clients);
        shutdown(&mut service, client).await;
    })
    .await
    .expect("bounded connection-capacity test");
}

#[tokio::test(flavor = "current_thread")]
async fn three_simultaneous_private_clients_read_status_metadata_and_missing_objects() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        drop(service.ready(&fixture.root).await);
        let (application, first_adapter, second_adapter) = tokio::join!(
            ServiceClient::connect(&fixture.root),
            ServiceClient::connect(&fixture.root),
            ServiceClient::connect(&fixture.root)
        );
        let (mut application, mut first_adapter, mut second_adapter) = (
            application.unwrap(),
            first_adapter.unwrap(),
            second_adapter.unwrap(),
        );
        let (a, b, c) = tokio::join!(
            application.status(),
            first_adapter.status(),
            second_adapter.status()
        );
        let statuses = [a.unwrap(), b.unwrap(), c.unwrap()];
        for status in &statuses {
            assert_eq!(status.library_id, statuses[0].library_id);
            assert_eq!(status.library_epoch, statuses[0].library_epoch);
            assert_eq!(status.process_id, service.child.id());
            assert_eq!(status.api, ApiVersion::CURRENT);
            assert_eq!(status.build_id, build_id());
            assert_eq!(status.authentication, "local_os_user");
            assert!(status.development_fixture_only);
        }
        let (ready, missing, damaged) = tokio::join!(
            application.meeting(READY_MEETING_ID.parse().unwrap()),
            first_adapter.meeting(Uuid::new_v4()),
            second_adapter.meeting(DAMAGED_MEETING_ID.parse().unwrap())
        );
        let ready = ready.unwrap().unwrap();
        assert_eq!(ready.id, READY_MEETING_ID);
        assert_eq!(ready.title, "Орион / Budget review");
        assert_eq!(ready.participants, ["Анна", "Alex"]);
        assert!(missing.unwrap().is_none());
        assert_eq!(damaged.unwrap().unwrap().status, "failed");
        assert!(
            !fixture
                .root
                .join("recordings")
                .join(DAMAGED_MEETING_ID)
                .exists(),
            "metadata read must not create missing media"
        );
        drop(first_adapter);
        drop(second_adapter);
        shutdown(&mut service, application).await;
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn second_service_cannot_take_ownership_and_cli_reads_use_the_existing_owner() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let before = client.status().await.unwrap();
        let duplicate = cli("serve-fixture", &fixture.root, &[]).await;
        assert!(
            !duplicate.status.success(),
            "second service unexpectedly became an owner"
        );
        assert_eq!(
            client.status().await.unwrap().library_epoch,
            before.library_epoch
        );
        let status = cli("status", &fixture.root, &[]).await;
        assert!(
            status.status.success(),
            "status CLI failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let status: ServiceStatus = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status.process_id, service.child.id());
        let meeting = cli(
            "get-meeting",
            &fixture.root,
            &["--meeting-id", READY_MEETING_ID],
        )
        .await;
        assert!(meeting.status.success());
        let meeting: Option<MeetingMetadata> = serde_json::from_slice(&meeting.stdout).unwrap();
        assert_eq!(meeting.unwrap().title, "Орион / Budget review");
        shutdown(&mut service, client).await;
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn incompatible_api_schema_build_and_library_are_rejected_by_the_service() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        for case in 0..6 {
            let (descriptor, mut stream) = raw(&fixture.root).await;
            let mut wrong = hello(&descriptor);
            match case {
                0 => wrong.api.major += 1,
                1 => wrong.api.minor += 1,
                2 => wrong.schema_max = status.schema_version - 1,
                3 => wrong.schema_min = status.schema_version + 1,
                4 => wrong.build_id = "another-build".into(),
                5 => wrong.library_id = Uuid::new_v4(),
                _ => unreachable!(),
            }
            let request = envelope(&descriptor, Request::Hello(wrong));
            let reply = exchange(&mut stream, &request).await;
            assert_error(&reply, request.request_id, ErrorCode::UpgradeRequired);
        }
        assert!(matches!(ServiceClient::connect_with(&fixture.root, 1, status.schema_version - 1, build_id()).await,
            Err(ClientError::Remote(error)) if error.code == ErrorCode::UpgradeRequired));
        assert!(matches!(ServiceClient::connect_with(&fixture.root, 1, status.schema_version, "another-build").await,
            Err(ClientError::Remote(error)) if error.code == ErrorCode::UpgradeRequired));
        shutdown(&mut service, client).await;
    }).await.expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn epoch_authentication_and_deadline_checks_precede_data_operations() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let client = service.ready(&fixture.root).await;
        let (descriptor, mut stream) = raw(&fixture.root).await;
        let request = envelope(&descriptor, Request::GetStatus);
        assert_error(
            &exchange(&mut stream, &request).await,
            request.request_id,
            ErrorCode::AuthRequired,
        );
        drop(stream);
        let (descriptor, mut stream) = raw(&fixture.root).await;
        let mut request = envelope(&descriptor, Request::Hello(hello(&descriptor)));
        request.library_epoch = Uuid::new_v4();
        let reply = exchange(&mut stream, &request).await;
        assert_error(&reply, request.request_id, ErrorCode::ResyncRequired);
        assert_eq!(reply.library_epoch, descriptor.library_epoch);
        drop(stream);
        let (descriptor, mut stream) = authenticated_stream(&fixture.root).await;
        let mut expired = envelope(
            &descriptor,
            Request::GetMeeting(MeetingRequest {
                meeting_id: READY_MEETING_ID.parse().unwrap(),
            }),
        );
        expired.deadline_unix_ms = unix_ms().saturating_sub(1);
        assert_error(
            &exchange(&mut stream, &expired).await,
            expired.request_id,
            ErrorCode::DeadlineExceeded,
        );
        drop(stream);
        let (descriptor, mut stream) = authenticated_stream(&fixture.root).await;
        let mut unbounded = envelope(&descriptor, Request::GetStatus);
        unbounded.deadline_unix_ms = unix_ms() + MAX_CALL_MS + 60_000;
        assert_error(
            &exchange(&mut stream, &unbounded).await,
            unbounded.request_id,
            ErrorCode::InvalidArgument,
        );
        drop(stream);
        shutdown(&mut service, client).await;
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_methods_and_oversized_frames_close_only_the_bad_connection() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let (descriptor, mut stream) = raw(&fixture.root).await;
        let unknown = json!({"request_id":Uuid::new_v4(),"library_epoch":descriptor.library_epoch,
            "deadline_unix_ms":unix_ms()+1000,"request":{"method":"invoke","params":{"sql":"DROP TABLE meetings"}}});
        write_frame(&mut stream, &unknown).await.unwrap();
        assert!(timeout(Duration::from_secs(1), read_frame::<ReplyEnvelope>(&mut stream)).await.expect("unknown method connection must close").is_err());
        drop(stream);
        for length in [0, MAX_FRAME_BYTES as u32 + 1] {
            let (_, mut stream) = raw(&fixture.root).await;
            stream.write_u32(length).await.unwrap();
            stream.flush().await.unwrap();
            assert!(timeout(Duration::from_secs(1), read_frame::<ReplyEnvelope>(&mut stream)).await.expect("oversized frame connection must close without waiting for body").is_err());
        }
        assert_eq!(client.meeting(READY_MEETING_ID.parse().unwrap()).await.unwrap().unwrap().title, "Орион / Budget review");
        shutdown(&mut service, client).await;
    }).await.expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn graceful_shutdown_and_restart_keep_library_identity_but_change_epoch() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let first = client.status().await.unwrap();
        shutdown(&mut service, client).await;
        let mut reopened = OwnedService::spawn(&fixture.root);
        let mut client = reopened.ready(&fixture.root).await;
        let second = client.status().await.unwrap();
        assert_eq!(second.library_id, first.library_id);
        assert_ne!(second.library_epoch, first.library_epoch);
        assert_eq!(
            client
                .meeting(READY_MEETING_ID.parse().unwrap())
                .await
                .unwrap()
                .unwrap()
                .title,
            "Орион / Budget review"
        );
        shutdown(&mut reopened, client).await;
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn discovery_pid_is_untrusted_until_os_peer_authentication_succeeds() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let client = service.ready(&fixture.root).await;
        let original = read_endpoint(&fixture.root).unwrap();
        let mut stale = original.clone();
        stale.process_id = std::process::id(); // Live test parent, not the service.
        assert_ne!(stale.process_id, service.child.id());
        publish_endpoint(&fixture.root, &stale).unwrap();
        assert!(matches!(ServiceClient::connect(&fixture.root).await,
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied));
        publish_endpoint(&fixture.root, &original).unwrap();
        let mut reconnected = ServiceClient::connect(&fixture.root).await.unwrap();
        assert_eq!(
            reconnected.status().await.unwrap().library_id,
            original.library_id
        );
        drop(reconnected);
        shutdown(&mut service, client).await;
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn owned_service_crash_leaves_stale_discovery_but_new_owner_recovers() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::migrated().await;
        let mut service = OwnedService::spawn(&fixture.root);
        let mut first_client = service.ready(&fixture.root).await;
        let first = first_client.status().await.unwrap();
        let old_discovery = read_endpoint(&fixture.root).unwrap();
        service.crash();
        drop(first_client);
        // Recreate discovery explicitly if the platform cleaned it while dying;
        // a stale public descriptor must never be sufficient authentication.
        publish_endpoint(&fixture.root, &old_discovery).unwrap();
        assert!(ServiceClient::connect(&fixture.root).await.is_err());
        let mut reopened = OwnedService::spawn(&fixture.root);
        let mut client = reopened.ready(&fixture.root).await;
        let second = client.status().await.unwrap();
        assert_eq!(second.library_id, first.library_id);
        assert_ne!(second.library_epoch, first.library_epoch);
        assert_eq!(
            client
                .meeting(READY_MEETING_ID.parse().unwrap())
                .await
                .unwrap()
                .unwrap()
                .participants,
            ["Анна", "Alex"]
        );
        shutdown(&mut reopened, client).await;
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn serving_does_not_implicitly_upgrade_and_invalid_paths_create_no_library() {
    timeout(TEST_TIMEOUT, async {
        let missing_base = tempfile::tempdir().unwrap();
        let missing = missing_base.path().join("missing/library");
        for action in ["serve-fixture", "migrate-fixture", "status"] {
            assert!(!cli(action, &missing, &[]).await.status.success());
            assert!(!missing_base.path().join("missing").exists());
        }
        let fixture = Fixture::new();
        let before = fs::read(fixture.root.join("bazza.db")).unwrap();
        assert!(!cli("serve-fixture", &fixture.root, &[])
            .await
            .status
            .success());
        assert_eq!(fs::read(fixture.root.join("bazza.db")).unwrap(), before);
        assert!(!fixture.root.join(ENDPOINT_FILE).exists());
        assert!(!fixture.root.join(".bazza-library-identity.json").exists());
        assert!(!fixture.root.join("bazza.db-wal").exists());
        assert!(!fixture.root.join("bazza.db-shm").exists());

        fs::remove_file(fixture.root.join(MARKER_FILE)).unwrap();
        for action in ["serve-fixture", "migrate-fixture"] {
            assert!(!cli(action, &fixture.root, &[]).await.status.success());
            assert_eq!(fs::read(fixture.root.join("bazza.db")).unwrap(), before);
            assert!(!fixture.root.join(ENDPOINT_FILE).exists());
            assert!(!fixture.root.join(".bazza-library-identity.json").exists());
        }
    })
    .await
    .expect("service integration test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn future_library_is_rejected_without_wal_migrations_identity_or_discovery() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let path = fixture.root.join("bazza.db");
        let mut bytes = fs::read(&path).unwrap();
        bytes[60..64].copy_from_slice(&999_i32.to_be_bytes());
        fs::write(&path, &bytes).unwrap();
        for action in ["migrate-fixture", "serve-fixture"] {
            assert!(!cli(action, &fixture.root, &[]).await.status.success());
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(!fixture.root.join("bazza.db-wal").exists());
            assert!(!fixture.root.join("bazza.db-shm").exists());
            assert!(!fixture.root.join(ENDPOINT_FILE).exists());
            assert!(!fixture.root.join(".bazza-library-identity.json").exists());
        }
    })
    .await
    .expect("service integration test deadline");
}

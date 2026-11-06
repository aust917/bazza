#![cfg(any(windows, target_os = "macos"))]

//! Real local service processes against a fresh, disposable managed library.
use bazza_core::IDENTITY_FILE;
use bazza_service_api::{
    external_read::{IssueExternalReadGrant, RevokeExternalReadGrant},
    fixture_read::{FixtureReadRequest, FixtureReadTool, FIXTURE_READ_CAPABILITY},
    rpc::{ErrorCode, ServiceStatus},
    worker::WorkerSecret,
};
use bazza_service_client::{
    external_read::ExternalReadClient, ClientError, ServiceClient, ENDPOINT_FILE,
};
use serde_json::json;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(25);
const START_TIMEOUT: Duration = Duration::from_secs(8);

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
        command.creation_flags(0x0800_0000);
    }
    command
}

async fn cli(action: &str, root: &Path) -> Output {
    let mut command = tokio::process::Command::from(command(action, root));
    command.kill_on_drop(true);
    timeout(START_TIMEOUT, command.output())
        .await
        .expect("bounded managed CLI call")
        .unwrap()
}

struct ServiceProcess(Child);

impl ServiceProcess {
    fn start(action: &str, root: &Path) -> Self {
        Self(
            command(action, root)
                .args(["--idle-timeout-ms", "15000"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }

    async fn ready(&mut self, root: &Path) -> ServiceClient {
        timeout(START_TIMEOUT, async {
            loop {
                if let Ok(client) = ServiceClient::connect_managed(root).await {
                    return client;
                }
                if let Some(status) = self.0.try_wait().unwrap() {
                    use std::io::Read;
                    let mut stderr = String::new();
                    self.0
                        .stderr
                        .take()
                        .unwrap()
                        .read_to_string(&mut stderr)
                        .unwrap();
                    panic!("managed service exited before readiness ({status}): {stderr}");
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("managed service readiness deadline")
    }

    async fn wait_for_exit(&mut self) {
        timeout(START_TIMEOUT, async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "managed process shutdown failed: {status}"
                    );
                    return;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("managed service shutdown deadline");
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn denied<T>(result: Result<T, ClientError>) {
    assert!(
        matches!(result, Err(ClientError::Remote(error)) if error.code == ErrorCode::PermissionDenied)
    );
}

fn read_status() -> FixtureReadRequest {
    FixtureReadRequest {
        tool: FixtureReadTool::Status,
        arguments: json!({}),
    }
}

#[tokio::test]
async fn fresh_managed_process_has_no_fixture_bypass_and_external_reads_require_a_grant() {
    timeout(TEST_TIMEOUT, async {
        let base = tempfile::tempdir().unwrap();
        let old = bazza_service_test_support::create_v8(&base.path().join("Old library")).unwrap();
        fs::remove_file(old.root.join(bazza_service_test_support::MARKER_FILE)).unwrap();
        let old_database = fs::read(&old.database_path).unwrap();
        let fresh = base.path().join("Managed working Орион");
        assert!(!cli("init-library", &old.root).await.status.success());
        let output = cli("init-library", &fresh).await;
        assert!(
            output.status.success(),
            "managed initialization: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let init: ServiceStatus = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!init.development_fixture_only);
        let identity: serde_json::Value =
            serde_json::from_slice(&fs::read(fresh.join(IDENTITY_FILE)).unwrap()).unwrap();
        assert_eq!(
            identity,
            json!({"version": 2, "library_id": init.library_id})
        );
        assert!(!fresh.join(ENDPOINT_FILE).exists());
        assert!(!fresh.join(bazza_service_test_support::MARKER_FILE).exists());
        assert!(!cli("serve-fixture", &fresh).await.status.success());

        let mut private = ServiceProcess::start("serve-library", &fresh);
        let mut app = private.ready(&fresh).await;
        let status = app.status().await.unwrap();
        assert_eq!(status.library_id, init.library_id);
        assert!(!status.development_fixture_only);
        assert!(!status
            .capabilities
            .iter()
            .any(|capability| capability == FIXTURE_READ_CAPABILITY));
        assert!(ServiceClient::connect(&fresh).await.is_err());
        let meetings = app.desktop_meetings(Default::default()).await.unwrap();
        assert!(meetings.data.items.is_empty());
        denied(app.fixture_read(read_status()).await);
        denied(
            app.issue_external_read_grant(IssueExternalReadGrant {
                client_label: "isolated test".into(),
            })
            .await,
        );
        drop(app);
        assert!(cli("status-library", &fresh).await.status.success());
        assert!(cli("shutdown-library", &fresh).await.status.success());
        private.wait_for_exit().await;

        let mut external = ServiceProcess::start("serve-library-external", &fresh);
        let mut app = external.ready(&fresh).await;
        denied(app.fixture_read(read_status()).await);
        denied(
            ExternalReadClient::connect_managed_pinned(
                &fresh,
                Uuid::new_v4(),
                &WorkerSecret::random(),
                init.library_id,
            )
            .await,
        );
        let grant = app
            .issue_external_read_grant(IssueExternalReadGrant {
                client_label: "isolated test".into(),
            })
            .await
            .unwrap();
        let mut reader = ExternalReadClient::connect_managed_pinned(
            &fresh,
            grant.connection_id,
            &grant.secret,
            grant.library_id,
        )
        .await
        .unwrap();
        let reply = reader.read(read_status()).await.unwrap();
        assert_eq!(reply.output["library_id"], json!(init.library_id));
        assert_eq!(reply.output["data"]["schema_version"], init.schema_version);
        app.revoke_external_read_grant(RevokeExternalReadGrant {
            connection_id: grant.connection_id,
        })
        .await
        .unwrap();
        denied(reader.read(read_status()).await);
        app.shutdown().await.unwrap();
        external.wait_for_exit().await;

        assert_eq!(fs::read(old.database_path).unwrap(), old_database);
        assert!(!old.root.join(IDENTITY_FILE).exists());
        assert!(!old.root.join(ENDPOINT_FILE).exists());
    })
    .await
    .expect("managed process integration deadline");
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires an interactive Windows Task Scheduler session"]
async fn registered_managed_broker_restarts_for_readers_and_revoke_keeps_desktop_alive() {
    use bazza_service_client::{broker, RepositoryClient};

    async fn wait_for_shutdown(root: &Path) {
        timeout(START_TIMEOUT, async {
            while root.join(ENDPOINT_FILE).exists() {
                sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("service shutdown removed the endpoint");
    }

    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("Windows installed Орион");
    let expected = bazza_core::Repository::create_managed(&root)
        .unwrap()
        .status()
        .library_id;
    broker::register_managed(Path::new(env!("CARGO_BIN_EXE_bazza-service")), &root)
        .await
        .unwrap();
    let selected = root.clone();
    let exercise = tokio::spawn(async move {
        timeout(TEST_TIMEOUT, async {
            let (desktop, admin) = tokio::join!(
                RepositoryClient::connect_or_start_managed(&selected),
                ServiceClient::connect_or_start_managed(&selected),
            );
            let desktop = desktop.unwrap();
            let mut admin = admin.unwrap();
            let first = admin.status().await.unwrap();
            assert_eq!(desktop.identity().await.library_epoch, first.library_epoch);
            assert_eq!(first.library_id, expected);
            assert!(broker::unregister_managed(&selected).await.is_err());

            let grant = admin
                .issue_external_read_grant(IssueExternalReadGrant {
                    client_label: "isolated broker test".into(),
                })
                .await
                .unwrap();
            admin.shutdown().await.unwrap();
            wait_for_shutdown(&selected).await;

            let mut reader = ExternalReadClient::connect_or_start_managed_pinned(
                &selected,
                grant.connection_id,
                &grant.secret,
                expected,
            )
            .await
            .unwrap();
            let reply = reader.read(read_status()).await.unwrap();
            assert_eq!(reply.output["library_id"], json!(expected));
            let second = desktop.reconnect().await.unwrap();
            assert_ne!(second.library_epoch, first.library_epoch);
            assert_eq!(second.library_id, expected);

            let mut admin = ServiceClient::connect_managed(&selected).await.unwrap();
            admin
                .revoke_external_read_grant(RevokeExternalReadGrant {
                    connection_id: grant.connection_id,
                })
                .await
                .unwrap();
            denied(reader.read(read_status()).await);
            assert_eq!(desktop.bootstrap().await.unwrap().identity, second);
        })
        .await
        .expect("broker lifecycle integration deadline");
    })
    .await;

    if let Ok(mut admin) = ServiceClient::connect_managed(&root).await {
        admin.shutdown().await.unwrap();
        wait_for_shutdown(&root).await;
    }
    timeout(START_TIMEOUT, async {
        while broker::unregister_managed(&root).await.is_err() {
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("broker registration cleaned up");
    exercise.unwrap();
}

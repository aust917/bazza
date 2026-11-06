//! Child of desktop_tests: reuse its authenticated synthetic peer helpers.
use super::*;
use bazza_service_api::configuration::{
    NativeConfiguration, NativeSettings, UpdateNativeConfigurationRequest,
};
use tokio::task::JoinHandle;

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
    let d = descriptor(root.path());
    publish_endpoint(root.path(), &d).unwrap();
    let mut listener = Listener::bind(&d.endpoint).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.process_id, std::process::id());
        hello(&mut stream, &d).await;
        serve(stream, d).await;
    });
    Fake { root, task }
}
fn configuration() -> NativeConfiguration {
    NativeConfiguration {
        revision: "a".repeat(64),
        settings: NativeSettings::default(),
    }
}
fn request() -> UpdateNativeConfigurationRequest {
    UpdateNativeConfigurationRequest {
        expected_revision: "a".repeat(64),
        idempotency_key: "config-original-key".into(),
        settings: NativeSettings::default(),
    }
}
fn receipt() -> DesktopMutationOutcome {
    mutation("native_configuration", &"b".repeat(64), 5)
}
async fn poisoned(client: &mut ServiceClient) {
    assert!(
        client.stream.is_none(),
        "invalid configuration reply must drop the stream"
    );
    assert!(matches!(client.status().await, Err(ClientError::Protocol)));
}

#[test]
fn malformed_configuration_read_replies_poison_the_stream() {
    run(async {
        for case in 0..7 {
            let mut server = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(request.request, Request::NativeConfiguration));
                let mut value = envelope(&d, configuration());
                let reply = match case {
                    0 => Reply::NativeConfigurationUpdated(envelope(&d, receipt())),
                    1 => {
                        value.library_id = Uuid::new_v4();
                        Reply::NativeConfiguration(Box::new(value))
                    }
                    2 => {
                        value.library_epoch = Uuid::new_v4();
                        Reply::NativeConfiguration(Box::new(value))
                    }
                    3 => {
                        value.data.revision = "bad revision".into();
                        Reply::NativeConfiguration(Box::new(value))
                    }
                    4 => {
                        value.data.revision = "A".repeat(64);
                        Reply::NativeConfiguration(Box::new(value))
                    }
                    5 => {
                        value.data.revision = "a".repeat(63);
                        Reply::NativeConfiguration(Box::new(value))
                    }
                    // Escaped JSON exceeds 64 KiB, while still fitting the
                    // transport frame so typed reply budget validation runs.
                    6 => {
                        value.data.settings.user_name = "\0".repeat(12_000);
                        Reply::NativeConfiguration(Box::new(value))
                    }
                    _ => unreachable!(),
                };
                send(&mut stream, &request, reply).await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            assert!(
                matches!(
                    client.native_configuration().await,
                    Err(ClientError::Protocol)
                ),
                "case {case}"
            );
            poisoned(&mut client).await;
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn forged_configuration_receipts_poison_the_stream() {
    run(async {
        for case in 0..11 {
            let mut server = fake(move |mut stream, d| async move {
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                assert!(matches!(
                    request.request,
                    Request::UpdateNativeConfiguration(_)
                ));
                let mut value = envelope(&d, receipt());
                let reply = match case {
                    0 => Reply::NativeConfiguration(Box::new(envelope(&d, configuration()))),
                    1 => {
                        value.library_id = Uuid::new_v4();
                        Reply::NativeConfigurationUpdated(value)
                    }
                    2 => {
                        value.library_epoch = Uuid::new_v4();
                        Reply::NativeConfigurationUpdated(value)
                    }
                    3 => {
                        value.data.entity_id = "desktop_preferences".into();
                        Reply::NativeConfigurationUpdated(value)
                    }
                    4 => {
                        value.data.revision = "invalid".into();
                        Reply::NativeConfigurationUpdated(value)
                    }
                    5 => {
                        value.data.revision = "B".repeat(64);
                        Reply::NativeConfigurationUpdated(value)
                    }
                    6 => {
                        value.data.job_id = Some(Uuid::new_v4().to_string());
                        Reply::NativeConfigurationUpdated(value)
                    }
                    7 => {
                        value.data.change_sequence = 0;
                        Reply::NativeConfigurationUpdated(value)
                    }
                    8 => {
                        value.data.change_sequence = -1;
                        Reply::NativeConfigurationUpdated(value)
                    }
                    9 => {
                        value.data.change_sequence = i64::MAX;
                        Reply::NativeConfigurationUpdated(value)
                    }
                    10 => {
                        value.data.job_id = Some("x".repeat(70_000));
                        Reply::NativeConfigurationUpdated(value)
                    }
                    _ => unreachable!(),
                };
                send(&mut stream, &request, reply).await;
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            assert!(
                matches!(
                    client.update_native_configuration(request()).await,
                    Err(ClientError::Protocol)
                ),
                "case {case}"
            );
            poisoned(&mut client).await;
            (&mut server.task).await.unwrap();
        }
    });
}

#[test]
fn typed_invalid_legacy_settings_read_is_preserved_and_invalid_updates_never_reach_wire() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::NativeConfiguration));
            let mut legacy = configuration();
            legacy.settings.video_fps = 0;
            legacy.settings.llm_backend = "foreign-legacy-backend".into();
            send(
                &mut stream,
                &request,
                Reply::NativeConfiguration(Box::new(envelope(&d, legacy))),
            )
            .await;
            // No invalid update may consume a wire request or poison a valid
            // stream; the next request is the caller's explicit status read.
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(request.request, Request::GetStatus));
            send(&mut stream, &request, Reply::Status(status(&d))).await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let legacy = client.native_configuration().await.unwrap().data;
        assert_eq!(legacy.settings.video_fps, 0);
        assert_eq!(legacy.settings.llm_backend, "foreign-legacy-backend");
        let mut invalid = request();
        invalid.settings = legacy.settings;
        let mut oversized = request();
        oversized.settings.user_name = "\0".repeat(12_000);
        let mut bad_revision = request();
        bad_revision.expected_revision = "wrong".into();
        let mut bad_key = request();
        bad_key.idempotency_key = "contains space".into();
        for invalid in [invalid, oversized, bad_revision, bad_key] {
            assert!(
                matches!(client.update_native_configuration(invalid).await, Err(ClientError::Remote(error)) if error.code==ErrorCode::InvalidArgument)
            );
            assert!(client.stream.is_some());
        }
        client.status().await.unwrap();
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn configuration_conflict_does_not_retry_or_rewrite_revision_key_or_private_settings() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            let wire: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::UpdateNativeConfiguration(input) = &wire.request else {
                panic!("expected explicit configuration update");
            };
            assert_eq!(input.expected_revision, "a".repeat(64));
            assert_eq!(input.idempotency_key, "config-original-key");
            assert_eq!(
                input.settings.llm_api_key.as_deref(),
                Some("synthetic-private-credential")
            );
            assert_eq!(input.settings.video_fps, 30);
            send(
                &mut stream,
                &wire,
                Reply::Error(RpcError::new(ErrorCode::Conflict)),
            )
            .await;
            let wire: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(
                matches!(wire.request, Request::GetStatus),
                "Conflict triggered an implicit retry"
            );
            send(&mut stream, &wire, Reply::Status(status(&d))).await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let mut input = request();
        input.settings.video_fps = 30;
        input.settings.llm_api_key = Some("synthetic-private-credential".into());
        input.settings.llm_endpoint =
            "https://fixture.invalid/?private=synthetic-private-url".into();
        input.settings.calendar_sources =
            vec!["https://fixture.invalid/synthetic-private-calendar".into()];
        let original = serde_json::to_value(&input).unwrap();
        let debug = format!("{input:?}");
        assert!(!debug.contains("synthetic-private-credential"));
        assert!(!debug.contains("synthetic-private-url"));
        assert!(!debug.contains("synthetic-private-calendar"));
        assert!(
            matches!(client.update_native_configuration(input.clone()).await,Err(ClientError::Remote(error)) if error.code==ErrorCode::Conflict)
        );
        assert_eq!(serde_json::to_value(input).unwrap(), original);
        assert!(client.stream.is_some());
        client.status().await.unwrap();
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn configuration_read_and_update_preserve_private_wire_values_and_exact_receipt() {
    run(async {
        let mut server = fake(|mut stream, d| async move {
            let read: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(read.request, Request::NativeConfiguration));
            let mut value = configuration();
            value.settings.llm_api_key = Some("synthetic-private-roundtrip".into());
            send(
                &mut stream,
                &read,
                Reply::NativeConfiguration(Box::new(envelope(&d, value))),
            )
            .await;
            let update: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::UpdateNativeConfiguration(input) = &update.request else {
                panic!("expected explicit configuration update");
            };
            assert_eq!(input.expected_revision, "a".repeat(64));
            assert_eq!(input.idempotency_key, "config-original-key");
            assert_eq!(
                input.settings.llm_api_key.as_deref(),
                Some("synthetic-private-roundtrip")
            );
            send(
                &mut stream,
                &update,
                Reply::NativeConfigurationUpdated(envelope(&d, receipt())),
            )
            .await;
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let read = client.native_configuration().await.unwrap().data;
        assert_eq!(
            read.settings.llm_api_key.as_deref(),
            Some("synthetic-private-roundtrip")
        );
        assert!(!format!("{read:?}").contains("synthetic-private-roundtrip"));
        let reply = client
            .update_native_configuration(UpdateNativeConfigurationRequest {
                expected_revision: read.revision,
                idempotency_key: "config-original-key".into(),
                settings: read.settings,
            })
            .await
            .unwrap();
        assert_eq!(reply.data, receipt());
        assert!(client.stream.is_some());
        (&mut server.task).await.unwrap();
    });
}

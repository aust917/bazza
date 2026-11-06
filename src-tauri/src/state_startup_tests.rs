use super::*;
use bazza_service_client::{
    api::{
        configuration::UpdateNativeConfigurationRequest,
        desktop::{DesktopPreferencesPatch, UpdateDesktopPreferencesRequest},
    },
    validate_fixture_root,
};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an already running service on a disposable BAZZA_TEST_SERVICE_FIXTURE"]
async fn initializes_runtime_from_running_fixture_service() {
    let root = PathBuf::from(
        std::env::var_os("BAZZA_TEST_SERVICE_FIXTURE")
            .expect("set BAZZA_TEST_SERVICE_FIXTURE to an absolute marked disposable root"),
    );
    let root = validate_fixture_root(&root).expect("invalid disposable service fixture");
    let had_models = root.join("models").exists();
    let repository = RepositoryClient::connect(&root).await.unwrap();
    let identity = repository.identity().await;
    let native = repository.native_configuration().await.unwrap();
    assert_eq!(native.identity, identity);
    let mut settings = native.data.settings;
    settings.language = Some("ja".into());
    settings.video_fps = 29;
    settings.close_to_tray = false;
    settings.hotkey = "CmdOrCtrl+Alt+F9".into();
    repository
        .update_native_configuration(UpdateNativeConfigurationRequest {
            expected_revision: native.data.revision,
            idempotency_key: Uuid::new_v4().to_string(),
            settings: settings.clone(),
        })
        .await
        .unwrap();

    let preferences = repository.preferences().await.unwrap();
    let (locale, expected_locale) = match preferences.data.ui_locale {
        DesktopLocale::Ru => (DesktopLocale::En, UiLocale::En),
        DesktopLocale::En => (DesktopLocale::Ru, UiLocale::Ru),
    };
    repository
        .update_preferences(UpdateDesktopPreferencesRequest {
            expected_revision: preferences.data.revision,
            idempotency_key: Uuid::new_v4().to_string(),
            patch: DesktopPreferencesPatch {
                ui_locale: Some(locale),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let preferences = repository.preferences().await.unwrap();
    assert_eq!(preferences.data.ui_locale, locale);
    assert_eq!(preferences.data.hotkey, settings.hotkey);
    assert!(!preferences.data.close_to_tray);

    let before = repository
        .templates_list(None, 100, MAX_TEMPLATE_DATA_BYTES as u32)
        .await
        .unwrap();
    assert!(before.next.is_none(), "use a disposable small fixture");
    let hardware = Hardware {
        cpu_cores: 2,
        cpu_name: "synthetic startup test".into(),
        ram_gb: 8.0,
        gpu: None,
        os: "windows".into(),
        arch: "test".into(),
    };
    let runtime = RuntimeState::initialize_from_service_with(&repository, || hardware.clone())
        .await
        .unwrap();
    assert_eq!(runtime.hardware, hardware);
    assert_eq!(*runtime.ui_locale.read(), expected_locale);
    assert!(
        serde_json::to_value(&*runtime.settings.read()).unwrap()
            == serde_json::to_value(&settings).unwrap(),
        "the runtime must preserve the private native settings snapshot"
    );
    assert!(runtime.engines.lock().last_used.is_none());

    let seeded = repository
        .templates_list(None, 100, MAX_TEMPLATE_DATA_BYTES as u32)
        .await
        .unwrap();
    assert_eq!(seeded.identity, identity);
    assert!(seeded.next.is_none());
    assert!(before.items.iter().all(|item| seeded.items.contains(item)));
    for builtin in bazza_llm::summary::builtin_templates() {
        let template = seeded.items.iter().find(|item| item.id == builtin.id);
        assert!(template.is_some(), "missing builtin {}", builtin.id);
    }
    let reopened = RuntimeState::initialize_from_service_with(&repository, || hardware.clone())
        .await
        .unwrap();
    assert_eq!(*reopened.ui_locale.read(), expected_locale);
    let reused = repository
        .templates_list(None, 100, MAX_TEMPLATE_DATA_BYTES as u32)
        .await
        .unwrap();
    assert_eq!(reused.catalog_revision, seeded.catalog_revision);
    assert!(reused.items == seeded.items, "existing templates changed");
    assert_eq!(
        repository.preferences().await.unwrap().data,
        preferences.data
    );
    assert_eq!(root.join("models").exists(), had_models);
}

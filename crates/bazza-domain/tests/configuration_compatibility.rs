use bazza_domain::{NativeSettings, NativeSettingsValidationCode as Code, NativeSttProvider, NativeTier, NativeVideoTarget};
use serde_json::json;

#[test]
fn legacy_missing_fields_preserve_the_native_default_document() {
    let defaults: NativeSettings = serde_json::from_value(json!({"future_setting": true})).unwrap();
    // Native Settings defaults, including capture and integration policy.
    let expected = json!({
        "onboarded":false,"user_name":"Me","language":null,"mic_device":null,"loopback_device":null,
        "capture_system":true,"video_enabled":true,
        "video_target":{"kind":"monitor","index":0},"video_fps":15,"video_max_height":1080,
        "tier_override":null,"live_asr_model":null,"quality_asr_model":null,
        "stt_provider":"local","soniox_model":"stt-rt-v5","soniox_consent":false,"llm_model":null,
        "hotkey":"CmdOrCtrl+Shift+R","detection_enabled":true,"audio_activity_enabled":true,
        "never_apps":[],"popup_timeout_secs":30,"close_to_tray":true,"llm_backend":"bundled",
        "llm_endpoint":"http://localhost:11434/v1","llm_endpoint_model":"qwen3:8b","llm_api_key":null,
        "cleanup_enabled":false,"diarize_enabled":true,"summary_enabled":false,"default_template_id":"general",
        "share_port":47123,"calendar_sources":[],"calendar_refresh_min":15,
        "webhook_url":"","webhook_enabled":false,"auto_update_check":false
    });
    assert_eq!(expected.as_object().unwrap().len(), 37);
    assert_eq!(serde_json::to_value(&defaults).unwrap(), expected);
    defaults.validate().unwrap();
    let legacy: NativeSettings =
        serde_json::from_value(json!({"capture_system":false,"language":null})).unwrap();
    assert!(!legacy.capture_system);
    assert_eq!(legacy.language, None);
    assert_eq!((legacy.video_fps, legacy.video_max_height), (15, 1080));
}

#[test]
fn populated_legacy_settings_roundtrip_without_rewriting_private_or_foreign_values() {
    let mut document = json!({
        "onboarded":true,"user_name":" Анна ","language":"en","mic_device":"Mic fixture","loopback_device":"Loop fixture",
        "capture_system":false,"audio_scope":"all","video_enabled":false,
        "video_target":{"kind":"window","title":" Exact title "},"video_fps":30,"video_max_height":720,
        "tier_override":"gpu","live_asr_model":"foreign-live-model","quality_asr_model":"foreign-quality-model",
        "stt_provider":"soniox","soniox_model":"custom-model_7","soniox_consent":true,"llm_model":"foreign-llm",
        "hotkey":"Ctrl+F8","detection_enabled":false,"audio_activity_enabled":false,
        "never_apps":["custom-app"],"popup_timeout_secs":0,"close_to_tray":false,"llm_backend":"future-private-backend",
        "llm_endpoint":"https://user:private@endpoint.invalid/v1?token=private","llm_endpoint_model":"foreign-remote-model","llm_api_key":" exact credential ",
        "cleanup_enabled":true,"diarize_enabled":false,"summary_enabled":true,"default_template_id":"custom-template",
        "share_port":0,"calendar_sources":["webcal://fixture.invalid/private-calendar","C:\\private\\calendar.ics"],"calendar_refresh_min":0,
        "webhook_url":"https://fixture.invalid/private-hook","webhook_enabled":true,"auto_update_check":true
    });
    let settings: NativeSettings = serde_json::from_value(document.clone()).unwrap();
    settings.validate().unwrap();
    assert_eq!(settings.llm_backend, "future-private-backend");
    document.as_object_mut().unwrap().remove("audio_scope");
    assert_eq!(serde_json::to_value(&settings).unwrap(), document);
    assert_eq!(
        serde_json::from_str::<NativeSettings>(&serde_json::to_string(&settings).unwrap()).unwrap(),
        settings
    );
}

#[test]
fn native_enum_wire_shapes_and_unknown_variant_behavior_stay_compatible() {
    for (tier, wire) in [
        (NativeTier::CpuLow, "cpu_low"),
        (NativeTier::CpuHigh, "cpu_high"),
        (NativeTier::Gpu, "gpu"),
    ] {
        assert_eq!(serde_json::to_value(tier).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<NativeTier>(json!(wire)).unwrap(),
            tier
        );
    }
    assert_eq!(
        serde_json::to_value(NativeVideoTarget::Monitor { index: 7 }).unwrap(),
        json!({"kind":"monitor","index":7})
    );
    assert_eq!(
        serde_json::to_value(NativeVideoTarget::Window {
            title: " fixture ".into()
        })
        .unwrap(),
        json!({"kind":"window","title":" fixture "})
    );
    assert_eq!(
        serde_json::to_value(NativeSttProvider::Soniox).unwrap(),
        "soniox"
    );
    for value in [
        json!({"stt_provider":"unknown"}),
        json!({"tier_override":"CPU"}),
        json!({"video_target":{"kind":"unknown"}}),
        json!({"video_target":{"kind":"monitor","index":-1}}),
    ] {
        assert!(serde_json::from_value::<NativeSettings>(value).is_err());
    }
}

#[test]
fn soniox_validation_preserves_identifier_limits_consent_and_error_precedence() {
    for model in ["stt-rt-v5".to_owned(), "a".repeat(128), "-_".to_owned()] {
        NativeSettings {
            soniox_model: model,
            ..NativeSettings::default()
        }
        .validate()
        .unwrap();
    }
    for model in [
        "".to_owned(),
        " ".to_owned(),
        "a".repeat(129),
        "модель".to_owned(),
        "model/v5".to_owned(),
        "model\n".to_owned(),
    ] {
        let invalid = NativeSettings {
            soniox_model: model,
            stt_provider: NativeSttProvider::Soniox,
            video_fps: 0,
            ..NativeSettings::default()
        };
        assert_eq!(invalid.validate(), Err(Code::SonioxModel));
    }
    let mut cloud = NativeSettings {
        stt_provider: NativeSttProvider::Soniox,
        ..NativeSettings::default()
    };
    assert_eq!(cloud.validate(), Err(Code::SonioxConsent));
    cloud.soniox_consent = true;
    cloud.validate().unwrap(); // Native validation does not require a stored key.
    assert_eq!(
        serde_json::to_value(Code::SonioxConsent).unwrap(),
        "soniox_consent"
    );
    assert_eq!(Code::SonioxModel.to_string(), "soniox_model");
}

#[test]
fn video_validation_keeps_current_encoder_boundaries() {
    for (fps, height) in [(1, 144), (15, 1080), (60, 4320)] {
        NativeSettings {
            video_fps: fps,
            video_max_height: height,
            ..NativeSettings::default()
        }
        .validate()
        .unwrap();
    }
    for fps in [0, 61, u32::MAX] {
        assert_eq!(
            NativeSettings {
                video_fps: fps,
                ..NativeSettings::default()
            }
            .validate(),
            Err(Code::VideoFps)
        );
    }
    for height in [0, 142, 719, 4322, u32::MAX] {
        assert_eq!(
            NativeSettings {
                video_max_height: height,
                ..NativeSettings::default()
            }
            .validate(),
            Err(Code::VideoHeight)
        );
    }
}

#[test]
fn debug_redacts_credentials_private_urls_calendars_and_arbitrary_user_strings() {
    let secret = "synthetic-private-key-marker";
    let endpoint = "https://synthetic-user:synthetic-password@fixture.invalid/v1?token=synthetic-endpoint-token";
    let calendar = "https://fixture.invalid/synthetic-secret-calendar.ics";
    let hook = "https://fixture.invalid/synthetic-private-hook";
    let user = "synthetic-private-user-marker";
    let settings = NativeSettings {
        llm_api_key: Some(secret.into()),
        llm_endpoint: endpoint.into(),
        calendar_sources: vec![calendar.into()],
        webhook_url: hook.into(),
        user_name: user.into(),
        ..NativeSettings::default()
    };
    for debug in [format!("{settings:?}"), format!("{settings:#?}")] {
        for private in [
            secret,
            endpoint,
            calendar,
            hook,
            user,
            "synthetic-password",
            "synthetic-endpoint-token",
        ] {
            assert!(
                !debug.contains(private),
                "private configuration leaked in Debug"
            );
        }
        assert!(debug.contains("[redacted]"));
    }
    let wire = serde_json::to_value(&settings).unwrap();
    assert_eq!(wire["llm_api_key"], secret);
    assert_eq!(wire["llm_endpoint"], endpoint);
    assert_eq!(wire["calendar_sources"], json!([calendar]));
    assert_eq!(wire["webhook_url"], hook);
}

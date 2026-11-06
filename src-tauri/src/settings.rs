use bazza_capture::VideoTarget;
use bazza_models::Tier;
use bazza_store::Store;
use serde::{Deserialize, Serialize};

/// True when the binary was compiled with a GPU backend.
pub const GPU_BUILD: bool = cfg!(any(feature = "cuda", feature = "metal"));

pub const SETTINGS_KEY: &str = "settings";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub onboarded: bool,
    pub user_name: String,
    pub language: Option<String>,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub capture_system: bool,
    pub video_enabled: bool,
    pub video_target: VideoTarget,
    pub video_fps: u32,
    pub video_max_height: u32,
    pub tier_override: Option<Tier>,
    pub live_asr_model: Option<String>,
    pub quality_asr_model: Option<String>,
    pub stt_provider: bazza_asr::provider::SttProvider,
    pub soniox_model: String,
    pub soniox_consent: bool,
    pub llm_model: Option<String>,
    pub hotkey: String,
    pub detection_enabled: bool,
    pub audio_activity_enabled: bool,
    pub never_apps: Vec<String>,
    pub popup_timeout_secs: u64,
    pub close_to_tray: bool,
    /// "bundled" (llama.cpp in-process) or "openai" (any OpenAI-compatible endpoint).
    pub llm_backend: String,
    pub llm_endpoint: String,
    pub llm_endpoint_model: String,
    pub llm_api_key: Option<String>,
    pub cleanup_enabled: bool,
    pub diarize_enabled: bool,
    pub summary_enabled: bool,
    pub default_template_id: String,
    pub share_port: u16,
    /// .ics URLs (Google "secret address", Outlook "publish calendar") or local files.
    pub calendar_sources: Vec<String>,
    pub calendar_refresh_min: u64,
    /// POST each finished meeting (summary + transcript) here. Off unless enabled.
    pub webhook_url: String,
    pub webhook_enabled: bool,
    /// Retained for settings compatibility; local Bazza builds have no update channel.
    pub auto_update_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            onboarded: false,
            user_name: "Me".into(),
            language: None,
            mic_device: None,
            loopback_device: None,
            capture_system: true,
            video_enabled: true,
            video_target: VideoTarget::default(),
            video_fps: 15,
            video_max_height: 1080,
            tier_override: None,
            live_asr_model: None,
            quality_asr_model: None,
            stt_provider: bazza_asr::provider::SttProvider::Local,
            soniox_model: bazza_asr::soniox::DEFAULT_MODEL.into(),
            soniox_consent: false,
            llm_model: None,
            hotkey: "CmdOrCtrl+Shift+R".into(),
            detection_enabled: true,
            audio_activity_enabled: true,
            never_apps: Vec::new(),
            popup_timeout_secs: 30,
            close_to_tray: true,
            llm_backend: "bundled".into(),
            llm_endpoint: "http://localhost:11434/v1".into(),
            llm_endpoint_model: "qwen3:8b".into(),
            llm_api_key: None,
            cleanup_enabled: false,
            diarize_enabled: true,
            summary_enabled: false,
            default_template_id: "general".into(),
            share_port: 47123,
            calendar_sources: Vec::new(),
            calendar_refresh_min: 15,
            webhook_url: String::new(),
            webhook_enabled: false,
            auto_update_check: false,
        }
    }
}

impl Settings {
    /// Validate before changing persisted or in-memory settings. Height must be
    /// even for the existing yuv420p encoder; capture defaults stay 15 FPS/1080p.
    pub fn validate(&self) -> Result<(), crate::ui_message::UiMessage> {
        if self.soniox_model.trim().is_empty()
            || self.soniox_model.len() > 128
            || !self.soniox_model.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(crate::ui_message::app("native.sonioxModel", "Soniox model must be a model identifier of at most 128 characters."));
        }
        if self.stt_provider == bazza_asr::provider::SttProvider::Soniox && !self.soniox_consent {
            return Err(crate::ui_message::app("native.sonioxConsent", "Enable Soniox audio processing consent before selecting cloud STT."));
        }
        if !(1..=60).contains(&self.video_fps) {
            return Err(crate::ui_message::app("native.videoFps", "Video frame rate must be between 1 and 60 FPS."));
        }
        if !(144..=4320).contains(&self.video_max_height) || !self.video_max_height.is_multiple_of(2) {
            return Err(crate::ui_message::app("native.videoHeight", "Video maximum height must be an even number between 144 and 4320 pixels."));
        }
        Ok(())
    }

    pub fn load(store: &Store) -> Settings {
        store.get_setting::<Settings>(SETTINGS_KEY).ok().flatten().unwrap_or_default()
    }

    pub fn save(&self, store: &Store) -> bazza_store::Result<()> {
        store.set_setting(SETTINGS_KEY, self)
    }

    pub(crate) fn initialize_portable(store: &Store) -> bazza_store::Result<()> {
        if store.get_setting::<Settings>(SETTINGS_KEY)?.is_none() {
            let model = "whisper-large-v3-turbo";
            Self {
                live_asr_model: Some(model.into()),
                quality_asr_model: Some(model.into()),
                ..Self::default()
            }.save(store)?;
        }
        Ok(())
    }

    /// Effective tier: explicit override or hardware-selected. A build without
    /// CUDA/Metal cannot use a GPU, so it picks CPU-sized models even when one is present.
    pub fn tier(&self, hw: &bazza_models::Hardware) -> Tier {
        self.tier_override.unwrap_or_else(|| {
            if GPU_BUILD {
                bazza_models::select_tier(hw)
            } else {
                bazza_models::select_tier(&bazza_models::Hardware { gpu: None, ..hw.clone() })
            }
        })
    }

    /// Model ids to use, falling back to the tier defaults.
    pub fn model_ids(&self, catalog: &bazza_models::Catalog, tier: Tier) -> (String, String, String) {
        let t = catalog.tier_models(tier);
        (
            self.live_asr_model.clone().unwrap_or_else(|| t.asr_live.clone()),
            self.quality_asr_model.clone().unwrap_or_else(|| t.asr_quality.clone()),
            self.llm_model.clone().unwrap_or_else(|| t.llm.clone()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_keep_capture_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"capture_system":false}"#).unwrap();
        assert!(!settings.capture_system);
        assert_eq!(settings.video_fps, 15);
        assert_eq!(settings.video_max_height, 1080);
        assert_eq!(settings.stt_provider, bazza_asr::provider::SttProvider::Local);
        assert!(!settings.soniox_consent);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn video_quality_roundtrip() {
        let settings = Settings { video_fps: 30, video_max_height: 720, ..Settings::default() };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), settings);
    }

    #[test]
    fn validates_video_settings_before_capture() {
        for (fps, height) in [(1, 144), (15, 1080), (60, 4320)] {
            assert!(Settings { video_fps: fps, video_max_height: height, ..Settings::default() }.validate().is_ok());
        }
        for fps in [0, 61, u32::MAX] {
            assert!(Settings { video_fps: fps, ..Settings::default() }.validate().is_err());
        }
        for height in [0, 142, 719, 4322, u32::MAX] {
            assert!(Settings { video_max_height: height, ..Settings::default() }.validate().is_err());
        }
    }

    #[test]
    fn cloud_stt_requires_explicit_consent_and_a_model_identifier() {
        let mut settings = Settings { stt_provider: bazza_asr::provider::SttProvider::Soniox, ..Settings::default() };
        assert!(settings.validate().is_err());
        settings.soniox_consent = true;
        assert!(settings.validate().is_ok());
        settings.soniox_model = "invalid model\n".into();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn service_configuration_preserves_native_wire_defaults_and_all_fields() {
        use bazza_service_client::api::configuration::NativeSettings;
        let default = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(default.as_object().unwrap().len(), 37);
        assert_eq!(default, serde_json::to_value(NativeSettings::default()).unwrap());
        for saved in [
            serde_json::json!({}),
            serde_json::json!({"capture_system":false,"unknown_future_field":{"keep":true}}),
            serde_json::json!({
                "onboarded":true,"user_name":"Ирина","language":null,
                "mic_device":"Microphone","loopback_device":"Speakers",
                "capture_system":false,"audio_scope":"all","video_enabled":false,
                "video_target":{"kind":"window","title":"Встреча"},
                "video_fps":30,"video_max_height":720,"tier_override":"gpu",
                "live_asr_model":"live","quality_asr_model":"quality",
                "stt_provider":"soniox","soniox_model":"stt-rt-v5","soniox_consent":true,
                "llm_model":"model","hotkey":"Ctrl+Shift+F9","detection_enabled":false,
                "audio_activity_enabled":false,"never_apps":["app"],"popup_timeout_secs":0,
                "close_to_tray":false,"llm_backend":"legacy-provider",
                "llm_endpoint":"http://localhost:1234/v1","llm_endpoint_model":"endpoint-model",
                "llm_api_key":"synthetic-key","cleanup_enabled":false,
                "diarize_enabled":false,"summary_enabled":false,"default_template_id":"custom",
                "share_port":12345,"calendar_sources":["https://example.invalid/private.ics"],
                "calendar_refresh_min":20,"webhook_url":"https://example.invalid/hook",
                "webhook_enabled":true,"auto_update_check":true
            }),
        ] {
            let native: Settings = serde_json::from_value(saved.clone()).unwrap();
            let wire: NativeSettings = serde_json::from_value(saved).unwrap();
            assert_eq!(serde_json::to_value(&native).unwrap(), serde_json::to_value(&wire).unwrap());
            assert_eq!(native.validate().is_ok(), wire.validate().is_ok());
            assert_eq!(serde_json::from_value::<Settings>(serde_json::to_value(wire).unwrap()).unwrap(), native);
        }
    }

    #[test]
    fn service_configuration_validation_matches_native_recording_boundaries() {
        use bazza_service_client::api::configuration::NativeSettings;
        let cases = [
            serde_json::json!({"stt_provider":"soniox","soniox_consent":false}),
            serde_json::json!({"stt_provider":"soniox","soniox_consent":true}),
            serde_json::json!({"soniox_model":"bad model"}),
            serde_json::json!({"soniox_model":""}),
            serde_json::json!({"soniox_model":"x".repeat(129)}),
            serde_json::json!({"video_fps":0}),
            serde_json::json!({"video_fps":1}),
            serde_json::json!({"video_fps":60}),
            serde_json::json!({"video_fps":61}),
            serde_json::json!({"video_max_height":142}),
            serde_json::json!({"video_max_height":144}),
            serde_json::json!({"video_max_height":719}),
            serde_json::json!({"video_max_height":4320}),
            serde_json::json!({"video_max_height":4322}),
        ];
        for saved in cases {
            let native: Settings = serde_json::from_value(saved.clone()).unwrap();
            let wire: NativeSettings = serde_json::from_value(saved).unwrap();
            assert_eq!(native.validate().is_ok(), wire.validate().is_ok());
        }
    }
}

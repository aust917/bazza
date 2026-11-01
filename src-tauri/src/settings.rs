use hark_capture::VideoTarget;
use hark_models::Tier;
use hark_store::Store;
use serde::{Deserialize, Serialize};

/// True when the binary was compiled with a GPU backend.
pub const GPU_BUILD: bool = cfg!(any(feature = "cuda", feature = "metal"));

pub const SETTINGS_KEY: &str = "settings";

/// The popup keeps its existing per-app preference until the user changes it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioScope {
    #[default]
    App,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub onboarded: bool,
    pub user_name: String,
    pub language: Option<String>,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub capture_system: bool,
    pub audio_scope: AudioScope,
    pub video_enabled: bool,
    pub video_target: VideoTarget,
    pub video_fps: u32,
    pub video_max_height: u32,
    pub tier_override: Option<Tier>,
    pub live_asr_model: Option<String>,
    pub quality_asr_model: Option<String>,
    pub stt_provider: hark_asr::provider::SttProvider,
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
    /// Ask GitHub for a newer release on startup (only the release manifest is fetched).
    pub auto_update_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            onboarded: false,
            user_name: "Me".into(),
            language: Some("en".into()),
            mic_device: None,
            loopback_device: None,
            capture_system: true,
            audio_scope: AudioScope::App,
            video_enabled: true,
            video_target: VideoTarget::default(),
            video_fps: 15,
            video_max_height: 1080,
            tier_override: None,
            live_asr_model: None,
            quality_asr_model: None,
            stt_provider: hark_asr::provider::SttProvider::Local,
            soniox_model: hark_asr::soniox::DEFAULT_MODEL.into(),
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
            cleanup_enabled: true,
            diarize_enabled: true,
            summary_enabled: true,
            default_template_id: "general".into(),
            share_port: 47123,
            calendar_sources: Vec::new(),
            calendar_refresh_min: 15,
            webhook_url: String::new(),
            webhook_enabled: false,
            auto_update_check: true,
        }
    }
}

impl Settings {
    /// Validate before changing persisted or in-memory settings. Height must be
    /// even for the existing yuv420p encoder; capture defaults stay 15 FPS/1080p.
    pub fn validate(&self) -> Result<(), String> {
        if self.soniox_model.trim().is_empty()
            || self.soniox_model.len() > 128
            || !self.soniox_model.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("Soniox model must be a model identifier of at most 128 characters.".into());
        }
        if self.stt_provider == hark_asr::provider::SttProvider::Soniox && !self.soniox_consent {
            return Err("Enable Soniox audio processing consent before selecting cloud STT.".into());
        }
        if !(1..=60).contains(&self.video_fps) {
            return Err("Video frame rate must be between 1 and 60 FPS.".into());
        }
        if !(144..=4320).contains(&self.video_max_height) || !self.video_max_height.is_multiple_of(2) {
            return Err("Video maximum height must be an even number between 144 and 4320 pixels.".into());
        }
        Ok(())
    }

    pub fn load(store: &Store) -> Settings {
        store.get_setting::<Settings>(SETTINGS_KEY).ok().flatten().unwrap_or_default()
    }

    pub fn save(&self, store: &Store) -> hark_store::Result<()> {
        store.set_setting(SETTINGS_KEY, self)
    }

    /// Effective tier: explicit override or hardware-selected. A build without
    /// CUDA/Metal cannot use a GPU, so it picks CPU-sized models even when one is present.
    pub fn tier(&self, hw: &hark_models::Hardware) -> Tier {
        self.tier_override.unwrap_or_else(|| {
            if GPU_BUILD {
                hark_models::select_tier(hw)
            } else {
                hark_models::select_tier(&hark_models::Hardware { gpu: None, ..hw.clone() })
            }
        })
    }

    /// Model ids to use, falling back to the tier defaults.
    pub fn model_ids(&self, catalog: &hark_models::Catalog, tier: Tier) -> (String, String, String) {
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
        assert_eq!(settings.audio_scope, AudioScope::App);
        assert_eq!(settings.video_fps, 15);
        assert_eq!(settings.video_max_height, 1080);
        assert_eq!(settings.stt_provider, hark_asr::provider::SttProvider::Local);
        assert!(!settings.soniox_consent);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn audio_scope_and_video_quality_roundtrip() {
        let settings = Settings { audio_scope: AudioScope::All, video_fps: 30, video_max_height: 720, ..Settings::default() };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["audio_scope"], "all");
        assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), settings);
        assert!(serde_json::from_str::<Settings>(r#"{"audio_scope":"unknown"}"#).is_err());
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
        let mut settings = Settings { stt_provider: hark_asr::provider::SttProvider::Soniox, ..Settings::default() };
        assert!(settings.validate().is_err());
        settings.soniox_consent = true;
        assert!(settings.validate().is_ok());
        settings.soniox_model = "invalid model\n".into();
        assert!(settings.validate().is_err());
    }
}

//! Private native configuration values, independent of capture/model engines.
//! These preserve the existing Settings JSON; they are not public MCP grants.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum NativeVideoTarget {
    Monitor { index: u32 },
    Window { title: String },
}

impl Default for NativeVideoTarget {
    fn default() -> Self {
        Self::Monitor { index: 0 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeTier {
    CpuLow,
    CpuHigh,
    Gpu,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeSttProvider {
    #[default]
    Local,
    Soniox,
}

/// The native application's settings document. Missing fields use defaults and
/// unknown fields, including the retired `audio_scope`, are ignored.
/// Credentials and private integration URLs retain their existing wire shape.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NativeSettings {
    pub onboarded: bool,
    pub user_name: String,
    pub language: Option<String>,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub capture_system: bool,
    pub video_enabled: bool,
    pub video_target: NativeVideoTarget,
    pub video_fps: u32,
    pub video_max_height: u32,
    pub tier_override: Option<NativeTier>,
    pub live_asr_model: Option<String>,
    pub quality_asr_model: Option<String>,
    pub stt_provider: NativeSttProvider,
    pub soniox_model: String,
    pub soniox_consent: bool,
    pub llm_model: Option<String>,
    pub hotkey: String,
    pub detection_enabled: bool,
    pub audio_activity_enabled: bool,
    pub never_apps: Vec<String>,
    pub popup_timeout_secs: u64,
    pub close_to_tray: bool,
    /// Keep String: unknown legacy backends use the native bundled fallback.
    pub llm_backend: String,
    pub llm_endpoint: String,
    pub llm_endpoint_model: String,
    pub llm_api_key: Option<String>,
    pub cleanup_enabled: bool,
    pub diarize_enabled: bool,
    pub summary_enabled: bool,
    pub default_template_id: String,
    pub share_port: u16,
    pub calendar_sources: Vec<String>,
    pub calendar_refresh_min: u64,
    pub webhook_url: String,
    pub webhook_enabled: bool,
    pub auto_update_check: bool,
}

impl Default for NativeSettings {
    fn default() -> Self {
        Self {
            onboarded: false,
            user_name: "Me".into(),
            language: None,
            mic_device: None,
            loopback_device: None,
            capture_system: true,
            video_enabled: true,
            video_target: NativeVideoTarget::default(),
            video_fps: 15,
            video_max_height: 1080,
            tier_override: None,
            live_asr_model: None,
            quality_asr_model: None,
            stt_provider: NativeSttProvider::Local,
            soniox_model: "stt-rt-v5".into(),
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

impl fmt::Debug for NativeSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Do not print arbitrary user strings: endpoint userinfo/query tokens,
        // calendar secret addresses, local paths and credentials are private.
        // This changes diagnostics only; serde deliberately remains compatible.
        formatter
            .debug_struct("NativeSettings")
            .field("stt_provider", &self.stt_provider)
            .field("soniox_consent", &self.soniox_consent)
            .field("video_fps", &self.video_fps)
            .field("video_max_height", &self.video_max_height)
            .field(
                "llm_api_key",
                &self.llm_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("llm_endpoint", &"[redacted]")
            .field("calendar_sources", &"[redacted]")
            .field("webhook_url", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// Stable, input-free validation codes. Native orchestration maps these to its
/// localized UI messages; this crate never imports Tauri or native engines.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum NativeSettingsValidationCode {
    #[error("soniox_model")]
    SonioxModel,
    #[error("soniox_consent")]
    SonioxConsent,
    #[error("video_fps")]
    VideoFps,
    #[error("video_height")]
    VideoHeight,
}

impl NativeSettings {
    /// Preserve only the native Settings validation rules. In particular,
    /// credential presence, backend names and network policy are not validated
    /// here, and this method performs no cancellation or permission mutation.
    pub fn validate(&self) -> Result<(), NativeSettingsValidationCode> {
        if self.soniox_model.trim().is_empty()
            || self.soniox_model.len() > 128
            || !self.soniox_model.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            })
        {
            return Err(NativeSettingsValidationCode::SonioxModel);
        }
        if self.stt_provider == NativeSttProvider::Soniox && !self.soniox_consent {
            return Err(NativeSettingsValidationCode::SonioxConsent);
        }
        if !(1..=60).contains(&self.video_fps) {
            return Err(NativeSettingsValidationCode::VideoFps);
        }
        if !(144..=4320).contains(&self.video_max_height)
            || !self.video_max_height.is_multiple_of(2)
        {
            return Err(NativeSettingsValidationCode::VideoHeight);
        }
        Ok(())
    }
}

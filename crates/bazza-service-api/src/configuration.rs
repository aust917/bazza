//! Closed private native configuration. Never a public MCP settings/secret API.
//! This does not implement cloud permission epochs or cancellation acknowledgments.
use crate::desktop::{desktop_body_fits, desktop_revision};
pub use bazza_domain::{NativeSettings, NativeSettingsValidationCode, NativeSttProvider, NativeTier, NativeVideoTarget};
use serde::{Deserialize, Serialize};

pub const MAX_NATIVE_CONFIGURATION_DATA_BYTES: usize = 65_024;
pub const MAX_NATIVE_CONFIGURATION_STORED_BYTES: usize = 65_536;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NativeConfiguration {
    pub revision: String,
    pub settings: NativeSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateNativeConfigurationRequest {
    pub expected_revision: String,
    pub idempotency_key: String,
    pub settings: NativeSettings,
}

pub fn native_configuration_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl NativeConfiguration {
    pub fn valid(&self) -> bool {
        // Reads may expose validly typed legacy settings that need repair under
        // native validation. Never silently replace those values with defaults.
        native_configuration_revision(&self.revision)
            && desktop_body_fits(self, MAX_NATIVE_CONFIGURATION_DATA_BYTES)
    }
}
impl UpdateNativeConfigurationRequest {
    pub fn valid(&self) -> bool {
        native_configuration_revision(&self.expected_revision)
            && desktop_revision(&self.idempotency_key)
            && self.settings.validate().is_ok()
            // Fixture/private constraint: the same stored hotkey is consumed
            // by bounded desktop bootstrap. This is not a native validation rule.
            && !self.settings.hotkey.is_empty()
            && self.settings.hotkey.len() <= 256
            && !self.settings.hotkey.contains('\0')
            && desktop_body_fits(self, MAX_NATIVE_CONFIGURATION_DATA_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_private_request_rejects_arbitrary_authority_and_bounds_escaped_json() {
        let request = UpdateNativeConfigurationRequest {
            expected_revision: "a".repeat(64),
            idempotency_key: "configuration-1".into(),
            settings: NativeSettings::default(),
        };
        assert!(request.valid());
        for field in ["key", "sql", "path", "principal", "permission_epoch"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = "not accepted".into();
            assert!(serde_json::from_value::<UpdateNativeConfigurationRequest>(value).is_err());
        }
        let mut large = request.clone();
        large.settings.user_name = "\0".repeat(12_000);
        assert!(!large.valid());
        let mut diagnostic = request;
        diagnostic.settings.llm_api_key = Some("synthetic-private-token".into());
        assert!(!format!("{diagnostic:?}").contains("synthetic-private-token"));
    }

    #[test]
    fn private_write_hotkeys_preserve_the_narrow_bootstrap_contract_without_changing_native_rules()
    {
        let oversized = format!("{}a", "я".repeat(128));
        assert_eq!(oversized.len(), 257);
        for hotkey in [String::new(), oversized, "Ctrl\0+R".into()] {
            let settings = NativeSettings {
                hotkey,
                ..NativeSettings::default()
            };
            assert!(settings.validate().is_ok());
            let legacy = NativeConfiguration {
                revision: "a".repeat(64),
                settings: settings.clone(),
            };
            assert!(
                legacy.valid(),
                "validly typed legacy reads remain representable"
            );
            let request = UpdateNativeConfigurationRequest {
                expected_revision: "a".repeat(64),
                idempotency_key: "private-hotkey".into(),
                settings,
            };
            assert!(!request.valid());
        }
        let boundary = UpdateNativeConfigurationRequest {
            expected_revision: "a".repeat(64),
            idempotency_key: "private-boundary".into(),
            settings: NativeSettings {
                hotkey: "я".repeat(128),
                ..NativeSettings::default()
            },
        };
        assert!(boundary.valid());
    }
}

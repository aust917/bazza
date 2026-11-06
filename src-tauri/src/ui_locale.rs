//! UI language is independent of transcription, recording settings and meeting data.
use bazza_store::Store;
use serde::{Deserialize, Serialize};

pub const SETTINGS_KEY: &str = "ui_locale";

pub fn commit<E>(current: &mut UiLocale, next: UiLocale, save: impl FnOnce(UiLocale) -> Result<(), E>) -> Result<(), E> {
    save(next)?;
    *current = next;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiLocale {
    Ru,
    En,
}

impl UiLocale {
    pub fn load(store: &Store) -> Self {
        match store.get_setting::<Self>(SETTINGS_KEY) {
            Ok(Some(locale)) => locale,
            Ok(None) => Self::Ru,
            Err(error) => {
                log::warn!("UI locale unavailable; falling back to English: {error}");
                Self::En
            }
        }
    }

    pub fn save(self, store: &Store) -> bazza_store::Result<()> {
        store.set_setting(SETTINGS_KEY, &self)
    }

    pub fn text(self, ru: &'static str, en: &'static str) -> &'static str {
        match self { Self::Ru => ru, Self::En => en }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_locale_survives_restart_without_rewriting_capture_or_stt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let settings = serde_json::json!({ "language": "ja", "video_fps": 30, "capture_system": false });
        {
            let store = Store::open(&path).unwrap();
            store.set_setting("settings", &settings).unwrap();
            assert_eq!(UiLocale::load(&store), UiLocale::Ru);
            UiLocale::En.save(&store).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(UiLocale::load(&store), UiLocale::En);
        assert_eq!(store.get_setting::<serde_json::Value>("settings").unwrap(), Some(settings));
        UiLocale::Ru.save(&store).unwrap();
        assert_eq!(UiLocale::load(&store), UiLocale::Ru);
    }

    #[test]
    fn malformed_saved_locale_uses_english_and_rejects_unsupported_input() {
        let store = Store::open_in_memory().unwrap();
        for invalid in [serde_json::json!("de"), serde_json::json!(null), serde_json::json!({"locale":"ru"})] {
            store.set_setting(SETTINGS_KEY, &invalid).unwrap();
            assert_eq!(UiLocale::load(&store), UiLocale::En);
        }
        assert!(serde_json::from_str::<UiLocale>("\"de\"").is_err());
    }

    #[test]
    fn a_failed_save_keeps_the_previous_in_memory_language() {
        let mut locale = UiLocale::En;
        let result = commit(&mut locale, UiLocale::Ru, |_| Err("controlled disk failure"));
        assert_eq!(result, Err("controlled disk failure"));
        assert_eq!(locale, UiLocale::En);
    }
}

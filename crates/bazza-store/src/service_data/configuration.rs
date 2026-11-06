//! Closed private native Settings persistence. No arbitrary keys, credentials
//! endpoint, worker cancellation, network policy changes or native side effects.
use super::*;
use bazza_domain::{NativeSettings, NativeSettingsValidationCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const STORED_BYTES: usize = 65_536;
const DATA_BYTES: usize = 65_024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NativeConfiguration {
    pub revision: String,
    pub settings: NativeSettings,
}

fn fits<T: Serialize>(value: &T, max: usize) -> bool {
    struct Budget {
        used: usize,
        max: usize,
    }
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|n| *n <= self.max)
                .ok_or_else(|| std::io::Error::other("native configuration budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget { used: 0, max }, value).is_ok()
}

fn validation(error: NativeSettingsValidationCode) -> DataError {
    use NativeSettingsValidationCode::*;
    DataError::Invalid(match error {
        SonioxModel => "soniox_model",
        SonioxConsent => "soniox_consent",
        VideoFps => "video_fps",
        VideoHeight => "video_height",
    })
}

fn state_revision(conn: &Connection) -> DataResult<String> {
    let revision: Option<String> = conn.query_row(
        "SELECT CASE WHEN length(CAST(revision AS BLOB)) BETWEEN 1 AND 128 THEN revision END FROM service_desktop_state WHERE singleton=1",
        [], |row| row.get(0))?;
    let revision = revision.ok_or(DataError::Invalid("desktop revision"))?;
    if !revision
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return Err(DataError::Invalid("desktop revision"));
    }
    Ok(revision)
}

struct Stored {
    raw: Option<String>,
    object: serde_json::Map<String, serde_json::Value>,
    settings: NativeSettings,
}

fn stored_on(conn: &Connection) -> DataResult<Stored> {
    // Check BLOB bytes in SQLite before materializing any legacy body in Rust.
    let raw: Option<Option<String>> = conn.query_row(
        "SELECT CASE WHEN length(CAST(value AS BLOB))<=65536 THEN value END FROM settings WHERE key='settings'",
        [], |row| row.get(0)).optional()?;
    match raw {
        None => Ok(Stored {
            raw: None,
            object: serde_json::Map::new(),
            settings: NativeSettings::default(),
        }),
        Some(None) => Err(DataError::Invalid("native configuration budget")),
        Some(Some(raw)) => {
            let settings = serde_json::from_str(&raw)
                .map_err(|_| DataError::Invalid("malformed native settings"))?;
            let object = match serde_json::from_str(&raw) {
                Ok(serde_json::Value::Object(object)) => object,
                _ => return Err(DataError::Invalid("malformed native settings")),
            };
            Ok(Stored {
                raw: Some(raw),
                object,
                settings,
            })
        }
    }
}

pub(super) fn soniox_selection_on(conn: &Connection) -> DataResult<Option<CaptureSelection>> {
    let settings = stored_on(conn)?.settings;
    Ok(
        (settings.stt_provider == bazza_domain::NativeSttProvider::Soniox
            && settings.soniox_consent)
            .then_some(CaptureSelection {
                provider: CaptureProvider::Soniox,
                model: Some(settings.soniox_model),
                language: settings.language,
            }),
    )
}

fn configuration_revision(shared: &str, raw: Option<&str>) -> String {
    let mut hash = Sha256::new();
    hash.update(b"bazza-private-native-configuration-v1\0");
    hash.update(shared.as_bytes());
    hash.update([0, u8::from(raw.is_some())]);
    if let Some(raw) = raw {
        hash.update(raw.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn snapshot_on(conn: &Connection) -> DataResult<NativeConfiguration> {
    let stored = stored_on(conn)?;
    let result = NativeConfiguration {
        revision: configuration_revision(&state_revision(conn)?, stored.raw.as_deref()),
        settings: stored.settings,
    };
    if !fits(&result, DATA_BYTES) {
        return Err(DataError::Invalid("native configuration reply budget"));
    }
    Ok(result)
}

impl crate::Store {
    /// Private desktop-only read. No initialization, rewriting malformed legacy
    /// blobs, generic key selection or credential-presence side effects.
    pub fn service_native_configuration(&self) -> DataResult<NativeConfiguration> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        snapshot_on(&tx)
    }

    /// Temporarily fixture-only: provider/consent must remain unchanged until
    /// the permission-epoch/cloud cancellation ACK boundary is implemented.
    /// Unrelated edits preserve them, unknown JSON fields and other setting keys.
    pub fn service_update_native_configuration(
        &self,
        ctx: &MutationContext,
        expected: &str,
        settings: &NativeSettings,
    ) -> DataResult<MutationOutcome> {
        if expected.len() != 64
            || !expected
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DataError::Invalid("native configuration revision"));
        }
        settings.validate().map_err(validation)?;
        // Private fixture writes must remain readable by narrow desktop
        // bootstrap. Preserve the native four-rule validator and legacy reads.
        bounded_text(&settings.hotkey, 1, 256)?;
        if !fits(&(expected, &ctx.idempotency_key, settings), DATA_BYTES) {
            return Err(DataError::Invalid("native configuration request budget"));
        }
        consistency::mutate(
            self,
            ctx,
            "update_native_configuration",
            &(expected, settings),
            |tx| {
                let mut stored = stored_on(tx)?;
                let old_revision =
                    configuration_revision(&state_revision(tx)?, stored.raw.as_deref());
                check_revision(&old_revision, expected)?;
                let serde_json::Value::Object(known) = serde_json::to_value(settings)? else {
                    return Err(DataError::Invalid("native settings object"));
                };
                // Only fixed, domain-owned fields are accepted from the caller;
                // unknown legacy fields remain exact JSON values in this bounded map.
                stored.object.extend(known);
                if !fits(&stored.object, STORED_BYTES) {
                    return Err(DataError::Invalid("native configuration budget"));
                }
                let raw = serde_json::to_string(&stored.object)?;
                let shared_revision = revision();
                let next = NativeConfiguration {
                    revision: configuration_revision(&shared_revision, Some(&raw)),
                    settings: settings.clone(),
                };
                if !fits(&next, DATA_BYTES) {
                    return Err(DataError::Invalid("native configuration reply budget"));
                }
                tx.execute("INSERT INTO settings(key,value) VALUES('settings',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [&raw])?;
                tx.execute(
                    "UPDATE service_desktop_state SET revision=?1 WHERE singleton=1",
                    [&shared_revision],
                )?;
                // Receipt/change feed contain only revision metadata, never settings.
                consistency::outcome(
                    tx,
                    "native_configuration",
                    "native_configuration",
                    &next.revision,
                    ctx,
                )
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use bazza_domain::NativeSttProvider;
    use serde_json::json;

    fn ctx(key: &str) -> MutationContext {
        MutationContext {
            principal: "private-gui".into(),
            idempotency_key: key.into(),
        }
    }
    fn raw(store: &Store) -> Option<String> {
        store
            .conn
            .lock()
            .query_row(
                "SELECT value FROM settings WHERE key='settings'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    #[test]
    fn absent_read_is_pure_and_merged_update_preserves_unknown_fields_and_private_values() {
        let store = Store::open_in_memory().unwrap();
        let defaults = store.service_native_configuration().unwrap();
        assert_eq!(defaults.settings, NativeSettings::default());
        assert!(raw(&store).is_none());
        let legacy = json!({"future":{"token":"private-unknown"},"llm_api_key":"private-llm","capture_system":false});
        store.set_setting("settings", &legacy).unwrap();
        store
            .set_setting("private:soniox_api_key", &"private-soniox")
            .unwrap();
        let original = raw(&store);
        let before = store.service_native_configuration().unwrap();
        assert!(!before.settings.capture_system);
        assert_eq!(before.settings.language, None);
        assert_eq!(raw(&store), original);
        let mut next = before.settings;
        next.video_fps = 30;
        store
            .service_update_native_configuration(&ctx("merge"), &before.revision, &next)
            .unwrap();
        let merged: serde_json::Value = store.get_setting("settings").unwrap().unwrap();
        assert_eq!(merged["future"], legacy["future"]);
        assert_eq!(merged["llm_api_key"], "private-llm");
        assert_eq!(
            store
                .get_setting::<String>("private:soniox_api_key")
                .unwrap()
                .as_deref(),
            Some("private-soniox")
        );
        assert_eq!(store.service_native_configuration().unwrap().settings, next);
    }

    #[test]
    fn full_and_narrow_preferences_invalidate_each_other_and_locale_is_conservative() {
        let store = Store::open_in_memory().unwrap();
        let full = store.service_native_configuration().unwrap();
        let narrow = store.service_desktop_preferences().unwrap();
        let mut next = full.settings;
        next.close_to_tray = false;
        store
            .service_update_native_configuration(&ctx("full"), &full.revision, &next)
            .unwrap();
        assert!(matches!(
            store.service_update_desktop_preferences(
                &ctx("stale-narrow"),
                &narrow.revision,
                &DesktopPreferencesPatch {
                    hotkey: Some("Ctrl+F8".into()),
                    ..Default::default()
                }
            ),
            Err(DataError::Conflict { .. })
        ));
        let current = store.service_native_configuration().unwrap();
        let narrow = store.service_desktop_preferences().unwrap();
        store
            .service_update_desktop_preferences(
                &ctx("narrow"),
                &narrow.revision,
                &DesktopPreferencesPatch {
                    hotkey: Some("Ctrl+F8".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(matches!(
            store.service_update_native_configuration(
                &ctx("stale-full"),
                &current.revision,
                &current.settings
            ),
            Err(DataError::Conflict { .. })
        ));
        let current = store.service_native_configuration().unwrap();
        let narrow = store.service_desktop_preferences().unwrap();
        store
            .service_update_desktop_preferences(
                &ctx("locale"),
                &narrow.revision,
                &DesktopPreferencesPatch {
                    ui_locale: Some(DesktopLocale::En),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_ne!(
            store.service_native_configuration().unwrap().revision,
            current.revision
        );
    }

    #[test]
    fn legacy_blob_writes_including_unknown_fields_and_whitespace_invalidate_cas() {
        let store = Store::open_in_memory().unwrap();
        let first = store.service_native_configuration().unwrap();
        store.set_setting("settings", &json!({"future":1})).unwrap();
        assert!(matches!(
            store.service_update_native_configuration(
                &ctx("stale"),
                &first.revision,
                &first.settings
            ),
            Err(DataError::Conflict { .. })
        ));
        let second = store.service_native_configuration().unwrap();
        store
            .conn
            .lock()
            .execute("UPDATE settings SET value=' {\"future\":1} '", [])
            .unwrap();
        assert_ne!(
            store.service_native_configuration().unwrap().revision,
            second.revision
        );
    }

    #[test]
    fn idempotent_receipt_survives_other_changes_without_storing_secret_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.db");
        let store = Store::open(&path).unwrap();
        let first = store.service_native_configuration().unwrap();
        let mut next = first.settings;
        next.llm_api_key = Some("private-credential-marker".into());
        let receipt = store
            .service_update_native_configuration(&ctx("once"), &first.revision, &next)
            .unwrap();
        let current = store.service_native_configuration().unwrap();
        let mut other = current.settings;
        other.video_fps = 30;
        store
            .service_update_native_configuration(&ctx("later"), &current.revision, &other)
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let replay = store
            .service_update_native_configuration(&ctx("once"), &first.revision, &next)
            .unwrap();
        assert_eq!(receipt, replay);
        assert_eq!(
            store
                .service_native_configuration()
                .unwrap()
                .settings
                .video_fps,
            30
        );
        let bodies: Vec<String> = store
            .conn
            .lock()
            .prepare("SELECT outcome_json FROM service_idempotency")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(bodies
            .iter()
            .all(|body| !body.contains("private-credential-marker")));
        let mut changed = next;
        changed.video_fps = 60;
        assert!(matches!(
            store.service_update_native_configuration(&ctx("once"), &first.revision, &changed),
            Err(DataError::IdempotencyConflict)
        ));
    }

    #[test]
    fn malformed_oversized_and_invalid_inputs_never_rewrite_legacy_data() {
        let store = Store::open_in_memory().unwrap();
        let first = store.service_native_configuration().unwrap();
        for body in [
            "not-json".to_owned(),
            "[]".to_owned(),
            "{\"video_fps\":\"wrong\"}".to_owned(),
            format!("{{\"future\":\"{}\"}}", "x".repeat(65_536)),
        ] {
            store.conn.lock().execute("INSERT INTO settings(key,value) VALUES('settings',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[&body]).unwrap();
            assert!(store.service_native_configuration().is_err());
            assert!(store
                .service_update_native_configuration(
                    &ctx("invalid-legacy"),
                    &first.revision,
                    &first.settings
                )
                .is_err());
            assert_eq!(raw(&store).as_deref(), Some(body.as_str()));
        }
        store.set_setting("settings", &json!({})).unwrap();
        let before = store.service_native_configuration().unwrap();
        let mut invalid = before.settings.clone();
        invalid.video_fps = 0;
        assert!(matches!(
            store.service_update_native_configuration(
                &ctx("invalid-fps"),
                &before.revision,
                &invalid
            ),
            Err(DataError::Invalid("video_fps"))
        ));
        invalid = before.settings.clone();
        invalid.user_name = "\0".repeat(12_000);
        assert!(store
            .service_update_native_configuration(
                &ctx("oversize-request"),
                &before.revision,
                &invalid
            )
            .is_err());
        assert_eq!(store.service_native_configuration().unwrap(), before);
        store
            .set_setting("settings", &json!({"future":"x".repeat(65_000)}))
            .unwrap();
        let before = store.service_native_configuration().unwrap();
        let original = raw(&store);
        assert!(store
            .service_update_native_configuration(
                &ctx("merge-budget"),
                &before.revision,
                &before.settings
            )
            .is_err());
        assert_eq!(raw(&store), original);
    }

    #[test]
    fn desktop_provider_changes_rotate_cloud_permission_and_other_edits_do_not() {
        let store = Store::open_in_memory().unwrap();
        let authority = || store.conn.lock().query_row(
            "SELECT permission_epoch FROM service_soniox_authority WHERE singleton=1",
            [],
            |row| row.get::<_, String>(0),
        ).unwrap();
        let initial_authority = authority();
        let before = store.service_native_configuration().unwrap();
        let mut next = before.settings.clone();
        next.soniox_consent = true;
        let consent = store.service_update_native_configuration(&ctx("consent"), &before.revision, &next).unwrap();
        let consent_authority = authority();
        assert_ne!(consent_authority, initial_authority);
        next.stt_provider = NativeSttProvider::Soniox;
        store.service_update_native_configuration(&ctx("provider"), &consent.revision, &next).unwrap();
        let provider_authority = authority();
        assert_ne!(provider_authority, consent_authority);
        let current = store.service_native_configuration().unwrap();
        let mut edit = current.settings;
        edit.video_fps = 30;
        let updated = store
            .service_update_native_configuration(&ctx("unrelated"), &current.revision, &edit)
            .unwrap();
        assert_eq!(store.service_native_configuration().unwrap().settings, edit);
        assert_eq!(authority(), provider_authority);
        edit.stt_provider = NativeSttProvider::Local;
        edit.soniox_consent = false;
        store.service_update_native_configuration(&ctx("revoke"), &updated.revision, &edit).unwrap();
        assert_ne!(authority(), provider_authority);
    }

    #[test]
    fn late_failure_rolls_back_blob_shared_revision_change_and_receipt() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_setting(
                "settings",
                &json!({"future":{"keep":true},"llm_api_key":"private-keep"}),
            )
            .unwrap();
        let before = store.service_native_configuration().unwrap();
        let narrow = store.service_desktop_preferences().unwrap();
        let original = raw(&store);
        store.conn.lock().execute_batch("CREATE TRIGGER fail_native_config BEFORE INSERT ON service_changes WHEN NEW.entity_kind='native_configuration' BEGIN SELECT RAISE(ABORT,'synthetic late failure'); END;").unwrap();
        let mut next = before.settings.clone();
        next.video_fps = 30;
        assert!(store
            .service_update_native_configuration(&ctx("rollback"), &before.revision, &next)
            .is_err());
        assert_eq!(raw(&store), original);
        assert_eq!(store.service_native_configuration().unwrap(), before);
        assert_eq!(store.service_desktop_preferences().unwrap(), narrow);
        assert_eq!(
            store
                .conn
                .lock()
                .query_row(
                    "SELECT COUNT(*) FROM service_idempotency WHERE key='rollback'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn private_hotkey_rejection_keeps_raw_blob_shared_revision_receipts_and_bootstrap_unchanged() {
        let store = Store::open_in_memory().unwrap();
        let legacy_raw = " { \"hotkey\": \"CmdOrCtrl+Shift+R\", \"future\": {\"keep\":true}, \"llm_api_key\": \"private-keep\" } ";
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO settings(key,value) VALUES('settings',?1)",
                [legacy_raw],
            )
            .unwrap();
        let before = store.service_native_configuration().unwrap();
        let narrow = store.service_desktop_preferences().unwrap();
        let bootstrap = store.service_desktop_bootstrap().unwrap();
        let oversized = format!("{}a", "я".repeat(128));
        assert_eq!(oversized.len(), 257);
        for (index, hotkey) in [String::new(), oversized, "Ctrl\0+R".into()]
            .into_iter()
            .enumerate()
        {
            let next = NativeSettings {
                hotkey,
                ..before.settings.clone()
            };
            assert!(
                next.validate().is_ok(),
                "private guard must not change native validation"
            );
            assert!(matches!(
                store.service_update_native_configuration(
                    &ctx(&format!("invalid-hotkey-{index}")),
                    &before.revision,
                    &next
                ),
                Err(DataError::Invalid(_))
            ));
            assert_eq!(raw(&store).as_deref(), Some(legacy_raw));
            assert_eq!(store.service_native_configuration().unwrap(), before);
            assert_eq!(store.service_desktop_preferences().unwrap(), narrow);
            let current = store.service_desktop_bootstrap().unwrap();
            assert_eq!(current.preferences, narrow);
            assert_eq!(current.change_sequence, bootstrap.change_sequence);
            assert_eq!(current.cursor_scope, bootstrap.cursor_scope);
            assert_eq!(
                store
                    .conn
                    .lock()
                    .query_row("SELECT COUNT(*) FROM service_idempotency", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                store
                    .conn
                    .lock()
                    .query_row("SELECT COUNT(*) FROM service_changes", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        // Reading an existing typed legacy value remains possible for repair;
        // only this new full-write boundary enforces bootstrap compatibility.
        let legacy = NativeSettings {
            hotkey: String::new(),
            ..before.settings
        };
        store.set_setting("settings", &legacy).unwrap();
        assert_eq!(
            store.service_native_configuration().unwrap().settings,
            legacy
        );
    }
}

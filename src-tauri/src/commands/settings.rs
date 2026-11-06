use super::{err, CmdResult};
use crate::settings::Settings;
use crate::shortcuts;
use crate::state::{AppState, RuntimeState};
use bazza_service_client::{api::configuration::UpdateNativeConfigurationRequest, RepositoryClient};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
pub(crate) struct ServiceSettingsOperations(tokio::sync::Mutex<()>);

#[derive(Serialize)]
pub struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_revision: Option<String>,
}

fn settings_unavailable() -> crate::ui_message::UiMessage {
    crate::ui_message::app("native.settingsUnavailable", "The library settings are unavailable. Refresh before trying again.")
}

#[tauri::command]
pub fn get_ui_locale(app: AppHandle) -> CmdResult<crate::ui_locale::UiLocale> {
    if let Some(runtime) = app.try_state::<RuntimeState>() {
        return Ok(*runtime.ui_locale.read());
    }
    let state = app.try_state::<AppState>().ok_or_else(settings_unavailable)?;
    let locale = *state.ui_locale.read();
    Ok(locale)
}

#[tauri::command]
pub fn set_ui_locale(app: AppHandle, locale: crate::ui_locale::UiLocale) -> CmdResult<crate::ui_locale::UiLocale> {
    let state = app.state::<AppState>();
    // Serialize competing writes. Publish only after persistence succeeds.
    let _update = state.ui_locale_update.lock();
    let mut current = state.ui_locale.write();
    crate::ui_locale::commit(&mut current, locale, |next| next.save(&state.store)).map_err(err)?;
    drop(current);
    crate::tray::refresh(&app);
    crate::windows::refresh_titles(&app);
    app.emit(crate::events::UI_LOCALE_CHANGED, crate::events::UiLocalePayload { locale }).map_err(err)?;
    Ok(locale)
}

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> CmdResult<SettingsView> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        let operations = app.state::<ServiceSettingsOperations>();
        let _operation = operations.0.lock().await;
        let snapshot = repository.native_configuration().await.map_err(|_| settings_unavailable())?;
        let settings: Settings = serde_json::to_value(snapshot.data.settings)
            .and_then(serde_json::from_value)
            .map_err(|_| settings_unavailable())?;
        *app.state::<RuntimeState>().settings.write() = settings.clone();
        return Ok(SettingsView { settings, service_revision: Some(snapshot.data.revision) });
    }
    let state = app.try_state::<AppState>().ok_or_else(settings_unavailable)?;
    let settings = state.settings.read().clone();
    Ok(SettingsView { settings, service_revision: None })
}

#[tauri::command]
pub async fn set_settings(app: AppHandle, new: Settings, expected_revision: Option<String>) -> CmdResult<SettingsView> {
    new.validate()?;
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        let operations = app.state::<ServiceSettingsOperations>();
        let _operation = operations.0.lock().await;
        let expected_revision = expected_revision.ok_or_else(settings_unavailable)?;
        let settings = serde_json::to_value(&new).and_then(serde_json::from_value)
            .map_err(|_| settings_unavailable())?;
        let receipt = repository.update_native_configuration(UpdateNativeConfigurationRequest {
            expected_revision,
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            settings,
        }).await.map_err(|_| settings_unavailable())?;
        *app.state::<RuntimeState>().settings.write() = new.clone();
        return Ok(SettingsView { settings: new, service_revision: Some(receipt.data.revision) });
    }
    let state = app.state::<AppState>();
    let old_hotkey = crate::stt_stage::commit_settings(&state.settings, &state.cloud_stt_cancel, new.clone(), |settings| {
        settings.save(&state.store).map_err(|error| error.to_string())
    })?;
    if new.hotkey != old_hotkey {
        shortcuts::register(&app, &new.hotkey).map_err(err)?;
    }
    Ok(SettingsView { settings: new, service_revision: None })
}

/// POST a `test` event to `url`; returns the HTTP status.
#[tauri::command]
pub async fn test_webhook(url: String) -> CmdResult<u16> {
    tauri::async_runtime::spawn_blocking(move || crate::webhook::test(&url).map_err(super::err)).await.map_err(super::err)?
}

#[derive(Serialize)]
pub struct DataInfo {
    pub data_dir: String,
    pub recordings_bytes: u64,
    pub models_bytes: u64,
}

#[tauri::command]
pub async fn data_info(app: AppHandle) -> CmdResult<DataInfo> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        let root = state.store.root();
        Ok(DataInfo {
            data_dir: root.to_string_lossy().into_owned(),
            recordings_bytes: dir_size(&root.join("recordings")),
            models_bytes: dir_size(&root.join("models")),
        })
    }).await
}

fn dir_size(p: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

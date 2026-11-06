//! Local Attendee configuration. The API key stays in the OS credential store.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use bazza_attendee::Config;
use bazza_service_client::RepositoryClient;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::{
    attendee_managed, commands::CmdResult, integration_credentials as credentials, state::AppState,
    ui_message::UiMessage,
};

const FILE_NAME: &str = "attendee-integration.json";
const CREDENTIAL_SERVICE: &str = "app.bazza.attendee";
static PROFILE: Mutex<()> = Mutex::new(());

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttendeeSettings {
    api_url: String,
    callback_url: String,
    bind_address: String,
    certificate_path: PathBuf,
    private_key_path: PathBuf,
    #[serde(default)]
    managed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttendeeSettingsStatus {
    api_url: Option<String>,
    callback_url: Option<String>,
    bind_address: Option<String>,
    certificate_path: Option<PathBuf>,
    private_key_path: Option<PathBuf>,
    configured: bool,
    managed: bool,
    setup_available: bool,
}

impl Default for AttendeeSettingsStatus {
    fn default() -> Self {
        Self {
            api_url: None,
            callback_url: None,
            bind_address: None,
            certificate_path: None,
            private_key_path: None,
            configured: false,
            managed: false,
            setup_available: attendee_managed::available(),
        }
    }
}

impl AttendeeSettings {
    fn config(&self, api_key: String) -> CmdResult<Config> {
        Ok(Config {
            api_url: self.api_url.clone(),
            api_key,
            callback_url: self.callback_url.clone(),
            bind_address: self.bind_address.parse().map_err(|_| invalid_settings())?,
            certificate_path: self.certificate_path.clone(),
            private_key_path: self.private_key_path.clone(),
        })
    }

    fn status(self, configured: bool) -> AttendeeSettingsStatus {
        AttendeeSettingsStatus {
            api_url: Some(self.api_url),
            callback_url: Some(self.callback_url),
            bind_address: Some(self.bind_address),
            certificate_path: Some(self.certificate_path),
            private_key_path: Some(self.private_key_path),
            configured,
            managed: self.managed,
            setup_available: attendee_managed::available(),
        }
    }
}

fn settings_unavailable() -> UiMessage {
    UiMessage::app(
        "native.attendeeSettingsUnavailable",
        "Attendee settings are unavailable.",
    )
}

fn credential_unavailable() -> UiMessage {
    UiMessage::app(
        "native.attendeeCredentialUnavailable",
        "Could not access the system credential store for Attendee.",
    )
}

fn invalid_settings() -> UiMessage {
    UiMessage::app(
        "native.attendeeInvalidSettings",
        "Check the Attendee API URL, WebSocket URL, bind address, and TLS files.",
    )
}

fn profile_lock() -> CmdResult<MutexGuard<'static, ()>> {
    PROFILE.lock().map_err(|_| settings_unavailable())
}

pub(crate) fn library_root(app: &AppHandle) -> CmdResult<PathBuf> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        return Ok(repository.root().to_path_buf());
    }
    app.try_state::<AppState>()
        .map(|state| state.store.root().to_path_buf())
        .ok_or_else(settings_unavailable)
}

fn credential_target(root: &Path, api_url: &str) -> String {
    let library = Sha256::digest(root.to_string_lossy().as_bytes());
    let endpoint = Sha256::digest(api_url.as_bytes());
    format!("Bazza:Attendee:api:v1:{library:x}:{endpoint:x}")
}

fn read_profile(root: &Path) -> CmdResult<Option<AttendeeSettings>> {
    match fs::read(root.join(FILE_NAME)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| settings_unavailable()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(settings_unavailable()),
    }
}

fn write_profile(root: &Path, settings: &AttendeeSettings) -> CmdResult<()> {
    let mut file = tempfile::NamedTempFile::new_in(root).map_err(|_| settings_unavailable())?;
    serde_json::to_writer(&mut file, settings).map_err(|_| settings_unavailable())?;
    file.flush().map_err(|_| settings_unavailable())?;
    file.as_file()
        .sync_all()
        .map_err(|_| settings_unavailable())?;
    file.persist(root.join(FILE_NAME))
        .map_err(|_| settings_unavailable())?;
    Ok(())
}

fn load_key(target: &str) -> CmdResult<Option<String>> {
    credentials::load(CREDENTIAL_SERVICE, target).map_err(|_| credential_unavailable())
}

pub(crate) fn recording_config(
    app: &AppHandle,
) -> CmdResult<(Config, Option<attendee_managed::Runtime>)> {
    let root = library_root(app)?;
    let not_configured = || {
        UiMessage::app(
            "native.attendeeNotConfigured",
            "Connect an Attendee instance in Settings first.",
        )
    };
    let (settings, key) = {
        let _guard = profile_lock()?;
        let settings = read_profile(&root)?.ok_or_else(not_configured)?;
        let key =
            load_key(&credential_target(&root, &settings.api_url))?.ok_or_else(not_configured)?;
        (settings, key)
    };
    if settings.managed {
        let (result, runtime) = attendee_managed::start(app)?;
        Ok((result.settings.config(result.api_key)?, Some(runtime)))
    } else {
        Ok((settings.config(key)?, None))
    }
}

#[tauri::command]
pub async fn attendee_status(app: AppHandle) -> CmdResult<AttendeeSettingsStatus> {
    let root = library_root(&app)?;
    crate::commands::blocking(move || {
        let _guard = profile_lock()?;
        let Some(settings) = read_profile(&root)? else {
            return Ok(AttendeeSettingsStatus::default());
        };
        let configured = load_key(&credential_target(&root, &settings.api_url))?.is_some();
        Ok(settings.status(configured))
    })
    .await
}

#[tauri::command]
pub async fn attendee_configure(
    app: AppHandle,
    mut settings: AttendeeSettings,
    api_key: Option<String>,
) -> CmdResult<AttendeeSettingsStatus> {
    let root = library_root(&app)?;
    settings.managed = false;
    crate::commands::blocking(move || save_profile(&root, settings, api_key)).await
}

fn reject_replacing_manual(root: &Path, previous: Option<&AttendeeSettings>) -> CmdResult<()> {
    if let Some(settings) = previous.filter(|settings| !settings.managed) {
        if load_key(&credential_target(root, &settings.api_url))?.is_some() {
            return Err(UiMessage::app(
                "native.attendeeSetupManualExists",
                "Disconnect the existing Attendee connection before setting up the local server.",
            ));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn attendee_setup(app: AppHandle) -> CmdResult<AttendeeSettingsStatus> {
    let root = library_root(&app)?;
    crate::commands::blocking(move || {
        if crate::commands::recording::recording_status(app.clone()).state != "idle" {
            return Err(UiMessage::app(
                "native.attendeeSetupDuringRecording",
                "Stop the recording before changing Google Meet setup.",
            ));
        }
        {
            let _guard = profile_lock()?;
            reject_replacing_manual(&root, read_profile(&root)?.as_ref())?;
        }
        let (mut result, _lease) = attendee_managed::setup(&app)?;
        result.settings.managed = true;
        save_profile(&root, result.settings, Some(result.api_key))
    })
    .await
}

fn save_profile(
    root: &Path,
    mut settings: AttendeeSettings,
    api_key: Option<String>,
) -> CmdResult<AttendeeSettingsStatus> {
    settings.api_url = settings.api_url.trim().trim_end_matches('/').to_owned();
    settings.callback_url = settings.callback_url.trim().to_owned();
    settings.bind_address = settings.bind_address.trim().to_owned();
    let _guard = profile_lock()?;
    let previous = read_profile(root)?;
    if settings.managed {
        reject_replacing_manual(root, previous.as_ref())?;
    }
    let target = credential_target(root, &settings.api_url);
    let previous_key = load_key(&target)?;
    let new_key = api_key.map(|key| key.trim().to_owned());
    let key = new_key
        .as_ref()
        .or(previous_key.as_ref())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            UiMessage::app(
                "native.attendeeKeyRequired",
                "Enter the API key for this Attendee instance.",
            )
        })?;
    settings
        .config(key.clone())?
        .validate()
        .map_err(|_| invalid_settings())?;
    if new_key.is_some() {
        credentials::store(CREDENTIAL_SERVICE, &target, key)
            .map_err(|_| credential_unavailable())?;
    }
    if let Err(error) = write_profile(root, &settings) {
        if new_key.is_some() {
            let rollback = match previous_key {
                Some(key) => credentials::store(CREDENTIAL_SERVICE, &target, &key),
                None => credentials::remove(CREDENTIAL_SERVICE, &target),
            };
            if rollback.is_err() {
                log::warn!("Could not restore the previous Attendee credential.");
            }
        }
        return Err(error);
    }
    if let Some(old) = previous.filter(|old| old.api_url != settings.api_url) {
        if credentials::remove(CREDENTIAL_SERVICE, &credential_target(root, &old.api_url)).is_err()
        {
            log::warn!("Could not remove the previous Attendee credential.");
        }
    }
    Ok(settings.status(true))
}

#[tauri::command]
pub async fn attendee_disconnect(app: AppHandle) -> CmdResult<AttendeeSettingsStatus> {
    let root = library_root(&app)?;
    crate::commands::blocking(move || {
        let _guard = profile_lock()?;
        if let Some(settings) = read_profile(&root)? {
            credentials::remove(
                CREDENTIAL_SERVICE,
                &credential_target(&root, &settings.api_url),
            )
            .map_err(|_| credential_unavailable())?;
        }
        match fs::remove_file(root.join(FILE_NAME)) {
            Ok(()) => Ok(AttendeeSettingsStatus::default()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Ok(AttendeeSettingsStatus::default())
            }
            Err(_) => Err(settings_unavailable()),
        }
    })
    .await
}

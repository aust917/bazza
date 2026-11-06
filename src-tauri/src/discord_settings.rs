//! Desktop-local Discord preferences and REST discovery. Voice capture is
//! started separately, after the user starts a recording.

mod api;

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use bazza_service_client::RepositoryClient;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::{commands::CmdResult, integration_credentials as credentials, state::AppState, ui_message::UiMessage};

const FILE_NAME: &str = "discord-integration.json";
const CREDENTIAL_SERVICE: &str = "app.bazza.discord";
const VIEW_AND_CONNECT: u64 = (1 << 10) | (1 << 20);
const DEVELOPER_PORTAL: &str = "https://discord.com/developers/applications";
static PROFILE: Mutex<()> = Mutex::new(());

#[derive(Clone, Deserialize, Serialize)]
pub struct DiscordBot {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Serialize)]
pub struct DiscordGuild {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct DiscordChannel {
    pub guild_id: String,
    pub guild_name: String,
    pub channel_id: String,
    pub channel_name: String,
}

#[derive(Default, Deserialize, Serialize)]
pub struct DiscordSettingsStatus {
    pub bot: Option<DiscordBot>,
    pub channel: Option<DiscordChannel>,
}

fn error(code: &str, message: &str) -> UiMessage {
    UiMessage::app(code, message)
}

// A synchronous command keeps ShellExecute on the Windows UI thread, where COM is initialized.
#[tauri::command]
pub fn discord_open_developer_portal(app: AppHandle) -> CmdResult<()> {
    app.opener()
        .open_url(DEVELOPER_PORTAL, None::<&str>)
        .map_err(|cause| {
            log::warn!("Could not open Discord Developer Portal: {cause}");
            error(
                "native.discordPortalOpenFailed",
                "Could not open Discord Developer Portal in the browser.",
            )
            .with_detail(cause.to_string())
        })
}

fn settings_unavailable() -> UiMessage {
    error(
        "native.discordSettingsUnavailable",
        "Discord settings are unavailable.",
    )
}

fn credential_unavailable() -> UiMessage {
    error(
        "native.discordCredentialUnavailable",
        "Could not access the system credential store for Discord.",
    )
}

fn not_configured() -> UiMessage {
    error(
        "native.discordNotConfigured",
        "Connect your Discord bot first.",
    )
}

fn invalid_link() -> UiMessage {
    error(
        "native.discordInvalidChannelLink",
        "Use a Discord server voice channel link in the form https://discord.com/channels/server/channel.",
    )
}

fn profile_lock() -> CmdResult<MutexGuard<'static, ()>> {
    PROFILE.lock().map_err(|_| settings_unavailable())
}

fn profile_root(app: &AppHandle) -> CmdResult<PathBuf> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        return Ok(repository.root().to_path_buf());
    }
    app.try_state::<AppState>()
        .map(|state| state.store.root().to_path_buf())
        .ok_or_else(settings_unavailable)
}

fn credential_target(root: &Path, bot_id: &str) -> String {
    let root = root.to_string_lossy();
    let library = Sha256::digest(root.as_bytes());
    format!("Bazza:Discord:bot:v1:{library:x}:{bot_id}")
}

fn read_profile(root: &Path) -> CmdResult<DiscordSettingsStatus> {
    match fs::read(root.join(FILE_NAME)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| settings_unavailable()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(DiscordSettingsStatus::default()),
        Err(_) => Err(settings_unavailable()),
    }
}

fn write_profile(root: &Path, status: &DiscordSettingsStatus) -> CmdResult<()> {
    let mut file = tempfile::NamedTempFile::new_in(root).map_err(|_| settings_unavailable())?;
    serde_json::to_writer(&mut file, status).map_err(|_| settings_unavailable())?;
    file.flush().map_err(|_| settings_unavailable())?;
    file.as_file()
        .sync_all()
        .map_err(|_| settings_unavailable())?;
    file.persist(root.join(FILE_NAME))
        .map_err(|_| settings_unavailable())?;
    Ok(())
}

fn load_token(root: &Path, bot: &DiscordBot) -> CmdResult<Option<String>> {
    credentials::load(CREDENTIAL_SERVICE, &credential_target(root, &bot.id)).map_err(|_| credential_unavailable())
}

fn connected(root: &Path) -> CmdResult<(DiscordBot, String)> {
    let _guard = profile_lock()?;
    let bot = read_profile(root)?.bot.ok_or_else(not_configured)?;
    let token = load_token(root, &bot)?.ok_or_else(not_configured)?;
    Ok((bot, token))
}

/// Native-only token read for the recording startup path. The voice worker
/// verifies the selected guild/channel and permissions directly with Discord.
pub(crate) fn recording_token(
    app: &AppHandle,
    guild_id: u64,
    channel_id: u64,
) -> CmdResult<String> {
    if guild_id == 0 || channel_id == 0 {
        return Err(invalid_link());
    }
    connected(&profile_root(app)?).map(|(_, token)| token)
}

fn snowflake(input: &str) -> Option<u64> {
    let parsed = input.parse::<u64>().ok()?;
    (parsed != 0 && input == parsed.to_string()).then_some(parsed)
}

fn channel_ids(input: &str) -> CmdResult<(&str, &str)> {
    let mut segments = input.trim().split('/');
    if segments.next() != Some("https:")
        || segments.next() != Some("")
        || segments.next() != Some("discord.com")
        || segments.next() != Some("channels")
    {
        return Err(invalid_link());
    }
    let guild_id = segments.next().ok_or_else(invalid_link)?;
    let channel_id = segments.next().ok_or_else(invalid_link)?;
    if snowflake(guild_id).is_none()
        || snowflake(channel_id).is_none()
        || !matches!(segments.next(), None | Some(""))
        || segments.next().is_some()
    {
        return Err(invalid_link());
    }
    Ok((guild_id, channel_id))
}

#[tauri::command]
pub async fn discord_status(app: AppHandle) -> CmdResult<DiscordSettingsStatus> {
    let root = profile_root(&app)?;
    crate::commands::blocking(move || {
        let _guard = profile_lock()?;
        let status = read_profile(&root)?;
        if let Some(bot) = &status.bot {
            if load_token(&root, bot)?.is_some() {
                return Ok(status);
            }
        }
        Ok(DiscordSettingsStatus::default())
    })
    .await
}

#[tauri::command]
pub async fn discord_save_token(app: AppHandle, token: String) -> CmdResult<DiscordSettingsStatus> {
    let root = profile_root(&app)?;
    let token = token.trim().to_owned();
    let bot = api::Api::new(&token)?.current_bot().await?;
    crate::commands::blocking(move || {
        let _guard = profile_lock()?;
        let previous = read_profile(&root)?;
        let same_bot = previous.bot.as_ref().is_some_and(|old| old.id == bot.id);
        let target = credential_target(&root, &bot.id);
        credentials::store(CREDENTIAL_SERVICE, &target, &token).map_err(|_| credential_unavailable())?;
        let status = DiscordSettingsStatus {
            bot: Some(bot),
            channel: if same_bot { previous.channel } else { None },
        };
        if let Err(err) = write_profile(&root, &status) {
            if !same_bot {
                let _ = credentials::remove(CREDENTIAL_SERVICE, &target);
            }
            return Err(err);
        }
        if let Some(old) = previous.bot.filter(|_| !same_bot) {
            if credentials::remove(CREDENTIAL_SERVICE, &credential_target(&root, &old.id)).is_err() {
                log::warn!("Could not remove the previous Discord credential.");
            }
        }
        Ok(status)
    })
    .await
}

#[tauri::command]
pub async fn discord_disconnect(app: AppHandle) -> CmdResult<DiscordSettingsStatus> {
    let root = profile_root(&app)?;
    crate::commands::blocking(move || {
        let _guard = profile_lock()?;
        if let Some(bot) = read_profile(&root)?.bot {
            credentials::remove(CREDENTIAL_SERVICE, &credential_target(&root, &bot.id))
                .map_err(|_| credential_unavailable())?;
        }
        match fs::remove_file(root.join(FILE_NAME)) {
            Ok(()) => Ok(DiscordSettingsStatus::default()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Ok(DiscordSettingsStatus::default())
            }
            Err(_) => Err(settings_unavailable()),
        }
    })
    .await
}

#[tauri::command]
pub async fn discord_guilds(app: AppHandle) -> CmdResult<Vec<DiscordGuild>> {
    let root = profile_root(&app)?;
    let (_, token) = crate::commands::blocking(move || connected(&root)).await?;
    api::Api::new(&token)?.guilds().await
}

#[tauri::command]
pub async fn discord_channels(app: AppHandle, guild_id: String) -> CmdResult<Vec<DiscordChannel>> {
    if snowflake(&guild_id).is_none() {
        return Err(invalid_link());
    }
    let root = profile_root(&app)?;
    let (bot, token) = crate::commands::blocking(move || connected(&root)).await?;
    api::Api::new(&token)?.channels(&bot, &guild_id).await
}

#[tauri::command]
pub async fn discord_set_channel(app: AppHandle, input: String) -> CmdResult<DiscordChannel> {
    let (guild_id, channel_id) = channel_ids(&input)?;
    let root = profile_root(&app)?;
    let (bot, token) = {
        let root = root.clone();
        crate::commands::blocking(move || connected(&root)).await?
    };
    let channel = api::Api::new(&token)?
        .channel(&bot, guild_id, channel_id)
        .await?;
    crate::commands::blocking(move || {
        let _guard = profile_lock()?;
        let mut status = read_profile(&root)?;
        if !status
            .bot
            .as_ref()
            .is_some_and(|active| active.id == bot.id)
        {
            return Err(error(
                "native.discordChannelChanged",
                "The connected Discord bot changed. Select the channel again.",
            ));
        }
        status.channel = Some(channel.clone());
        write_profile(&root, &status)?;
        Ok(channel)
    })
    .await
}

#[tauri::command]
pub async fn discord_invite_url(app: AppHandle, guild_id: Option<String>) -> CmdResult<String> {
    let guild_id = guild_id
        .as_deref()
        .map(|id| snowflake(id).ok_or_else(invalid_link))
        .transpose()?;
    let root = profile_root(&app)?;
    let (_, token) = crate::commands::blocking(move || connected(&root)).await?;
    let app_id = api::Api::new(&token)?.application_id().await?;
    let mut url = format!(
        "https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot&permissions={VIEW_AND_CONNECT}",
    );
    if let Some(guild_id) = guild_id {
        url.push_str(&format!("&guild_id={guild_id}&disable_guild_select=true"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_links_are_restricted_to_discord_guild_channels() {
        assert_eq!(
            channel_ids("https://discord.com/channels/123/456/").unwrap(),
            ("123", "456")
        );
        for input in [
            "https://discord.com.example/channels/123/456",
            "https://discord.com/channels/@me/456",
            "https://discord.com/channels/123/0",
            "https://discord.com/channels/123/456?other=1",
            "https://discord.com/channels/123/456/message",
            "https://discord.com/channels/123/18446744073709551616",
        ] {
            assert_eq!(
                channel_ids(input).unwrap_err().code,
                "native.discordInvalidChannelLink"
            );
        }
    }
}

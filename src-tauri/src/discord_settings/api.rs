use std::{collections::HashSet, time::Duration};

use reqwest::{
    header::{HeaderValue, AUTHORIZATION},
    Client, StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize};

use super::{
    credentials::MAX_TOKEN_BYTES, error, snowflake, DiscordBot, DiscordChannel, DiscordGuild,
    VIEW_AND_CONNECT,
};
use crate::{commands::CmdResult, ui_message::UiMessage};

const BASE: &str = "https://discord.com/api/v10";
const USER_AGENT: &str = concat!(
    "DiscordBot (",
    env!("CARGO_PKG_REPOSITORY"),
    ", ",
    env!("CARGO_PKG_VERSION"),
    ")",
);
const ADMINISTRATOR: u64 = 1 << 3;
const GUILD_VOICE: u8 = 2;
const CHANNEL_OBFUSCATED: u64 = 1 << 17;

fn token_invalid() -> UiMessage {
    error(
        "native.discordTokenInvalid",
        "Discord did not accept this bot token.",
    )
}

fn unavailable() -> UiMessage {
    error(
        "native.discordUnavailable",
        "Discord is unavailable. Try again later.",
    )
}

fn server_unavailable() -> UiMessage {
    error(
        "native.discordServerUnavailable",
        "This server is unavailable to the bot. Check whether it was added to the server.",
    )
}

fn channel_unavailable() -> UiMessage {
    error(
        "native.discordChannelUnavailable",
        "Choose a server voice channel that the bot can view and connect to.",
    )
}

pub(super) struct Api {
    client: Client,
    authorization: HeaderValue,
}

impl Api {
    pub fn new(token: &str) -> CmdResult<Self> {
        if !(1..=MAX_TOKEN_BYTES).contains(&token.len())
            || !token.bytes().all(|ch| ch.is_ascii_graphic())
        {
            return Err(token_invalid());
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Bot {token}")).map_err(|_| token_invalid())?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| unavailable())?;
        Ok(Self {
            client,
            authorization,
        })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        inaccessible: fn() -> UiMessage,
    ) -> CmdResult<T> {
        let response = self
            .client
            .get(format!("{BASE}{path}"))
            .header(AUTHORIZATION, self.authorization.clone())
            .send()
            .await
            .map_err(|_| unavailable())?;
        match response.status() {
            StatusCode::OK => response.json().await.map_err(|_| unavailable()),
            StatusCode::UNAUTHORIZED => Err(token_invalid()),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Err(inaccessible()),
            StatusCode::TOO_MANY_REQUESTS => Err(error(
                "native.discordRateLimited",
                "Discord is rate limiting requests. Wait a moment and try again.",
            )),
            _ => Err(unavailable()),
        }
    }

    pub async fn current_bot(&self) -> CmdResult<DiscordBot> {
        #[derive(Deserialize)]
        struct User {
            id: String,
            username: String,
            #[serde(default)]
            bot: bool,
        }
        let user: User = self.get("/users/@me", token_invalid).await?;
        if !user.bot || snowflake(&user.id).is_none() {
            return Err(token_invalid());
        }
        Ok(DiscordBot {
            id: user.id,
            name: user.username,
        })
    }

    pub async fn application_id(&self) -> CmdResult<u64> {
        #[derive(Deserialize)]
        struct Application {
            id: String,
        }
        let app: Application = self.get("/applications/@me", unavailable).await?;
        snowflake(&app.id).ok_or_else(unavailable)
    }

    pub async fn guilds(&self) -> CmdResult<Vec<DiscordGuild>> {
        let mut guilds = Vec::new();
        let mut after = 0;
        loop {
            let mut path = "/users/@me/guilds?limit=200".to_owned();
            if after > 0 {
                path.push_str(&format!("&after={after}"));
            }
            let page: Vec<DiscordGuild> = self.get(&path, unavailable).await?;
            let next = page
                .iter()
                .filter_map(|guild| snowflake(&guild.id))
                .max()
                .unwrap_or(after);
            let complete = page.len() < 200 || next <= after;
            guilds.extend(
                page.into_iter()
                    .filter(|guild| snowflake(&guild.id).is_some_and(|id| id > after)),
            );
            if complete {
                break;
            }
            after = next;
        }
        guilds.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then(left.id.cmp(&right.id))
        });
        Ok(guilds)
    }

    async fn access(&self, bot: &DiscordBot, guild_id: &str) -> CmdResult<GuildAccess> {
        let guild_path = format!("/guilds/{guild_id}");
        let member_path = format!(
            "/guilds/{guild_id}/members/{}",
            snowflake(&bot.id).ok_or_else(token_invalid)?
        );
        let (guild, member) = tokio::try_join!(
            self.get::<Guild>(&guild_path, server_unavailable),
            self.get::<Member>(&member_path, server_unavailable),
        )?;
        if guild.id != guild_id {
            return Err(server_unavailable());
        }
        Ok(GuildAccess::new(guild, member, &bot.id))
    }

    pub async fn channels(
        &self,
        bot: &DiscordBot,
        guild_id: &str,
    ) -> CmdResult<Vec<DiscordChannel>> {
        let channels_path = format!("/guilds/{guild_id}/channels");
        let (access, mut channels) = tokio::try_join!(
            self.access(bot, guild_id),
            self.get::<Vec<Channel>>(&channels_path, server_unavailable),
        )?;
        channels.sort_by_key(|channel| (channel.position, snowflake(&channel.id)));
        Ok(channels
            .into_iter()
            .filter_map(|channel| access.visible_voice(bot, channel))
            .collect())
    }

    pub async fn channel(
        &self,
        bot: &DiscordBot,
        guild_id: &str,
        channel_id: &str,
    ) -> CmdResult<DiscordChannel> {
        let channel_path = format!("/channels/{channel_id}");
        let (access, channel) = tokio::try_join!(
            self.access(bot, guild_id),
            self.get::<Channel>(&channel_path, channel_unavailable),
        )?;
        if channel.id != channel_id || channel.guild_id.as_deref() != Some(guild_id) {
            return Err(channel_unavailable());
        }
        access
            .visible_voice(bot, channel)
            .ok_or_else(channel_unavailable)
    }
}

#[derive(Deserialize)]
struct Guild {
    id: String,
    name: String,
    owner_id: String,
    roles: Vec<Role>,
}

#[derive(Deserialize)]
struct Role {
    id: String,
    permissions: String,
}

#[derive(Deserialize)]
struct Member {
    roles: HashSet<String>,
}

#[derive(Deserialize)]
struct Channel {
    id: String,
    guild_id: Option<String>,
    name: Option<String>,
    #[serde(rename = "type")]
    kind: u8,
    position: Option<i64>,
    flags: Option<u64>,
    permission_overwrites: Option<Vec<Overwrite>>,
}

#[derive(Deserialize)]
struct Overwrite {
    id: String,
    #[serde(rename = "type")]
    kind: u8,
    allow: String,
    deny: String,
}

impl Overwrite {
    fn bits(&self) -> Option<(u64, u64)> {
        Some((self.allow.parse().ok()?, self.deny.parse().ok()?))
    }
}

struct GuildAccess {
    guild: Guild,
    member: Member,
    base_permissions: u64,
}

impl GuildAccess {
    fn new(guild: Guild, member: Member, bot_id: &str) -> Self {
        let base_permissions = if guild.owner_id == bot_id {
            ADMINISTRATOR
        } else {
            guild
                .roles
                .iter()
                .filter(|role| role.id == guild.id || member.roles.contains(&role.id))
                .fold(0, |bits, role| {
                    bits | role.permissions.parse::<u64>().unwrap_or(0)
                })
        };
        Self {
            guild,
            member,
            base_permissions,
        }
    }

    /// Discord applies @everyone first, combined role denies/allows next, and
    /// the member-specific overwrite last.
    fn can_join(&self, bot_id: &str, overwrites: &[Overwrite]) -> Option<bool> {
        if self.base_permissions & ADMINISTRATOR != 0 {
            return Some(true);
        }
        let mut permissions = self.base_permissions;
        if let Some(everyone) = overwrites
            .iter()
            .find(|entry| entry.kind == 0 && entry.id == self.guild.id)
        {
            let (allow, deny) = everyone.bits()?;
            permissions = (permissions & !deny) | allow;
        }
        let (mut role_allow, mut role_deny) = (0, 0);
        for role in overwrites.iter().filter(|entry| {
            entry.kind == 0 && entry.id != self.guild.id && self.member.roles.contains(&entry.id)
        }) {
            let (allow, deny) = role.bits()?;
            role_allow |= allow;
            role_deny |= deny;
        }
        permissions = (permissions & !role_deny) | role_allow;
        if let Some(member) = overwrites
            .iter()
            .find(|entry| entry.kind == 1 && entry.id == bot_id)
        {
            let (allow, deny) = member.bits()?;
            permissions = (permissions & !deny) | allow;
        }
        Some(permissions & VIEW_AND_CONNECT == VIEW_AND_CONNECT)
    }

    fn visible_voice(&self, bot: &DiscordBot, channel: Channel) -> Option<DiscordChannel> {
        let name = channel.name?;
        if channel.kind != GUILD_VOICE
            || snowflake(&channel.id).is_none()
            || channel
                .guild_id
                .as_ref()
                .is_some_and(|id| id != &self.guild.id)
            || name == "___hidden___"
            || channel.flags.unwrap_or(0) & CHANNEL_OBFUSCATED != 0
            || !self.can_join(
                &bot.id,
                channel.permission_overwrites.as_deref().unwrap_or_default(),
            )?
        {
            return None;
        }
        Some(DiscordChannel {
            guild_id: self.guild.id.clone(),
            guild_name: self.guild.name.clone(),
            channel_id: channel.id,
            channel_name: name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_override_can_keep_a_bot_out_of_a_channel_its_role_can_join() {
        let guild = Guild {
            id: "1".into(),
            name: "Server".into(),
            owner_id: "9".into(),
            roles: vec![Role {
                id: "2".into(),
                permissions: VIEW_AND_CONNECT.to_string(),
            }],
        };
        let access = GuildAccess::new(
            guild,
            Member {
                roles: ["2".into()].into(),
            },
            "3",
        );
        assert_eq!(access.can_join("3", &[]), Some(true));
        let overwrite = Overwrite {
            id: "3".into(),
            kind: 1,
            allow: "0".into(),
            deny: (1 << 20).to_string(),
        };
        assert_eq!(access.can_join("3", &[overwrite]), Some(false));
    }
}

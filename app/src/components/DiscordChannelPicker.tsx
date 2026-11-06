import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Select } from "@/components/ui";
import { cmd, type DiscordChannel, type DiscordGuild, type DiscordRecordingSelection } from "@/lib/ipc";
import { displayError } from "@/lib/i18n";

export const discordChannelUrl = ({ guild_id, channel_id }: DiscordRecordingSelection) =>
  `https://discord.com/channels/${guild_id}/${channel_id}`;

export function DiscordChannelPicker({ channel, onChange, disabled, compact = false }: {
  channel: DiscordChannel | null;
  onChange: (channel: DiscordChannel | null) => void;
  disabled?: boolean;
  compact?: boolean;
}) {
  const { t } = useTranslation("main");
  const [guilds, setGuilds] = useState<DiscordGuild[] | null>(null);
  const [guildId, setGuildId] = useState(channel?.guild_id ?? "");
  const [channels, setChannels] = useState<DiscordChannel[] | null>(null);
  const [guildError, setGuildError] = useState<unknown>(null);
  const [channelError, setChannelError] = useState<unknown>(null);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setGuilds(null);
    setGuildError(null);
    void cmd.discordGuilds().then((rows) => {
      if (cancelled) return;
      setGuilds(rows);
      if (rows.length === 1) setGuildId((current) => current || rows[0].id);
    }, (error) => { if (!cancelled) setGuildError(error); });
    return () => { cancelled = true; };
  }, [revision]);

  useEffect(() => {
    let cancelled = false;
    setChannels(null);
    setChannelError(null);
    if (!guildId) return;
    void cmd.discordChannels(guildId).then((rows) => {
      if (!cancelled) setChannels(rows);
    }, (error) => { if (!cancelled) setChannelError(error); });
    return () => { cancelled = true; };
  }, [guildId, revision]);

  const error = guildError ?? channelError;
  const channelOptions = channels?.filter((row) => row.guild_id === guildId) ?? [];
  const compactSelect = compact ? "h-6 min-w-0 px-1.5 pr-6 text-[11px]" : undefined;
  const field = compact ? "min-w-0 text-[11px] text-ink-3" : "flex min-w-0 flex-col justify-end gap-1 text-xs font-semibold text-ink-2";
  const fullRow = compact ? "col-span-2" : "sm:col-span-2";

  return (
    <div className={compact ? "grid grid-cols-2 gap-2" : "grid gap-2 sm:grid-cols-2"}>
      <label className={field}>
        <span>{t("discord.server")}</span>
        <Select value={guildId} className={compactSelect} disabled={disabled || guilds === null} onChange={(event) => {
          setGuildId(event.target.value);
          setChannels(null);
          onChange(null);
        }}>
          <option value="">{t(guilds === null && !guildError ? "discord.loading" : "discord.pickServer")}</option>
          {guildId && !guilds?.some((guild) => guild.id === guildId) && <option value={guildId}>{channel?.guild_name ?? guildId}</option>}
          {guilds?.map((guild) => <option key={guild.id} value={guild.id}>{guild.name}</option>)}
        </Select>
      </label>
      <label className={field}>
        <span>{t("discord.channel")}</span>
        <Select value={channel?.guild_id === guildId ? channel.channel_id : ""} className={compactSelect} disabled={disabled || !guildId || channels === null} onChange={(event) => onChange(channelOptions.find((row) => row.channel_id === event.target.value) ?? null)}>
          <option value="">{t(guildId && channels === null && !channelError ? "discord.loading" : "discord.pickChannel")}</option>
          {channel?.guild_id === guildId && !channelOptions.some((row) => row.channel_id === channel.channel_id) && <option value={channel.channel_id}>{channel.channel_name}</option>}
          {channelOptions.map((row) => <option key={row.channel_id} value={row.channel_id}>{row.channel_name}</option>)}
        </Select>
      </label>
      {guilds?.length === 0 && <p className={`text-[11px] text-ink-3 ${fullRow}`}>{t("discord.noServers")}</p>}
      {guildId && channels?.length === 0 && <p className={`text-[11px] text-ink-3 ${fullRow}`}>{t("discord.noChannels")}</p>}
      {error != null && <div className={`flex items-center gap-2 ${fullRow}`}>
        <p role="alert" className="text-[11px] text-ember">{displayError(error)}</p>
        <Button type="button" variant="outline" size="sm" disabled={disabled} onClick={() => setRevision((value) => value + 1)}>{t("discord.retry")}</Button>
      </div>}
    </div>
  );
}

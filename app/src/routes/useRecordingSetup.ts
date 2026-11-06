import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { displayError } from "@/lib/i18n";
import {
  cmd, on, type AudioDevice, type DiscordChannel, type DiscordSettingsStatus,
  type Meeting, type RecordingBotTarget, type RecordingState, type Settings, type VideoSource,
} from "@/lib/ipc";
import { isMeetUrl, screenSourceKey } from "@/lib/recordingSetup";
import { useRecordingBotConnection } from "./useRecordingBotConnection";

export type RecordingMode = "standard" | "meet" | "discord";
type IntegrationStatus<T> =
  | { state: "loading" }
  | { state: "error"; detail: unknown }
  | { state: "ready"; value: T };

interface SourceChoice {
  sources: VideoSource[];
  explicitScreenKey?: string;
}

export function useRecordingSetup() {
  const { t } = useTranslation("recording");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [settingsAttempt, setSettingsAttempt] = useState(0);
  const [loadingSettings, setLoadingSettings] = useState(true);
  const [settingsFailure, setSettingsFailure] = useState<unknown>(null);
  const [sourceChoice, setSourceChoice] = useState<SourceChoice | null>(null);
  const [sourceAttempt, setSourceAttempt] = useState(0);
  const [loadingSources, setLoadingSources] = useState(true);
  const [sourceFailure, setSourceFailure] = useState<unknown>(null);
  const [recording, setRecording] = useState<RecordingState | null>(null);
  const [statusAttempt, setStatusAttempt] = useState(0);
  const [statusFailure, setStatusFailure] = useState<unknown>(null);
  const [devices, setDevices] = useState<{ inputs: AudioDevice[]; outputs: AudioDevice[] }>({ inputs: [], outputs: [] });
  const [video, setVideo] = useState(true);
  const [mode, setMode] = useState<RecordingMode>("standard");
  const [discordStatus, setDiscordStatus] = useState<IntegrationStatus<DiscordSettingsStatus>>({ state: "loading" });
  const [discordAttempt, setDiscordAttempt] = useState(0);
  const [discordChannel, setDiscordChannel] = useState<DiscordChannel | null>(null);
  const [discordPickerRevision, setDiscordPickerRevision] = useState(0);
  const [discordLinkFailure, setDiscordLinkFailure] = useState<{ detail: unknown } | "invalid" | "unavailable" | null>(null);
  const [discordChecking, setDiscordChecking] = useState(false);
  const [attendeeStatus, setAttendeeStatus] = useState<IntegrationStatus<boolean>>({ state: "loading" });
  const [attendeeAttempt, setAttendeeAttempt] = useState(0);
  const [meetUrl, setMeetUrl] = useState("");
  const [failure, setFailure] = useState<{ detail: unknown } | null>(null);
  const [starting, setStarting] = useState(false);
  const pending = useRef(false);
  const checkingDiscord = useRef(false);
  const bot = useRecordingBotConnection();
  const { reset: resetBot } = bot;

  useEffect(() => {
    let disposed = false;
    void cmd.getSettings().then((value) => {
      if (disposed) return;
      setSettings(value);
      setVideo(value.video_enabled);
    }, (error) => {
      if (!disposed) setSettingsFailure(error);
    }).finally(() => {
      if (!disposed) setLoadingSettings(false);
    });
    return () => { disposed = true; };
  }, [settingsAttempt]);

  useEffect(() => {
    let disposed = false;
    void cmd.listVideoSources().then((sources) => {
      if (disposed) return;
      setSourceChoice((previous) => {
        const explicitScreenKey = previous?.explicitScreenKey;
        // If a manually selected display disappeared, require a new choice.
        if (explicitScreenKey && !sources.some((source) => screenSourceKey(source) === explicitScreenKey)) {
          return { sources, explicitScreenKey: "" };
        }
        return { sources, explicitScreenKey };
      });
    }, (error) => {
      if (!disposed) setSourceFailure(error);
    }).finally(() => {
      if (!disposed) setLoadingSources(false);
    });
    return () => { disposed = true; };
  }, [sourceAttempt]);

  useEffect(() => {
    let disposed = false;
    void cmd.listAudioDevices().then((value) => {
      if (!disposed) setDevices(value);
    }).catch(() => {});
    return () => { disposed = true; };
  }, []);

  useEffect(() => {
    let disposed = false;
    void cmd.discordStatus().then((value) => {
      if (disposed) return;
      setDiscordStatus({ state: "ready", value });
      setDiscordChannel(value.channel);
    }, (detail) => {
      if (!disposed) setDiscordStatus({ state: "error", detail });
    });
    return () => { disposed = true; };
  }, [discordAttempt]);

  useEffect(() => {
    let disposed = false;
    void cmd.attendeeStatus().then(({ configured }) => {
      if (!disposed) setAttendeeStatus({ state: "ready", value: configured });
    }, (detail) => {
      if (!disposed) setAttendeeStatus({ state: "error", detail });
    });
    return () => { disposed = true; };
  }, [attendeeAttempt]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let receivedEvent = false;

    async function loadStatus() {
      try {
        const stop = await on("recording_state", (status) => {
          if (disposed) return;
          receivedEvent = true;
          setStatusFailure(null);
          setRecording(status);
        });
        if (disposed) {
          stop();
          return;
        }
        unlisten = stop;
        const initial = await cmd.recordingStatus();
        if (!disposed && !receivedEvent) setRecording(initial);
      } catch (error) {
        if (!disposed && !receivedEvent) setStatusFailure(error);
      }
    }

    void loadStatus();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [statusAttempt]);

  useEffect(() => {
    if (recording && recording.state !== "idle" && !pending.current) resetBot();
  }, [recording, resetBot]);

  const screens = sourceChoice?.sources ?? [];
  const explicitScreenKey = sourceChoice?.explicitScreenKey;
  const selectedScreen = explicitScreenKey === undefined
    ? screens.find((source) => source.is_primary) ?? (screens.length === 1 ? screens[0] : undefined)
    : screens.find((source) => screenSourceKey(source) === explicitScreenKey);
  const screenKey = selectedScreen ? screenSourceKey(selectedScreen) : "";
  const attendeeReady = isMeetUrl(meetUrl.trim());
  const integration = mode === "meet" ? attendeeStatus : mode === "discord" ? discordStatus : null;
  const integrationConfigured = mode === "standard"
    || (mode === "meet" && attendeeStatus.state === "ready" && attendeeStatus.value)
    || (mode === "discord" && discordStatus.state === "ready" && Boolean(discordStatus.value.bot));
  const integrationError = integration?.state === "error" ? displayError(integration.detail) : null;
  const targetReady = mode === "meet" ? attendeeReady : mode === "discord" && !discordChecking && discordChannel != null;
  const busy = starting || (recording != null && recording.state !== "idle");
  const canRecord = Boolean(settings && recording?.state === "idle" && !busy
    && (!video || (!loadingSources && sourceFailure == null && selectedScreen)));
  const canConnect = Boolean(settings && integrationConfigured && targetReady && recording?.state === "idle" && !busy);

  const output = settings?.loopback_device
    ? devices.outputs.find((device) => device.id === settings.loopback_device)?.name ?? settings.loopback_device
    : devices.outputs.find((device) => device.is_default)?.name ?? t("popup.defaultOutput");
  const mic = settings?.mic_device
    ? devices.inputs.find((device) => device.id === settings.mic_device)?.name ?? settings.mic_device
    : devices.inputs.find((device) => device.is_default)?.name ?? t("popup.defaultMic");

  function retry() {
    if (settingsFailure != null) {
      setSettingsFailure(null);
      setLoadingSettings(true);
      setSettingsAttempt((attempt) => attempt + 1);
    }
    if (statusFailure != null) {
      setStatusFailure(null);
      setStatusAttempt((attempt) => attempt + 1);
    }
  }

  function refreshSources() {
    if (pending.current || busy || loadingSources) return;
    setSourceFailure(null);
    setLoadingSources(true);
    setSourceAttempt((attempt) => attempt + 1);
  }

  function retryIntegration() {
    if (mode === "meet" && attendeeStatus.state === "error") {
      setAttendeeStatus({ state: "loading" });
      setAttendeeAttempt((attempt) => attempt + 1);
    } else if (mode === "discord" && discordStatus.state === "error") {
      setDiscordStatus({ state: "loading" });
      setDiscordAttempt((attempt) => attempt + 1);
    }
  }

  function chooseScreen(screenKey: string) {
    setSourceChoice((current) => current && { ...current, explicitScreenKey: screenKey });
  }

  function chooseMode(next: RecordingMode) {
    if (pending.current || busy || mode === next) return;
    resetBot();
    setMode(next);
    setFailure(null);
  }

  function chooseMeetUrl(next: string) {
    if (next.trim() !== meetUrl.trim()) resetBot();
    setMeetUrl(next);
  }

  function chooseDiscordChannel(next: DiscordChannel | null) {
    if (next?.guild_id !== discordChannel?.guild_id || next?.channel_id !== discordChannel?.channel_id) resetBot();
    setDiscordChannel(next);
  }

  async function chooseDiscordLink(input: string): Promise<boolean> {
    if (mode !== "discord" || !integrationConfigured || pending.current || busy || checkingDiscord.current) return false;
    const match = /^https:\/\/discord\.com\/channels\/([1-9]\d{0,19})\/([1-9]\d{0,19})\/?$/.exec(input.trim());
    chooseDiscordChannel(null);
    if (!match) {
      setDiscordLinkFailure("invalid");
      return false;
    }
    const [, guildId, channelId] = match;
    checkingDiscord.current = true;
    setDiscordChecking(true);
    setDiscordLinkFailure(null);
    try {
      // This lookup lists only accessible voice channels and leaves the saved default alone.
      const channels = await cmd.discordChannels(guildId);
      const channel = channels.find((item) => item.guild_id === guildId && item.channel_id === channelId);
      if (!channel) {
        setDiscordLinkFailure("unavailable");
        return false;
      }
      setDiscordChannel(channel);
      setDiscordPickerRevision((revision) => revision + 1);
      return true;
    } catch (detail) {
      setDiscordLinkFailure({ detail });
      return false;
    } finally {
      checkingDiscord.current = false;
      setDiscordChecking(false);
    }
  }

  function connectBot() {
    if (!canConnect || pending.current || (mode === "discord" && checkingDiscord.current)) return;
    const target: RecordingBotTarget | null = mode === "meet"
      ? { provider: "meet", meetingUrl: meetUrl.trim() }
      : discordChannel
        ? { provider: "discord", guild_id: discordChannel.guild_id, channel_id: discordChannel.channel_id }
        : null;
    if (target) void bot.connect(target);
  }

  async function record(withBot: boolean): Promise<Meeting | null> {
    if (!settings || !canRecord || pending.current) return null;
    const botId = withBot ? bot.connectedId() : undefined;
    if (withBot && !botId) {
      setFailure({ detail: { message: t("setup.botConnectionLost") } });
      return null;
    }
    pending.current = true;
    setStarting(true);
    setFailure(null);
    if (!botId) resetBot();
    try {
      const meeting = await cmd.startRecording({
        app: mode === "standard" ? undefined : mode,
        video,
        target: video ? selectedScreen?.target : undefined,
        ...(video && selectedScreen?.screen_id ? { screen_id: selectedScreen.screen_id } : {}),
        ...(botId ? { bot_connection_id: botId } : {}),
      });
      if (botId) bot.adopted();
      return meeting;
    } catch (detail) {
      setFailure({ detail });
      return null;
    } finally {
      pending.current = false;
      setStarting(false);
    }
  }

  return {
    settings, screens, screenKey, selectedScreen, chooseScreen, video, setVideo,
    output, mic, mode, chooseMode, integrationConfigured, integrationError, retryIntegration,
    integrationLoading: integration?.state === "loading",
    discordChannel, setDiscordChannel: chooseDiscordChannel, discordPickerRevision, chooseDiscordLink, discordChecking,
    discordLinkError: discordLinkFailure === "invalid" ? t("setup.discordLinkInvalid")
      : discordLinkFailure === "unavailable" ? t("setup.discordLinkUnavailable")
      : discordLinkFailure ? displayError(discordLinkFailure.detail) : null,
    clearDiscordLinkError: () => setDiscordLinkFailure(null),
    meetUrl, setMeetUrl: chooseMeetUrl, attendeeReady,
    botStatus: bot.status,
    botDisconnecting: bot.disconnecting,
    botError: bot.error != null ? displayError(bot.error)
      : bot.status?.phase === "failed" && bot.status.message ? displayError(bot.status.message) : null,
    canConnect, connectBot, disconnectBot: bot.disconnect,
    error: failure ? displayError(failure.detail) : null,
    loadingSettings, loadingSources, starting, busy, recording, canRecord, retry, refreshSources, record,
    settingsError: settingsFailure == null ? null : t("popup.loadSettingsFailed", { detail: displayError(settingsFailure) }),
    sourceError: sourceFailure == null ? null : t("popup.listScreensFailed", { detail: displayError(sourceFailure) }),
    statusError: statusFailure == null ? null : displayError(statusFailure),
  };
}

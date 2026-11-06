import { Circle, RefreshCw, Video, VideoOff } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Input } from "@/components/ui";
import { DiscordChannelPicker } from "@/components/DiscordChannelPicker";
import { cmd, subscribe, type AudioDevice, type DiscordChannel, type DiscordSettingsStatus, type Events, type Settings, type VideoSource } from "@/lib/ipc";
import { displayError } from "@/lib/i18n";
import { sourceLabel } from "@/lib/format";
import { speechLanguageLabel } from "@/lib/languages";
import { isMeetUrl, screenSourceKey } from "@/lib/recordingSetup";

/** "Call detected - Record?" window. Nothing happens unless Record is clicked. */
export function Popup() {
  const { t, i18n } = useTranslation(["recording", "main"]);
  const [det, setDet] = useState<Events["detection"] | null>(null);
  const [video, setVideo] = useState(true);
  const [left, setLeft] = useState(30);
  const [screens, setScreens] = useState<VideoSource[]>([]);
  const [screenKey, setScreenKey] = useState("");
  const [sourceAttempt, setSourceAttempt] = useState(0);
  const [sourceFailure, setSourceFailure] = useState<unknown>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [devices, setDevices] = useState<{ inputs: AudioDevice[]; outputs: AudioDevice[] }>({ inputs: [], outputs: [] });
  const [error, setError] = useState<{ kind: "settings" | "record"; detail: unknown } | null>(null);
  const [starting, setStarting] = useState(false);
  const [loadingSources, setLoadingSources] = useState(false);
  const [discordStatus, setDiscordStatus] = useState<DiscordSettingsStatus | null>(null);
  const [discordChannel, setDiscordChannel] = useState<DiscordChannel | null>(null);
  const [discordEnabled, setDiscordEnabled] = useState(false);
  const [browsingDiscord, setBrowsingDiscord] = useState(false);
  const [attendeeConfigured, setAttendeeConfigured] = useState(false);
  const [attendeeEnabled, setAttendeeEnabled] = useState(false);
  const [meetUrl, setMeetUrl] = useState("");
  const startPending = useRef(false);
  const lastSourceDetection = useRef<Events["detection"] | null>(null);
  const detectionLabel = det?.app === "unknown" ? t("popup.unknown") : det?.label ?? "";
  const detectionTitle = det?.app === "unknown" ? t("popup.audioActivity") : det?.title ?? "";
  const attendeeReady = !attendeeEnabled || isMeetUrl(meetUrl.trim());

  const selectedScreen = screens.find((source) => screenSourceKey(source) === screenKey);
  const canStart = Boolean(settings && !starting && (!video || (!loadingSources && sourceFailure == null && selectedScreen))
    && (!discordEnabled || discordChannel) && attendeeReady);
  const output = settings?.loopback_device
    ? devices.outputs.find((d) => d.id === settings.loopback_device)?.name ?? settings.loopback_device
    : devices.outputs.find((d) => d.is_default)?.name ?? t("popup.defaultOutput");
  const mic = settings?.mic_device
    ? devices.inputs.find((d) => d.id === settings.mic_device)?.name ?? settings.mic_device
    : devices.inputs.find((d) => d.is_default)?.name ?? t("popup.defaultMic");

  useEffect(() => {
    return subscribe("detection", (d) => {
      setDet(d);
      setLeft(30);
    });
  }, []);

  // The popup window survives being hidden: reload preferences for every picker.
  useEffect(() => {
    if (!det) return;
    let cancelled = false;
    setSettings(null);
    setError(null);
    setDiscordStatus(null);
    setDiscordChannel(null);
    setDiscordEnabled(false);
    setBrowsingDiscord(false);
    setAttendeeConfigured(false);
    setAttendeeEnabled(false);
    setMeetUrl("");
    if (det.app === "meet" || det.app === "unknown") {
      void cmd.attendeeStatus().then(({ configured }) => {
        if (!cancelled) setAttendeeConfigured(configured);
      }).catch(() => {});
    }
    void cmd.discordStatus().then((status) => {
      if (cancelled) return;
      setDiscordStatus(status);
      setDiscordChannel(status.channel);
    }).catch(() => {});
    void cmd.getSettings().then((s) => {
      if (cancelled) return;
      setSettings(s);
      setVideo(s.video_enabled);
    }).catch((e) => { if (!cancelled) setError({ kind: "settings", detail: e }); });
    void cmd.listAudioDevices().then((d) => { if (!cancelled) setDevices(d); }).catch(() => {});
    return () => { cancelled = true; };
  }, [det]);

  useEffect(() => {
    if (!det) return;
    let cancelled = false;
    setLoadingSources(true);
    setSourceFailure(null);
    void cmd.listVideoSources().then((displays) => {
      if (cancelled) return;
      const firstLoad = lastSourceDetection.current !== det;
      lastSourceDetection.current = det;
      setScreens(displays);
      setScreenKey((current) => firstLoad
        ? displays.length === 1 ? screenSourceKey(displays[0]) : ""
        : displays.some((source) => screenSourceKey(source) === current) ? current : "");
    }).catch((e) => { if (!cancelled) setSourceFailure(e); })
      .finally(() => { if (!cancelled) setLoadingSources(false); });
    return () => { cancelled = true; };
  }, [det, sourceAttempt]);

  // Auto-hide countdown for detected calls.
  useEffect(() => {
    if (!det) return;
    const t = setInterval(() => setLeft((n) => n - 1), 1000);
    return () => clearInterval(t);
  }, [det]);
  useEffect(() => {
    if (det && left <= 0) void cmd.dismissDetection(det.app, "now");
  }, [left, det]);

  const record = async () => {
    if (!det || !settings || !canStart || startPending.current) return;
    startPending.current = true;
    setStarting(true);
    setError(null);
    const attendee = attendeeEnabled ? { meetingUrl: meetUrl.trim() } : undefined;
    const app = attendee ? "meet" : det.app === "unknown" ? undefined : det.app;
    const discord = discordEnabled && discordChannel ? { guild_id: discordChannel.guild_id, channel_id: discordChannel.channel_id } : undefined;
    try {
      await cmd.startRecording({
        app, video, target: video ? selectedScreen?.target : undefined,
        ...(video && selectedScreen?.screen_id ? { screen_id: selectedScreen.screen_id } : {}),
        ...(discord ? { discord } : {}), ...(attendee ? { attendee } : {}),
      });
    } catch (e) {
      setError({ kind: "record", detail: e });
    } finally {
      startPending.current = false;
      setStarting(false);
    }
  };

  // Escape = "Not now"; Enter = Record.
  useEffect(() => {
    if (!det) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") void cmd.dismissDetection(det.app, "now");
      else if (e.key === "Enter" && !(e.target instanceof HTMLSelectElement) && !(e.target instanceof HTMLButtonElement) && !(e.target instanceof HTMLInputElement)) {
        e.preventDefault();
        void record();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [det, video, selectedScreen, canStart, settings, discordEnabled, discordChannel, attendeeEnabled, meetUrl]);

  if (!det) return <div className="h-full" />;

  return (
    <div role="dialog" aria-label={t("popup.dialog", { app: detectionLabel })} className="flex h-full flex-col overflow-y-auto rounded-xl border border-line bg-canvas p-4 shadow-float" data-tauri-drag-region>
      <div className="flex items-start justify-between gap-3" data-tauri-drag-region>
        <div className="min-w-0" data-tauri-drag-region>
          <div className="flex items-center gap-2 text-[14px] font-semibold text-ink">
            <span className="h-2 w-2 rounded-full bg-moss" />
            {t("popup.detected", { app: detectionLabel })}
          </div>
          {det.event && <div className="mt-0.5 truncate text-[12px] font-medium text-ink-2" title={t("popup.calendar")}>{det.event}</div>}
          <div className="mt-0.5 truncate text-[12px] text-ink-3" title={detectionTitle}>{detectionTitle}</div>
        </div>
        <button
          aria-pressed={video}
          aria-label={t("popup.video")}
          onClick={() => setVideo((v) => !v)}
          className={`focus-ring inline-flex h-7 items-center gap-1.5 rounded-full px-2.5 text-[11px] font-medium ${video ? "bg-ink text-canvas" : "bg-canvas-3 text-ink-2"}`}
          title={t("popup.toggleVideo")}
        >
          {video ? <Video size={12} /> : <VideoOff size={12} />} {video ? t("popup.screenOn") : t("popup.audioOnly")}
        </button>
      </div>
      {video && (
        <div className="mt-2 flex items-center gap-2 text-[11px] text-ink-3">
          <label htmlFor="detected-recording-screen" className="shrink-0">{t("popup.screenToRecord")}</label>
          <select
            id="detected-recording-screen"
            value={screenKey}
            disabled={loadingSources || starting || sourceFailure != null || screens.length === 0}
            onChange={(e) => setScreenKey(e.target.value)}
            className="focus-ring h-6 min-w-0 flex-1 truncate rounded border border-line-2 bg-canvas px-1.5 text-[11px] text-ink"
          >
            <option value="" disabled>{t(loadingSources ? "setup.loadingSources" : "setup.chooseSource")}</option>
            {screens.map((source) => (
              <option key={screenSourceKey(source)} value={screenSourceKey(source)}>{sourceLabel(source)}</option>
            ))}
          </select>
          <Button variant="ghost" size="sm" className="h-6 px-1.5" aria-label={t("setup.refresh")} title={t("setup.refresh")}
            disabled={loadingSources || starting} onClick={() => {
              setError(null);
              setLoadingSources(true);
              setSourceAttempt((attempt) => attempt + 1);
            }}><RefreshCw size={13} aria-hidden="true" /></Button>
        </div>
      )}
      {video && !loadingSources && sourceFailure == null && screens.length === 0 && (
        <p role="status" className="mt-1 text-[10px] text-ink-3">{t("setup.noSources")}</p>
      )}
      {settings && (
        <div className="mt-2 text-[11px] leading-4 text-ink-3">
          <p>{settings.capture_system ? t("popup.outputAudio", { output, mic }) : t("popup.micOnly")}</p>
          <p>{t("popup.speechLanguage", {
            language: speechLanguageLabel(settings.language, i18n.resolvedLanguage ?? "en", t("main:settings.autoDetect")),
          })}</p>
        </div>
      )}
      {discordStatus?.bot && <div className="mt-2 space-y-1 border-t border-line pt-1.5 text-[11px] text-ink-2">
        <div className="flex flex-wrap items-center justify-between gap-x-2">
          <label className="inline-flex shrink-0 items-center gap-1.5">
            <input type="checkbox" role="switch" checked={discordEnabled} disabled={starting} onChange={(event) => {
              setDiscordEnabled(event.target.checked);
              setBrowsingDiscord(event.target.checked && !discordChannel);
            }} /> {t("popup.discordNames")}
          </label>
          {discordEnabled && <Button type="button" variant="ghost" size="sm" className="h-6 px-1.5 text-[11px]" disabled={starting} aria-expanded={browsingDiscord} onClick={() => setBrowsingDiscord(!browsingDiscord)}>
            {t(browsingDiscord ? "popup.discordHideChannels" : "popup.discordChangeChannel")}
          </Button>}
        </div>
        {discordEnabled && <>
          {discordChannel && <p className="truncate text-ink-3" title={`${discordChannel.guild_name} / ${discordChannel.channel_name}`}>{discordChannel.guild_name} / {discordChannel.channel_name}</p>}
          <p className="text-ink-3">{t("popup.discordJoins")}</p>
          {browsingDiscord && <DiscordChannelPicker channel={discordChannel} onChange={setDiscordChannel} compact disabled={starting} />}
        </>}
      </div>}
      {attendeeConfigured && <div className="mt-2 space-y-1 border-t border-line pt-1.5 text-[11px] text-ink-2">
        <label className="inline-flex items-center gap-1.5">
          <input type="checkbox" role="switch" checked={attendeeEnabled} disabled={starting} onChange={(event) => setAttendeeEnabled(event.target.checked)} /> {t("popup.attendeeTracks")}
        </label>
        {attendeeEnabled && <>
          <label className="flex items-center gap-2">
            <span className="shrink-0">{t("popup.attendeeLink")}</span>
            <Input type="url" autoComplete="off" spellCheck={false} value={meetUrl} disabled={starting} aria-invalid={Boolean(meetUrl.trim()) && !attendeeReady} placeholder="https://meet.google.com/…" className="h-6 min-w-0 px-1.5 text-[11px]" onChange={(event) => setMeetUrl(event.target.value)} />
          </label>
          <p className="text-ink-3">{t(meetUrl.trim() && !attendeeReady ? "popup.attendeeInvalidLink" : "popup.attendeeJoins")}</p>
        </>}
      </div>}
      {video && sourceFailure != null && <p role="alert" className="mt-1 text-[11px] text-ember">{t("popup.listScreensFailed", { detail: displayError(sourceFailure) })}</p>}
      {error && <p role="alert" className="mt-1 text-[11px] text-ember">{error.kind === "record" ? displayError(error.detail) : t("popup.loadSettingsFailed", { detail: displayError(error.detail) })}</p>}
      {settings?.stt_provider === "soniox" && <p className="mt-1 text-[10px] leading-4 text-ink-3">{t("popup.cloud")}</p>}
      <div className="mt-auto flex shrink-0 items-center gap-2 pt-2">
        <Button variant="ember" size="md" className="flex-1" disabled={!canStart} onClick={() => void record()}>
          <Circle size={12} fill="currentColor" /> {starting ? t("popup.starting") : t("popup.record")}
        </Button>
        <Button variant="ghost" size="md" onClick={() => void cmd.dismissDetection(det.app, "now")}>
          {t("popup.notNow")} <span className="ml-1 font-mono text-[11px] text-ink-3">{left}</span>
        </Button>
        <Button variant="ghost" size="md" onClick={() => void cmd.dismissDetection(det.app, "never")} title={t("popup.neverHint")}>
          {t("popup.never")}
        </Button>
      </div>
    </div>
  );
}

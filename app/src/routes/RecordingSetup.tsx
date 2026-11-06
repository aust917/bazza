import {
  ArrowLeft, AudioLines, Bot, Check, ChevronDown, Circle, FileText, Headphones, Languages, ListChecks,
  Mic, Monitor, RefreshCw, Settings as SettingsIcon, Video, VideoOff, Volume2,
} from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { DiscordChannelPicker } from "@/components/DiscordChannelPicker";
import { Button, Input, Menu, Spinner } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { sourceLabel } from "@/lib/format";
import { speechLanguageLabel } from "@/lib/languages";
import { screenSourceKey } from "@/lib/recordingSetup";
import { useRecordingSetup, type RecordingMode } from "./useRecordingSetup";
import "./RecordingSetup.css";

const MODES: RecordingMode[] = ["standard", "meet", "discord"];
type Setup = ReturnType<typeof useRecordingSetup>;

export default function RecordingSetup({ serviceDesktop = false, settingsAvailable = true }: {
  serviceDesktop?: boolean;
  settingsAvailable?: boolean;
}) {
  const { t } = useTranslation("recording");
  const { state } = useLocation();
  const navigate = useNavigate();
  const setup = useRecordingSetup();
  const title = useRef<HTMLHeadingElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const mounted = useRef(false);
  const startInFlight = useRef(false);
  const [startAt, setStartAt] = useState<number>();
  const [botActionAt, setBotActionAt] = useState<number>();
  const active = setup.recording != null && setup.recording.state !== "idle";
  const loading = setup.loadingSettings || (setup.recording == null && !setup.statusError);
  const botPhase = setup.botStatus?.phase;
  const botReady = botPhase === "connected" && !setup.botDisconnecting;
  const botPending = botPhase === "connecting" || botPhase === "waiting_admission";
  const botAction = setup.botDisconnecting ? "disconnectingBot" : botReady ? "disconnectBot" : botPending ? "cancelBotConnection"
    : botPhase === "failed" ? "retryBotConnection" : "connectBot";

  useEffect(() => {
    mounted.current = true;
    title.current?.focus();
    return () => { mounted.current = false; };
  }, []);

  function changeMode(next: RecordingMode) {
    setBotActionAt(undefined);
    setup.chooseMode(next);
    if (scroller.current) scroller.current.scrollTop = 0;
  }

  async function start() {
    if (startInFlight.current) return;
    startInFlight.current = true;
    setStartAt(performance.now());
    try {
      const meeting = await setup.record(botReady);
      if (meeting && mounted.current) navigate(`/meeting/${meeting.id}`, { replace: true });
    } finally {
      startInFlight.current = false;
      if (mounted.current) setStartAt(undefined);
    }
  }

  function performBotAction() {
    setBotActionAt(performance.now());
    if (botReady || botPending) setup.disconnectBot();
    else setup.connectBot();
  }

  return (
    <div className="recording-setup">
      <header className="recording-setup__header">
        <div className="recording-setup__width recording-setup__heading">
          <div className="flex min-w-0 items-center gap-3.5">
            <span className="grid size-12 shrink-0 place-items-center rounded-lg border border-ember/15 bg-ember-soft text-ember" aria-hidden="true">
              <AudioLines size={23} />
            </span>
            <h1 ref={title} tabIndex={-1} className="text-[26px] leading-tight font-extrabold tracking-tight outline-none">{t("setup.title")}</h1>
          </div>
          <fieldset className="recording-setup__modes" disabled={setup.busy}>
            <legend className="mb-1.5 text-xs font-semibold text-ink-2">{t("setup.mode")}</legend>
            <div className="recording-setup__mode-options">
              {MODES.map((mode) => (
                <Choice key={mode} name="recording-mode" checked={setup.mode === mode} onChange={() => changeMode(mode)} label={t(`setup.${mode}`)} />
              ))}
              <Choice name="recording-mode" checked={false} disabled label={t("setup.zoom")} detail={t("setup.zoomUnavailable")} />
            </div>
          </fieldset>
        </div>
      </header>

      <div className="recording-setup__scroll" ref={scroller}>
        <div className="recording-setup__width recording-setup__content">
          {active && <p role="status" className="recording-setup__notice"><Circle size={12} fill="currentColor" className="text-ember" />{t("setup.active")}</p>}
          {(setup.settingsError || setup.statusError) && (
            <div role="alert" className="recording-setup__notice">
              <div className="min-w-0 break-words text-ember">
                {setup.settingsError && <p>{setup.settingsError}</p>}
                {setup.statusError && <p>{t("setup.statusFailed", { detail: setup.statusError })}</p>}
              </div>
              <Button variant="outline" size="sm" onClick={setup.retry}>{t("setup.retry")}</Button>
            </div>
          )}
          {loading && <p role="status" className="recording-setup__notice"><Spinner />{t("setup.loading")}</p>}
          {setup.settings && (
            <div className="recording-setup__workspace">
              <div className="recording-setup__panels">
                {setup.mode !== "standard" && <BotPanel setup={setup} startedAt={botActionAt} settingsAvailable={settingsAvailable} key={setup.mode} />}
                <PicturePanel setup={setup} />
                <AudioPanel setup={setup} />
              </div>
              <Review setup={setup} language={setup.settings.language} />
            </div>
          )}
        </div>
      </div>

      <footer className="recording-setup__footer">
        <div className="recording-setup__width">
          {setup.error && <p role="alert" className="mb-2 break-words text-sm text-ember">{setup.error}</p>}
          {setup.starting && <OperationProgress label={t("setup.preparingRecording")} startedAt={startAt} announceStage className="mb-2 ml-auto max-w-xs" />}
          {!active && setup.settings?.stt_provider === "soniox" && (
            <p className="mb-2 text-xs text-ink-2">{t(serviceDesktop && botReady ? "setup.namedLocalCaptions" : "popup.cloud")}</p>
          )}
          <div className="recording-setup__actions">
            <Link className="focus-ring inline-flex min-h-10 items-center gap-2 rounded-md px-3 text-sm font-semibold text-ink-2 hover:bg-canvas-3 hover:text-ink aria-disabled:opacity-50" to={state?.returnTo ?? "/"} replace
              aria-disabled={setup.starting || undefined} onClick={(event) => { if (setup.starting) event.preventDefault(); }}>
              <ArrowLeft size={16} aria-hidden="true" />{t("popup.cancel")}
            </Link>
            <div className="flex flex-wrap items-center justify-end gap-2">
              {active && setup.recording?.meeting_id ? (
                <Button variant="primary" size="lg" onClick={() => navigate(`/meeting/${setup.recording?.meeting_id}`, { replace: true })}>{t("setup.openActive")}</Button>
              ) : (
                <div className="flex flex-wrap items-center justify-end gap-2">
                  {setup.settings && setup.mode !== "standard" && !active && (
                    <>
                      {!botReady && <p className="text-xs text-ink-2" id="recording-start-hint">{t("setup.startWithoutBot")}</p>}
                      <Button variant="outline" size="lg" disabled={setup.busy || setup.botDisconnecting || (!botReady && !botPending && !setup.canConnect)}
                        aria-describedby={botPhase || setup.botError ? "recording-bot-status" : undefined}
                        onClick={performBotAction}>
                        {botPending || setup.botDisconnecting ? <span aria-hidden="true"><Spinner /></span> : <Bot size={16} aria-hidden="true" />}
                        {t(`setup.${botAction}`)}
                      </Button>
                    </>
                  )}
                  <Button variant="ember" size="lg" disabled={!setup.canRecord} aria-describedby={setup.settings && setup.mode !== "standard" && !active && !botReady ? "recording-start-hint" : undefined} onClick={() => void start()}>
                    {setup.starting ? <Spinner /> : <Circle size={13} fill="currentColor" aria-hidden="true" />}
                    {t(setup.starting ? "popup.starting" : botReady ? "setup.startWithBot" : "setup.start")}
                  </Button>
                </div>
              )}
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}

function PicturePanel({ setup }: { setup: Setup }) {
  const { t } = useTranslation("recording");
  const screenLabel = t("popup.screenToRecord");
  const screenValue = setup.loadingSources ? t("setup.loadingSources")
    : setup.selectedScreen ? sourceLabel(setup.selectedScreen) : t("setup.chooseSource");

  return (
    <Panel title={t("setup.picture")} icon={<Monitor size={18} />}>
      <fieldset disabled={setup.busy}>
        <legend className="sr-only">{t("setup.format")}</legend>
        <div className="recording-setup__options">
          <Choice name="format" checked={setup.video} onChange={() => setup.setVideo(true)} icon={<Video size={17} />} label={t("setup.videoAudio")} />
          <Choice name="format" checked={!setup.video} onChange={() => setup.setVideo(false)} icon={<Headphones size={17} />} label={t("popup.audioOnly")} />
        </div>
      </fieldset>
      {setup.video && (
        <div className="recording-setup__group">
          <div className="mb-1.5 flex flex-wrap items-center justify-between gap-x-2">
            <span className="text-xs font-semibold text-ink-2">{screenLabel}</span>
            <Button variant="ghost" size="sm" disabled={setup.busy || setup.loadingSources} onClick={setup.refreshSources}>
              <RefreshCw size={14} aria-hidden="true" />{t("setup.refresh")}
            </Button>
          </div>
          <Menu inline className="recording-setup__source-picker"
            disabled={setup.busy || setup.loadingSources || Boolean(setup.sourceError) || setup.screens.length === 0}
            label={<>
              <span className="sr-only">{screenLabel}</span>
              <span className="min-w-0 truncate" title={screenValue}>{screenValue}</span>
              <ChevronDown size={16} aria-hidden="true" className="shrink-0 text-ink-2" />
            </>}
            items={setup.screens.map((source) => {
              const key = screenSourceKey(source);
              const checked = key === setup.screenKey;
              return {
                checked,
                label: <span className="flex items-center justify-between gap-2 break-words">
                  <span className="min-w-0">{sourceLabel(source)}</span>
                  {checked && <Check size={15} aria-hidden="true" className="shrink-0 text-ember" />}
                </span>,
                onSelect: () => setup.chooseScreen(key),
              };
            })}
          />
          {setup.sourceError ? <p role="alert" className="mt-2 break-words text-sm text-ember">{setup.sourceError}</p>
            : !setup.loadingSources && setup.screens.length === 0 ? <p role="status" className="mt-2 text-sm text-ink-2">{t("setup.noSources")}</p> : null}
        </div>
      )}
    </Panel>
  );
}

function AudioPanel({ setup }: { setup: Setup }) {
  const { t } = useTranslation("recording");
  return (
    <Panel title={t("setup.computerAudio")} icon={<Volume2 size={18} />}>
      <p className="text-sm text-ink-2">
        {setup.settings?.capture_system ? t("setup.outputHint", { output: setup.output }) : t("popup.micOnly")}
      </p>
      <div className="recording-setup__device">
        <Mic size={16} aria-hidden="true" className="shrink-0 text-ink-2" />
        <span className="shrink-0 font-semibold">{t("setup.mic")}</span>
        <span className="min-w-0 truncate text-ink-2" title={setup.mic}>{setup.mic}</span>
      </div>
    </Panel>
  );
}

function BotPanel({ setup, startedAt, settingsAvailable }: { setup: Setup; startedAt?: number; settingsAvailable: boolean }) {
  const { t } = useTranslation("recording");
  const navigate = useNavigate();
  const meet = setup.mode === "meet";
  const [browsingDiscord, setBrowsingDiscord] = useState(false);
  const [discordLink, setDiscordLink] = useState("");
  const [usingLink, setUsingLink] = useState(false);
  const invalidMeetLink = Boolean(setup.meetUrl.trim()) && !setup.attendeeReady;
  const botBusy = setup.botDisconnecting || setup.botStatus?.phase === "connecting" || setup.botStatus?.phase === "waiting_admission";
  const botFailed = setup.botStatus?.phase === "failed" || Boolean(setup.botError);
  const botLabel = botStatusLabel(setup, t);

  async function applyLink(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (await setup.chooseDiscordLink(discordLink)) {
      setUsingLink(false);
      setBrowsingDiscord(false);
      setDiscordLink("");
    }
  }

  return (
    <Panel title={t(meet ? "setup.meetTracks" : "setup.discordTracks")} icon={<Bot size={18} />} accent>
      {setup.integrationLoading ? (
        <p role="status" className="flex items-center gap-2 text-sm text-ink-2"><Spinner />{t("setup.integrationLoading")}</p>
      ) : setup.integrationError ? (
        <div className="space-y-2">
          <p role="alert" className="break-words text-sm text-ember">{setup.integrationError}</p>
          <Button variant="outline" size="sm" disabled={setup.busy} onClick={setup.retryIntegration}>{t("setup.retry")}</Button>
        </div>
      ) : !setup.integrationConfigured ? (
        <div className="space-y-2">
          <p className="text-sm text-ink-2">{t(meet ? "setup.meetNeedsSetup" : "setup.discordNeedsSetup")}</p>
          {settingsAvailable && (
            <Button variant="outline" size="sm" onClick={() => navigate(meet ? "/settings#google-meet" : "/settings#discord")}>
              <SettingsIcon size={14} aria-hidden="true" />
              {t("setup.openSettings")}
            </Button>
          )}
        </div>
      ) : meet ? (
        <div className="space-y-2">
          <label className="block text-xs font-semibold text-ink-2" htmlFor="recording-meet-link">{t("popup.attendeeLink")}</label>
          <Input id="recording-meet-link" type="url" autoComplete="off" spellCheck={false} value={setup.meetUrl} disabled={setup.busy}
            aria-invalid={invalidMeetLink}
            aria-describedby={invalidMeetLink ? "recording-meet-hint recording-meet-error" : "recording-meet-hint"}
            placeholder="https://meet.google.com/…" onChange={(event) => setup.setMeetUrl(event.target.value)} />
          {invalidMeetLink && <p id="recording-meet-error" className="text-sm text-ember">{t("popup.attendeeInvalidLink")}</p>}
          <p id="recording-meet-hint" className="text-sm text-ink-2">{t("setup.meetBotHint")}</p>
        </div>
      ) : (
        <div className="space-y-2">
          {setup.discordChannel && (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="break-words text-sm font-semibold">{setup.discordChannel.guild_name} / {setup.discordChannel.channel_name}</p>
              <Button variant="outline" size="sm" disabled={setup.busy || setup.discordChecking}
                aria-expanded={browsingDiscord} onClick={() => setBrowsingDiscord(!browsingDiscord)}>
                {t(browsingDiscord ? "popup.discordHideChannels" : "setup.discordChangeChannel")}
              </Button>
            </div>
          )}
          {(!setup.discordChannel || browsingDiscord) && (
            <DiscordChannelPicker key={setup.discordPickerRevision} channel={setup.discordChannel} disabled={setup.busy || setup.discordChecking} onChange={(channel) => {
              setup.setDiscordChannel(channel);
              setup.clearDiscordLinkError();
            }} />
          )}
          <Button variant="ghost" size="sm" disabled={setup.busy || setup.discordChecking} aria-expanded={usingLink} onClick={() => setUsingLink(!usingLink)}>
            {t(usingLink ? "setup.discordHideLink" : "setup.discordPasteLink")}
          </Button>
          {usingLink && (
            <form className="space-y-2" noValidate onSubmit={(event) => void applyLink(event)}>
              <label className="block text-xs font-semibold text-ink-2" htmlFor="recording-discord-link">{t("setup.discordLink")}</label>
              <div className="flex flex-wrap items-center gap-2">
                <Input id="recording-discord-link" className="min-w-36 flex-1" type="url" autoComplete="off" spellCheck={false}
                  value={discordLink} placeholder="https://discord.com/channels/…" disabled={setup.busy || setup.discordChecking}
                  aria-invalid={Boolean(setup.discordLinkError)}
                  aria-describedby={setup.discordLinkError ? "recording-discord-error" : undefined}
                  onChange={(event) => { setDiscordLink(event.target.value); setup.clearDiscordLinkError(); }} />
                <Button type="submit" variant="outline" disabled={!discordLink.trim() || setup.busy || setup.discordChecking}>
                  {t(setup.discordChecking ? "setup.discordLinkChecking" : "setup.discordUseLink")}
                </Button>
              </div>
              {setup.discordLinkError && <p id="recording-discord-error" role="alert" className="break-words text-sm text-ember">{setup.discordLinkError}</p>}
            </form>
          )}
        </div>
      )}
      {botBusy && !botFailed && botLabel
        ? <OperationProgress id="recording-bot-status" label={botLabel} startedAt={startedAt} announceStage className="mt-3" />
        : (botLabel || setup.botError) && <p id="recording-bot-status" role={botFailed ? "alert" : "status"}
            className={botFailed ? "mt-3 break-words text-sm text-ember" : "mt-3 text-sm text-ink-2"}>
            {[botLabel, setup.botError].filter(Boolean).join(" ")}
          </p>}
    </Panel>
  );
}

function Review({ setup, language }: { setup: Setup; language: string | null }) {
  const { t, i18n } = useTranslation(["recording", "main"]);
  const selected = setup.selectedScreen ? sourceLabel(setup.selectedScreen) : null;
  const localAudio = setup.settings?.capture_system
    ? t("setup.micAndOutput", { output: setup.output }) : t("setup.mic");
  const botDetail = setup.botStatus?.phase === "connected" && !setup.botDisconnecting
    ? t("setup.botReadyTracks")
    : t("setup.botTracksOff");

  return (
    <section className="recording-setup__review" aria-labelledby="recording-summary">
      <div className="recording-setup__review-header">
        <h2 id="recording-summary" className="flex items-center gap-2 text-lg font-extrabold tracking-tight">
          <ListChecks size={18} className="shrink-0 text-ember" aria-hidden="true" />{t("setup.summary")}
        </h2>
      </div>
      <dl className="recording-setup__summary" aria-live="polite">
        <SummaryItem icon={setup.video ? <Video size={17} /> : <VideoOff size={17} />} label={t("setup.summaryVideo")} value={setup.video ? selected ?? t("setup.chooseSource") : t("setup.off")} />
        <SummaryItem icon={<Volume2 size={17} />} label={t("setup.fileAudio")} value={localAudio} />
        <SummaryItem icon={<FileText size={17} />} label={t("setup.transcript")} value={t("setup.initialTranscriptSource")} />
        <SummaryItem icon={<Languages size={17} />} label={t("main:settings.spokenLanguage")}
          value={speechLanguageLabel(language, i18n.resolvedLanguage ?? "en", t("main:settings.autoDetect"))} />
        {setup.mode !== "standard" && <SummaryItem icon={<Bot size={17} />} label={t("setup.participantTracks")} value={botDetail} />}
      </dl>
    </section>
  );
}

function botStatusLabel(setup: Setup, t: ReturnType<typeof useTranslation<"recording">>["t"]): string | null {
  if (setup.botDisconnecting) return t("setup.disconnectingBot");
  switch (setup.botStatus?.phase) {
    case "connecting": return t("setup.botConnecting");
    case "waiting_admission": return t("setup.botWaitingAdmission");
    case "connected": return t("setup.botConnected");
    case "failed": return t("setup.botConnectionFailed");
    case "disconnected": return t("setup.botDisconnected");
    default: return null;
  }
}

function SummaryItem({ icon, label, value }: { icon: ReactNode; label: string; value: string }) {
  return (
    <div className="recording-setup__summary-item">
      <span aria-hidden="true">{icon}</span>
      <div className="min-w-0">
        <dt className="text-xs font-semibold text-ink-2">{label}</dt>
        <dd className="mt-0.5 break-words text-sm font-bold">{value}</dd>
      </div>
    </div>
  );
}

function Panel({ title, icon, children, accent }: { title: string; icon: ReactNode; children: ReactNode; accent?: boolean }) {
  return (
    <section className={accent ? "recording-setup__panel recording-setup__panel--bot" : "recording-setup__panel"}>
      <h2 className="recording-setup__panel-title">
        <span aria-hidden="true">{icon}</span>{title}
      </h2>
      {children}
    </section>
  );
}

function Choice({ name, checked, disabled, onChange, icon, label, detail }: {
  name: string; checked: boolean; disabled?: boolean; onChange?: () => void; icon?: ReactNode; label: string; detail?: string;
}) {
  const descriptionId = useId();
  return (
    <label className="recording-setup__choice">
      <input type="radio" name={name} className="sr-only" checked={checked} disabled={disabled} onChange={onChange} aria-label={label} aria-describedby={detail ? descriptionId : undefined} />
      <span className="recording-setup__choice-body">
        <span className="flex items-center gap-2 font-bold">
          {icon && <span aria-hidden="true" className="shrink-0">{icon}</span>}
          <span className="min-w-0 wrap-anywhere">{label}</span>
          <span className="recording-setup__check" aria-hidden="true"><Check size={12} strokeWidth={3} /></span>
        </span>
        {detail && <span id={descriptionId} className="mt-1 block text-[13px] text-ink-2">{detail}</span>}
      </span>
    </label>
  );
}

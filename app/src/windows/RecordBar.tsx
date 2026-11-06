import { AlertTriangle, Bookmark, Pause, Play, Square } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Meter } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { cn } from "@/lib/cn";
import { fmtDuration } from "@/lib/format";
import { cmd, subscribe, type Events, type RecordingState } from "@/lib/ipc";
import { displayError, displayMessage } from "@/lib/i18n";
import { useBotRecordingStatus } from "@/lib/useBotRecordingStatus";
import { captionDisplayName, captionSpeakerKey, useLiveFeed } from "@/lib/useLiveFeed";

function reportsVideoUnavailable(notice: Events["notice"]) {
  if (notice.code === "native.screenStartFailed" || notice.code === "native.screenCaptureInterrupted" || notice.code === "native.recordingAudioOnly") return true;
  if (notice.code !== "native.captureWarning") return false;
  // The generic capture notice also carries audio warnings. These legacy diagnostics
  // specifically describe losing video, while recording can continue.
  return /^Screen capture (stopped unexpectedly|is unavailable:)/.test(notice.detail ?? notice.message);
}

/** Slim always-on-top bar shown while recording. */
export function RecordBar({ serviceDesktop = false }: { serviceDesktop?: boolean }) {
  const { t } = useTranslation("recording");
  const [rec, setRec] = useState<RecordingState>({ state: "idle", meeting_id: null, elapsed_ms: 0 });
  const [levels, setLevels] = useState<Events["levels"]>({ mic_db: -100, sys_db: -100 });
  const [marked, setMarked] = useState(false);
  const [notice, setNotice] = useState<Events["notice"] | null>(null);
  const [lostAudio, setLostAudio] = useState<Partial<Record<"system" | "mic", Events["notice"]>>>({});
  const [actionError, setActionError] = useState<string | null>(null);
  const [stopStartedAt, setStopStartedAt] = useState<number>();
  const stopping = useRef(false);
  const { attendeePhase, discordState } = useBotRecordingStatus(rec.meeting_id);
  const live = useLiveFeed(rec.meeting_id, 2);

  useEffect(() => {
    void cmd.recordingStatus().then(setRec);
    const a = subscribe("recording_state", (s) => {
      setRec(s);
      if (s.state === "idle") {
        setLevels({ mic_db: -100, sys_db: -100 });
        setNotice(null);
        setLostAudio({});
        setActionError(null);
      }
    });
    const b = subscribe("levels", setLevels);
    const d = subscribe("notice", (message) => {
      switch (message.code) {
        case "native.systemAudioUnavailable": setLostAudio((state) => ({ ...state, system: message })); return;
        case "native.microphoneUnavailable": setLostAudio((state) => ({ ...state, mic: message })); return;
        case "native.systemAudioReconnected": setLostAudio((state) => ({ ...state, system: undefined })); break;
        case "native.microphoneReconnected": setLostAudio((state) => ({ ...state, mic: undefined })); break;
      }
      if (message.level !== "info") setNotice(message);
    });
    const t = setInterval(() => void cmd.recordingStatus().then(setRec), 1000);
    return () => {
      a();
      b();
      d();
      clearInterval(t);
    };
  }, []);

  const paused = rec.state === "paused";
  const active = rec.state === "recording" || paused;
  const captionUnavailable = live?.caption_unavailable;
  const captionStatusLabel = active && captionUnavailable
    ? t(captionUnavailable === "model_missing" ? "bar.captionsModelMissing" : "bar.captionsModelLoadFailed")
    : null;
  const retryStop = rec.state === "pending";
  const saving = stopStartedAt !== undefined || rec.state === "finishing";
  const recordingAudio = rec.state === "recording" && !saving;
  const currentNotice = lostAudio.system ?? lostAudio.mic ?? notice;
  const videoUnavailable = currentNotice !== null && reportsVideoUnavailable(currentNotice);
  const noticeText = videoUnavailable
    ? t(recordingAudio ? "bar.videoUnavailableAudioContinues" : "bar.videoUnavailable")
    : currentNotice ? displayMessage(currentNotice) : null;
  const noticeTitle = currentNotice && (!videoUnavailable || recordingAudio) ? displayMessage(currentNotice) : noticeText;
  const captionRows = saving || ((actionError || currentNotice) && discordState && attendeePhase) ? 1 : 2;
  const partials = live?.partials ?? [];
  const recentPartials = partials.length > captionRows ? [...partials].sort((a, b) => a.end_ms - b.end_ms) : partials;
  const visibleCaptions = live ? [...live.finals, ...recentPartials].slice(-captionRows) : [];
  const perform = async (action: () => Promise<unknown>) => {
    setActionError(null);
    try { await action(); }
    catch (error) { setActionError(displayError(error)); }
  };

  const mark = async () => {
    await cmd.markHighlight();
    setMarked(true);
    setTimeout(() => setMarked(false), 900);
  };
  const stop = async () => {
    if (stopping.current || saving) return;
    stopping.current = true;
    setStopStartedAt(performance.now());
    try { await perform(cmd.stopRecording); }
    finally { stopping.current = false; setStopStartedAt(undefined); }
  };

  return (
    <div className="flex h-full flex-col rounded-xl border border-line bg-canvas/95 px-3 shadow-float backdrop-blur" data-tauri-drag-region>
      <div className="flex min-h-0 flex-1 items-center gap-3" data-tauri-drag-region>
        <div className="flex w-[84px] shrink-0 items-center gap-2" data-tauri-drag-region>
          <span className={cn("h-2.5 w-2.5 rounded-full", paused ? "bg-amber" : "rec-dot bg-ember")} />
          <span className="font-mono text-[13px] font-medium tabular-nums">{fmtDuration(rec.elapsed_ms)}</span>
        </div>
        <div className="flex w-[94px] shrink-0 flex-col gap-1" aria-label={t("bar.levels")}>
          <Meter label={t("bar.mic")} db={paused ? -100 : levels.mic_db} />
          <Meter label={t("bar.sys")} db={paused ? -100 : levels.sys_db} />
        </div>
        <div className="min-w-0 flex-1 text-[12px] leading-4 text-ink-2" data-tauri-drag-region>
          {saving && <OperationProgress label={t("bar.saving")} startedAt={stopStartedAt} />}
          {!saving && discordState && <p role="status" className={cn("truncate text-[10px] leading-3", discordState === "unavailable" ? "text-amber" : "text-ink-3")}>{t(`discordStatus.${discordState}`)}</p>}
          {!saving && attendeePhase && <p role="status" className={cn("truncate text-[10px] leading-3", attendeePhase === "failed" ? "text-amber" : "text-ink-3")}>{t(`attendeeStatus.${attendeePhase}`)}</p>}
          {paused && !saving ? (
            <span className="text-ink-3">{t("bar.paused")}</span>
          ) : visibleCaptions.length === 0 ? (
            !saving && (captionStatusLabel ? (
              <span role="status" title={captionStatusLabel} className="block truncate text-ink-3">{captionStatusLabel}</span>
            ) : <span className="text-ink-3">{t("bar.listening")}</span>)
          ) : (
            visibleCaptions.map((c, i) => {
              const last = i === visibleCaptions.length - 1;
              const speaker = captionDisplayName(c, t("bar.recordingAudio"));
              return (
                <div key={c.is_final ? `${c.start_ms}-${i}` : `partial-${captionSpeakerKey(c)}`} aria-live={last || (speaker && !c.is_final) ? "polite" : undefined} className={cn("flex gap-1", last ? "text-ink" : "text-ink-3")}>
                  {speaker && <><strong className="max-w-[40%] shrink-0 truncate font-semibold" title={speaker}>{speaker}</strong>{" "}</>}
                  <span className="min-w-0 truncate">{c.text}</span>
                </div>
              );
            })
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {!serviceDesktop && <IconBtn title={t("bar.highlight")} aria-label={t("bar.highlight")} disabled={saving} onClick={() => void perform(mark)} className={marked ? "text-amber" : ""}>
            <Bookmark size={15} fill={marked ? "currentColor" : "none"} />
          </IconBtn>}
          <IconBtn title={paused ? t("bar.resume") : t("bar.pause")} aria-label={paused ? t("bar.resume") : t("bar.pause")} disabled={saving || (serviceDesktop && !active)} onClick={() => void perform(paused ? cmd.resumeRecording : cmd.pauseRecording)}>
            {paused ? <Play size={15} /> : <Pause size={15} />}
          </IconBtn>
          <button
            disabled={saving || (serviceDesktop && !active && !retryStop)}
            onClick={() => void stop()}
            className="focus-ring ml-1 inline-flex h-8 items-center gap-1.5 rounded-md bg-ember px-3 text-[12px] font-semibold text-white hover:bg-ember-2"
          >
            <Square size={11} fill="currentColor" /> {t(retryStop ? "bar.retryStop" : "bar.stop")}
          </button>
        </div>
      </div>
      {(actionError || noticeText) && (
        <div role="alert" className="shrink-0 pb-1 text-[12px] leading-4 text-amber">
          {actionError ? (
            <span className="flex items-center gap-1" title={actionError}>
              <AlertTriangle size={13} className="shrink-0" aria-hidden="true" />
              <span className="truncate">{actionError}</span>
            </span>
          ) : (
            <button type="button" title={noticeTitle ?? undefined} onClick={() => void cmd.openMain()} className="focus-ring flex w-full items-center gap-1 rounded-sm text-left">
              <AlertTriangle size={13} className="shrink-0" aria-hidden="true" />
              <span className="truncate">{noticeText}</span>
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function IconBtn({ children, className, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button className={cn("focus-ring grid h-8 w-8 place-items-center rounded-md text-ink-2 hover:bg-canvas-3 hover:text-ink", className)} {...props}>
      {children}
    </button>
  );
}

import { AlertTriangle, AudioLines } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui";
import { fmtDuration, fmtStamp } from "@/lib/format";
import { displayError } from "@/lib/i18n";
import { cmd, type BotTranscript, type BotTranscriptJob, type BotTranscriptProvider, type BotTranscriptSegment, type BotTranscriptSource } from "@/lib/ipc";

type TranscriptSource = BotTranscriptProvider | null;

const PROVIDERS: Record<BotTranscriptProvider, string> = { meet: "Google Meet", discord: "Discord" };

function recordingTimestamp(row: BotTranscriptSegment, durationMs: number): number | null {
  const { playback_start_ms: start, playback_end_ms: end } = row;
  if (typeof start !== "number" || !Number.isFinite(start) || start < 0 || start >= durationMs) return null;
  if (end != null && (!Number.isFinite(end) || end < start || end > durationMs)) return null;
  return start;
}

function BotTranscriptText({ transcript, recordingDurationMs, onSeek }: {
  transcript: BotTranscript;
  recordingDurationMs: number;
  onSeek?: (ms: number) => void;
}) {
  const { t } = useTranslation("meeting");
  const labels = useMemo(() => {
    const participants = new Map<string, string | null>();
    for (const row of transcript.segments) {
      if (!participants.has(row.speaker_id)) participants.set(row.speaker_id, row.speaker_name);
    }
    const counts = new Map<string, number>();
    for (const name of participants.values()) {
      if (name) counts.set(name, (counts.get(name) ?? 0) + 1);
    }
    return new Map(Array.from(participants, ([id, name], index) => [
      id,
      !name ? t("botTranscript.unnamed", { number: index + 1 })
        : counts.get(name) === 1 ? name
        : t("botTranscript.duplicateName", { name, number: index + 1 }),
    ]));
  }, [t, transcript]);
  const timestamps = transcript.segments.map((row) =>
    onSeek && Number.isFinite(recordingDurationMs) ? recordingTimestamp(row, recordingDurationMs) : null
  );
  const botTiming = transcript.timing === "approximate" ? "approximateTiming" : "relativeTiming";
  const timing = timestamps.some((ms) => ms !== null)
    ? timestamps.every((ms) => ms !== null) ? "recordingTiming" : "mixedTiming"
    : botTiming;

  return (
    <div className="space-y-2">
      {transcript.partial && (
        <p className="flex items-start gap-2 rounded-md border border-amber/40 bg-amber-soft px-3 py-2 text-[13px] text-ink">
          <AlertTriangle size={14} aria-hidden="true" className="mt-0.5 shrink-0" /> {t("botTranscript.partial")}
        </p>
      )}
      {transcript.segments.length === 0 ? <p className="text-sm text-ink-3">{t("botTranscript.empty")}</p> : (
        <>
          <p className="text-[12px] text-ink-3">{t(`botTranscript.${timing}`)}</p>
          <ol className="divide-y divide-line" aria-label={t("botTranscript.utterances")}>
            {transcript.segments.map((row, index) => {
              const recordingMs = timestamps[index];
              const timeMs = recordingMs ?? row.start_ms;
              const stamp = fmtStamp(timeMs);
              const time = <time dateTime={`PT${timeMs / 1000}S`}>{stamp}</time>;
              return (
                <li key={`${row.speaker_id}-${row.start_ms}-${index}`} className="grid grid-cols-[4.5rem_minmax(0,1fr)] gap-2 py-2">
                  <div className="font-mono text-[12px] text-ink-3">
                    {recordingMs !== null && onSeek ? (
                      <button type="button" className="focus-ring cursor-pointer rounded-sm text-ink-2 underline underline-offset-2 hover:text-ink"
                        aria-label={t("botTranscript.playAt", { time: stamp })} onClick={() => onSeek(recordingMs)}>
                        {time}
                      </button>
                    ) : time}
                  </div>
                  <div className="min-w-0 break-words">
                    <p className="text-[12px] font-semibold text-ink">{labels.get(row.speaker_id)}</p>
                    <p className="text-sm leading-relaxed text-ink-2">{row.text}</p>
                  </div>
                </li>
              );
            })}
          </ol>
        </>
      )}
    </div>
  );
}

const activeJob = (job: BotTranscriptJob) => !["done", "cancelled", "failed"].includes(job.stage);

function JobProgress({ job, onCancel }: { job?: BotTranscriptJob; onCancel?: () => void }) {
  const { t } = useTranslation("meeting");
  const label = t(`botTranscript.stages.${job?.stage ?? "queued"}`);
  const value = job?.stage === "done" ? 100 : job?.total && job.completed != null
    ? Math.min(99, Math.floor(100 * job.completed / job.total)) : undefined;
  const backend = job?.backend === "unverified" ? t("botTranscript.backendUnknown") : job?.backend?.toUpperCase();
  return (
    <div className="mt-2 space-y-1.5 text-[12px] text-ink-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span role="status">{label}</span>
        {job?.can_cancel && onCancel && <Button size="sm" variant="ghost" onClick={onCancel}>{t("actions.cancel")}</Button>}
      </div>
      <div role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={value}
        className="h-1.5 overflow-hidden rounded-full bg-line">
        <div className={value === undefined ? "h-full w-1/3 animate-pulse rounded-full bg-ember motion-reduce:animate-none"
          : "h-full rounded-full bg-ember transition-[width] motion-reduce:transition-none"}
          style={value === undefined ? undefined : { width: `${value}%` }} />
      </div>
      <div className="flex flex-wrap gap-x-2 text-ink-3">
        {job && <span>{t("botTranscript.elapsed", { time: fmtDuration(job.elapsed_ms) })}</span>}
        {job?.model && <span>{job.model}{backend ? ` · ${backend}` : ""}</span>}
        {value !== undefined && <span className="ml-auto tabular-nums">{value}%</span>}
      </div>
    </div>
  );
}

export const BotTranscripts = memo(function BotTranscripts({ meetingId, visible, selected, setSelected, onAvailableSources, showLoadError, recordingDurationMs, onSeek }: {
  meetingId: string;
  visible: boolean;
  selected: TranscriptSource;
  setSelected: Dispatch<SetStateAction<TranscriptSource>>;
  onAvailableSources: (sources: BotTranscriptProvider[]) => void;
  showLoadError: boolean;
  recordingDurationMs: number;
  onSeek?: (ms: number) => void;
}) {
  const { t } = useTranslation("meeting");
  const [sources, setSources] = useState<BotTranscriptSource[]>([]);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  const [errors, setErrors] = useState<Partial<Record<BotTranscriptProvider, unknown>>>({});
  const [jobStatuses, setJobStatuses] = useState<BotTranscriptJob[]>([]);
  const knownCompletions = useRef(new Set<string>());

  useEffect(() => {
    let cancelled = false;
    void cmd.botTranscriptSources(meetingId).then((next) => {
      if (cancelled) return;
      setSources(next);
      setLoadError(null);
      setSelected((current) => next.some(({ source, transcript }) => source === current && transcript !== null) ? current : null);
    }, (error: unknown) => {
      if (cancelled) return;
      setSelected(null);
      setLoadError(error);
    });
    return () => { cancelled = true; };
  }, [meetingId, attempt, setSelected]);

  const hasPendingCards = sources.some(({ transcript, track_count }) => !transcript && track_count > 0);
  const generated = useMemo(() => sources.filter(({ transcript }) => transcript !== null), [sources]);
  useEffect(() => onAvailableSources(generated.map(({ source }) => source)), [generated, onAvailableSources]);
  useEffect(() => {
    if (!visible || !hasPendingCards) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await cmd.botTranscriptJobs(meetingId);
        if (cancelled) return;
        setJobStatuses(next);
        for (const job of next) {
          if (job.stage !== "done" || knownCompletions.current.has(job.id)) continue;
          knownCompletions.current.add(job.id);
          setAttempt((n) => n + 1);
        }
      } catch { /* Historical job status may be unavailable while the saved result loads. */ }
      if (!cancelled) timer = setTimeout(() => void poll(), 650);
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [hasPendingCards, meetingId, visible]);

  if (!visible) return null;
  if (loadError !== null && showLoadError) return (
    <div className="mb-3 flex flex-wrap items-center justify-between gap-2 rounded-md border border-line bg-canvas-2 p-3">
      <p role="alert" className="min-w-0 break-words text-[13px] text-ember">{t("botTranscript.loadError", { error: displayError(loadError) })}</p>
      <Button size="sm" variant="outline" onClick={() => { setLoadError(null); setAttempt((n) => n + 1); }}>{t("actions.retry")}</Button>
    </div>
  );

  const activeTranscript = generated.find(({ source }) => source === selected)?.transcript;
  const completedJob = jobStatuses.find((entry) => entry.source === selected && entry.stage === "done");
  return (
    <>
      {sources.filter(({ transcript, track_count }) => !transcript && track_count > 0).map(({ source }) => {
        const job = jobStatuses.find((entry) => entry.source === source);
        const active = job && activeJob(job) ? job : undefined;
        const error = active || job?.stage === "cancelled" ? null : (job?.error ?? errors[source]);
        return (
          <section key={source} className="mb-3 rounded-lg border border-line bg-canvas-2 p-3">
            <div className="flex flex-wrap items-center gap-2">
              <AudioLines size={16} aria-hidden="true" className="shrink-0 text-ember" />
              <div className="min-w-0 flex-1 basis-32">
                <h2 className="text-[13px] font-semibold text-ink">{t("botTranscript.tracks", { provider: PROVIDERS[source] })}</h2>
              </div>
            </div>
            {active && <JobProgress job={active} onCancel={() => {
              void cmd.cancelBotTranscript(meetingId, source, active.id)
                .catch((error: unknown) => setErrors((previous) => ({ ...previous, [source]: error })));
            }} />}
            {job?.stage === "cancelled" && <p role="status" className="mt-2 text-[12px] text-ink-2">{t("botTranscript.stages.cancelled")}</p>}
            {error != null && <p role="alert" className="mt-2 break-words text-[13px] text-ember">{displayError(error)}</p>}
          </section>
        );
      })}
      {activeTranscript && <>
        {completedJob && <JobProgress job={completedJob} />}
        <BotTranscriptText transcript={activeTranscript} recordingDurationMs={recordingDurationMs} onSeek={onSeek} />
      </>}
    </>
  );
});

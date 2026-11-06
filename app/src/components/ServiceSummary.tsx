import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { displayError } from "@/lib/i18n";
import { cmd, type Segment, type ServiceSummaryJob, type SummaryAgent } from "@/lib/ipc";
import { Markdown } from "./Markdown";
import { SubscriptionOptions, useSubscriptionProviders } from "./SubscriptionProviders";
import { TranscriptExcerpt } from "./TranscriptExcerpt";
import { Button } from "./ui";
import { OperationProgress } from "./ui/OperationProgress";

function isRunning(job: ServiceSummaryJob | null) {
  return job?.status === "running" || job?.status === "cancelling" || job?.status === "saving";
}

interface Props {
  meetingId: string;
  segments: Segment[];
  canGenerate: boolean;
  onSeek?: (ms: number) => void;
}

export function ServiceSummary({ meetingId, segments, canGenerate, onSeek }: Props) {
  const { t } = useTranslation("meeting");
  const { providers, ready, refresh } = useSubscriptionProviders();
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<{ markdown: string | null; savedId: string | null } | { error: string } | null>(null);
  const [agent, setAgent] = useState<SummaryAgent | "">("");
  const [job, setJob] = useState<ServiceSummaryJob | null>(null);
  const [jobRefresh, setJobRefresh] = useState(0);
  const [checked, setChecked] = useState(false);
  const [readFailed, setReadFailed] = useState(false);
  const [pending, setPending] = useState(false);
  const [startedAt, setStartedAt] = useState<number>();
  const [error, setError] = useState<unknown>(null);
  const mutating = useRef(false);
  const mutationRevision = useRef(0);
  const savedId = job?.status === "succeeded" ? job.preview_id : null;
  const prepared = job?.status === "prepared" ? job : null;
  const busy = isRunning(job) || pending;
  const completed = savedId !== null && result && "markdown" in result && result.savedId === savedId && Boolean(result.markdown);
  const progressLabel = job?.status === "running" ? t("subscription.waitingFor", { provider: t(`summary.${job.agent}`) })
    : job?.status === "cancelling" ? t("subscription.cancelling")
    : job?.status === "saving" ? t("summary.serviceSaving")
    : null;
  const progressStartedAt = job?.started_at_ms == null ? startedAt : job.started_at_ms - performance.timeOrigin;
  const progressFinishedAt = job?.finished_at_ms == null ? undefined : job.finished_at_ms - performance.timeOrigin;

  useEffect(() => {
    let active = true;
    void cmd.getServiceSummary(meetingId).then(
      (data) => { if (active) setResult({ markdown: data?.markdown ?? null, savedId }); },
      (failure) => { if (active) setResult({ error: displayError(failure) }); },
    );
    return () => { active = false; };
  }, [meetingId, revision, savedId]);

  useEffect(() => {
    let active = true;
    let first = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      let again = first || mutating.current;
      first = false;
      const current = mutationRevision.current;
      if (!mutating.current) {
        try {
          const next = await cmd.getServiceSummaryJob(meetingId);
          if (active && current === mutationRevision.current) {
            setJob(next);
            setChecked(true);
            setReadFailed(false);
            again ||= isRunning(next);
          }
        } catch {
          if (active && current === mutationRevision.current) {
            setReadFailed(true);
            again = true;
          }
        }
      }
      if (active && again) timer = setTimeout(() => void poll(), 750);
    };
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [meetingId, jobRefresh]);

  const run = async (action: () => Promise<ServiceSummaryJob | null>) => {
    if (mutating.current) return;
    mutating.current = true;
    mutationRevision.current++;
    setPending(true);
    setError(null);
    try {
      setJob(await action());
    } catch (failure) {
      setError(failure);
    } finally {
      mutating.current = false;
      setPending(false);
      setJobRefresh((value) => value + 1);
    }
  };

  const prepare = (selected: Segment[]) => {
    if (!agent || !ready(agent) || !checked || readFailed || !canGenerate || busy || prepared) return;
    setStartedAt(undefined);
    void run(async () => ({
      ...await cmd.prepareServiceSummary(meetingId, agent, selected.map((segment) => segment.id)),
      agent,
      status: "prepared",
      error: null,
    }));
  };

  return (
    <div className="space-y-3">
      {canGenerate && segments.length > 0 && (
        <div className="space-y-3 rounded-lg border border-line bg-canvas-2 p-3" aria-busy={busy}>
          <p className="text-[13px] text-ink-3">{t("summary.serviceGeneration")}</p>
          <div className="flex flex-wrap items-center gap-2">
            <select aria-label={t("summary.provider")} value={prepared?.agent ?? (isRunning(job) ? job?.agent : agent)}
              disabled={busy || Boolean(prepared)} onChange={(event) => setAgent(event.target.value as SummaryAgent | "")}
              className="focus-ring h-8 rounded-md border border-line-2 bg-canvas px-2 text-[12px]">
              <option value="">{t("subscription.chooseProvider")}</option>
              <SubscriptionOptions providers={providers} />
            </select>
            <Button type="button" variant="outline" size="sm" disabled={busy || providers === "checking"} onClick={refresh}>
              {t("subscription.refresh")}
            </Button>
          </div>
          <TranscriptExcerpt segments={segments} useClean disabled={busy || Boolean(prepared)}
            canPreview={checked && !readFailed && Boolean(agent && ready(agent))}
            hint={t("summary.serviceExcerptHint")} previewLabel={t("summary.servicePreview")} onPreview={prepare} />
          {prepared?.prompt && (
            <div className="space-y-2">
              <p className="text-[12px] text-ink-3">{t("summary.serviceConsent", { provider: t(`summary.${prepared.agent}`) })}</p>
              <label className="block space-y-1 text-[13px]">
                {t("summary.servicePrompt")}
                <textarea className="focus-ring block w-full rounded-md border border-line-2 bg-canvas p-2" rows={8} readOnly value={prepared.prompt} />
              </label>
              <Button type="button" variant="primary" size="sm" disabled={busy || readFailed || !ready(prepared.agent)}
                onClick={() => {
                  setStartedAt(performance.now());
                  void run(() => cmd.generateServiceSummary(prepared.preview_id));
                }}>
                {result && "markdown" in result && result.markdown ? t("summary.regenerate") : t("summary.generate")}
              </Button>
              <Button type="button" variant="outline" size="sm" disabled={pending} className="ml-2"
                onClick={() => void run(() => cmd.cancelServiceSummary(prepared.preview_id))}>
                {t("summary.serviceDiscard")}
              </Button>
            </div>
          )}
        </div>
      )}
      {job?.status === "running" && (
        <Button type="button" variant="outline" size="sm" disabled={pending}
          onClick={() => void run(() => cmd.cancelServiceSummary(job.preview_id))}>
          {t("summary.cancel")}
        </Button>
      )}
      {progressLabel && <OperationProgress label={progressLabel} startedAt={progressStartedAt} announceStage />}
      {completed && <OperationProgress label={t("summary.serviceSaved")} value={1}
        startedAt={progressFinishedAt === undefined ? undefined : progressStartedAt} finishedAt={progressFinishedAt} announceStage />}
      {job?.status === "cancelled" && <p role="status">{t("subscription.cancelled")}</p>}
      {readFailed && <p role="alert" className="text-ember">{t("subscription.jobUnavailable")}</p>}
      {error != null && <p role="alert" className="text-ember">{displayError(error)}</p>}
      {job?.error && <p role="alert" className="text-ember">{displayError(job.error)}</p>}
      {result === null ? <p role="status">{t("summary.loading")}</p>
        : "error" in result ? (
          <div className="space-y-2">
            <p role="alert" className="text-ember">{result.error}</p>
            <Button variant="outline" size="sm" onClick={() => { setResult(null); setRevision((value) => value + 1); }}>
              {t("summary.retry")}
            </Button>
          </div>
        ) : result.markdown ? <Markdown text={result.markdown} onSeek={onSeek} />
          : <p className="text-sm text-ink-3">{t("page.noSummary")}</p>}
    </div>
  );
}

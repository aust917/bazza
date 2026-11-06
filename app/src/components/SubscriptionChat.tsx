import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { transcriptTxt } from "@/lib/export";
import { displayError } from "@/lib/i18n";
import { cmd, type Segment, type SubscriptionJob, type SummaryAgent } from "@/lib/ipc";
import { Markdown } from "./Markdown";
import { SubscriptionOptions, useSubscriptionProviders } from "./SubscriptionProviders";
import { TranscriptExcerpt } from "./TranscriptExcerpt";
import { Button } from "./ui";
import { OperationProgress } from "./ui/OperationProgress";

type TranscriptContext = { segments: Segment[]; useClean: boolean };
type Props = { chatKey: string; transcript?: TranscriptContext };

function isRunning(job: SubscriptionJob | null) {
  return job?.status === "running" || job?.status === "cancelling";
}

export function SubscriptionChat(props: Props) {
  return <SubscriptionChatForm key={props.chatKey} {...props} />;
}

function SubscriptionChatForm({ chatKey, transcript }: Props) {
  const { t } = useTranslation("meeting");
  const { providers, ready, refresh } = useSubscriptionProviders();
  const [agent, setAgent] = useState<SummaryAgent | "">("");
  const [question, setQuestion] = useState("");
  const [context, setContext] = useState("");
  const [questionOnly, setQuestionOnly] = useState(false);
  const [job, setJob] = useState<SubscriptionJob | null>(null);
  const [checked, setChecked] = useState(false);
  const [readFailed, setReadFailed] = useState(false);
  const [refreshJob, setRefreshJob] = useState(0);
  const [mutation, setMutation] = useState<"start" | "cancel" | null>(null);
  const [requestedAt, setRequestedAt] = useState<number>();
  const [error, setError] = useState<string | null>(null);
  const revision = useRef(0);
  const mutating = useRef(false);
  const running = isRunning(job);
  const pending = running || mutation !== null;
  const hasContext = context.trim().length > 0;
  const canSend = agent && ready(agent) && question.trim() && (hasContext || questionOnly) && checked && !readFailed && !pending;

  useEffect(() => {
    let active = true;
    let first = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      // One follow-up read covers a start already in flight when the form reopened.
      let again = first || mutating.current;
      first = false;
      const current = revision.current;
      if (!mutating.current) {
        try {
          const result = await cmd.getSubscriptionJob(chatKey);
          if (active && current === revision.current) {
            setJob(result);
            setChecked(true);
            setReadFailed(false);
            again ||= isRunning(result);
          }
        } catch {
          if (active && current === revision.current) {
            setReadFailed(true);
            again = true;
          }
        }
      }
      if (active && again) timer = setTimeout(() => void poll(), 750);
    };
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [chatKey, refreshJob]);

  const run = async (kind: "start" | "cancel", action: () => Promise<SubscriptionJob | null>) => {
    if (mutating.current) return;
    mutating.current = true;
    revision.current++;
    if (kind === "start") setRequestedAt(performance.now());
    setMutation(kind);
    setError(null);
    try {
      setJob(await action());
    } catch (failure) {
      setError(displayError(failure));
    } finally {
      mutating.current = false;
      setMutation(null);
      setRefreshJob((value) => value + 1);
    }
  };

  const send = () => {
    if (!canSend) return;
    void run("start", () => cmd.startSubscription(chatKey, agent, question.trim(), hasContext ? context : null));
    setQuestionOnly(false);
  };

  const updateContext = (text: string) => {
    setContext(text);
    setQuestionOnly(false);
  };
  const startedAt = mutation === "start" || job?.started_at_ms === undefined
    ? requestedAt
    : job.started_at_ms - performance.timeOrigin;
  const finishedAt = job?.finished_at_ms == null ? undefined : job.finished_at_ms - performance.timeOrigin;
  return (
    <div className="space-y-3 rounded-lg border border-line bg-canvas-2 p-3">
      <p className="text-[13px] text-ink-3">{t("subscription.chatScope")}</p>
      <form className="space-y-3" aria-busy={pending} onSubmit={(event) => { event.preventDefault(); send(); }}>
        <div className="flex flex-wrap items-center gap-2">
          <select
            aria-label={t("subscription.chatProvider")}
            value={agent}
            disabled={pending}
            onChange={(event) => setAgent(event.target.value as SummaryAgent | "")}
            className="focus-ring h-8 rounded-md border border-line-2 bg-canvas px-2 text-[12px]"
          >
            <option value="">{t("subscription.chooseProvider")}</option>
            <SubscriptionOptions providers={providers} />
          </select>
          <Button type="button" variant="outline" size="sm" disabled={pending || providers === "checking"} onClick={refresh}>
            {t("subscription.refresh")}
          </Button>
        </div>
        <label className="block space-y-1 text-[13px]">
          {t("subscription.question")}
          <textarea className="focus-ring block w-full rounded-md border border-line-2 bg-canvas p-2" rows={2} value={question} disabled={pending} onChange={(event) => setQuestion(event.target.value)} />
        </label>
        {transcript && transcript.segments.length > 0 && <TranscriptExcerpt
          segments={transcript.segments} useClean={transcript.useClean} disabled={pending}
          hint={t("subscription.excerptHint")} previewLabel={t("subscription.excerptPreview")}
          onPreview={(selected) => updateContext(transcriptTxt(selected, transcript.useClean))}
        />}
        <label className="block space-y-1 text-[13px]">
          {t("subscription.context")}
          <textarea className="focus-ring block w-full rounded-md border border-line-2 bg-canvas p-2" rows={4} value={context} disabled={pending} onChange={(event) => updateContext(event.target.value)} />
        </label>
        {!hasContext && <label className="flex items-center gap-2 text-[13px]">
          <input type="checkbox" checked={questionOnly} disabled={pending} onChange={(event) => setQuestionOnly(event.target.checked)} />
          {t("subscription.questionOnly")}
        </label>}
        {agent && <p className="text-[12px] text-ink-3">{t("subscription.chatConsent", { provider: t(`summary.${agent}`) })}</p>}
        <Button type="submit" variant="primary" size="sm" disabled={!canSend}>{t("subscription.send")}</Button>
        {job?.status === "running" && (
          <Button type="button" variant="outline" size="sm" className="ml-2" disabled={mutation !== null}
            onClick={() => void run("cancel", () => cmd.cancelSubscription(chatKey, job.request_id))}>
            {t("subscription.cancel")}
          </Button>
        )}
      </form>
      {pending && <OperationProgress
        announceStage
        label={mutation === "cancel" || job?.status === "cancelling" ? t("subscription.cancelling")
          : job && running ? t("subscription.waitingFor", { provider: t(`summary.${job.agent}`) })
            : t("subscription.waiting")}
        startedAt={startedAt}
      />}
      {!pending && job?.status === "cancelled" && <p role="status" className="text-[12px] text-ink-3">{t("subscription.cancelled")}</p>}
      {readFailed && <p role="alert" className="text-[13px] text-ember">{t("subscription.jobUnavailable")}</p>}
      {error && <p role="alert" className="text-[13px] text-ember">{error}</p>}
      {job?.error && <p role="alert" className="text-[13px] text-ember">{displayError(job.error)}</p>}
      {job?.reply && (
        <div aria-live="polite">
          {!pending && job.status === "succeeded" && <OperationProgress label={t("subscription.replyReady")} value={1} startedAt={startedAt} finishedAt={finishedAt} />}
          <p className="text-[12px] text-ink-3">{job.reply.model}</p>
          <Markdown text={job.reply.markdown} />
        </div>
      )}
    </div>
  );
}

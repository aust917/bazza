import { useTranslation } from "react-i18next";
import { useEffect, useRef, useState } from "react";
import { Sparkles } from "lucide-react";
import { cmd, type SummaryAgent, type Template } from "@/lib/ipc";
import { displayError } from "@/lib/i18n";
import { SubscriptionOptions, useSubscriptionProviders } from "./SubscriptionProviders";
import { templateName } from "./templateLabels";
import { Button } from "./ui";

interface Props {
  meetingId: string;
  templates: Template[];
  templateId: string;
  onTemplateChange: (id: string) => void;
  disabled: boolean;
  writing: boolean;
  hasSummary: boolean;
  onStarted: () => void;
  onFinished: () => void;
  onError: (error: string) => void;
}

export function SummaryControls(p: Props) {
  const { t: tr } = useTranslation("meeting");
  const [agent, setAgent] = useState<SummaryAgent | "">("");
  const { providers, ready, refresh } = useSubscriptionProviders();
  const [pending, setPending] = useState(false);
  const [running, setRunning] = useState(false);
  const [runningAgent, setRunningAgent] = useState<SummaryAgent | null>(null);
  const observedJob = useRef(false);
  const callbacks = useRef(p);
  callbacks.current = p;
  useEffect(() => {
    let active = true;
    observedJob.current = false;
    setRunning(false);
    setRunningAgent(null);
    setAgent("");
    const check = async () => {
      try {
        const job = await cmd.summaryJob(p.meetingId);
        if (!active) return;
        setRunning(job.running);
        setRunningAgent(job.running ? job.agent : null);
        if (job.running) {
          if (!observedJob.current) callbacks.current.onStarted();
        } else if (observedJob.current) callbacks.current.onFinished();
        observedJob.current = job.running;
      } catch { /* The existing processing events still report progress. */ }
    };
    void check();
    const timer = setInterval(() => void check(), 1500);
    return () => { active = false; clearInterval(timer); };
  }, [p.meetingId]);
  const writing = p.writing || running;
  const cannotGenerate = p.disabled || writing || pending || Boolean(agent && !ready(agent));
  const generate = async () => {
    if (cannotGenerate) return;
    setPending(true);
    p.onStarted();
    try {
      await cmd.generateSummary(p.meetingId, p.templateId || undefined, agent || undefined);
    } catch (e) { p.onError(displayError(e)); }
    finally { setPending(false); }
  };
  return (
    <>
      <select aria-label={tr("summary.template")} value={p.templateId} disabled={writing || pending} onChange={(e) => p.onTemplateChange(e.target.value)} className="focus-ring h-8 max-w-[220px] rounded-md border border-line-2 bg-canvas px-2 text-[12px]">
        {!p.templateId && <option value="">{tr("summary.default")}</option>}
        {p.templates.map((t) => <option key={t.id} value={t.id}>{templateName(t, tr)}</option>)}
      </select>
      <select aria-label={tr("summary.provider")} value={agent} disabled={writing || pending} onChange={(e) => setAgent(e.target.value as SummaryAgent | "")} className="focus-ring h-8 rounded-md border border-line-2 bg-canvas px-2 text-[12px]">
        <option value="">{tr("summary.configured")}</option>
        <SubscriptionOptions providers={providers} />
      </select>
      <Button variant="outline" size="sm" disabled={writing || pending || providers === "checking"} onClick={refresh}>{tr("subscription.refresh")}</Button>
      <Button variant="outline" size="sm" disabled={cannotGenerate} onClick={() => void generate()}><Sparkles size={14} /> {p.hasSummary ? tr("summary.regenerate") : tr("summary.generate")}</Button>
      {(runningAgent || agent) && writing && <Button variant="outline" size="sm" onClick={() => void cmd.cancelSummary(p.meetingId).catch((e) => p.onError(displayError(e)))}>{tr("summary.cancel")}</Button>}
      {agent && <p className="basis-full text-[12px] text-ink-3">{tr("summary.consent", { provider: agent === "codex" ? tr("summary.codex") : tr("summary.claude") })}</p>}
    </>
  );
}

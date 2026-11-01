import { useEffect, useRef, useState } from "react";
import { Sparkles } from "lucide-react";
import { cmd, type SummaryAgent, type SummaryAgentAvailability, type Template } from "@/lib/ipc";
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
  const [agent, setAgent] = useState<SummaryAgent | "">("");
  const [available, setAvailable] = useState<SummaryAgentAvailability[]>([]);
  const [pending, setPending] = useState(false);
  const [running, setRunning] = useState(false);
  const observedJob = useRef(false);
  const callbacks = useRef(p);
  callbacks.current = p;
  useEffect(() => {
    let active = true;
    observedJob.current = false;
    setRunning(false);
    setAgent("");
    const check = async () => {
      try {
        const job = await cmd.summaryJob(p.meetingId);
        if (!active) return;
        setRunning(job.running);
        if (job.running) {
          setAgent(job.agent ?? "");
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
  useEffect(() => {
    let active = true;
    void cmd.summaryAgents().then((result) => { if (active) setAvailable(result); }).catch(() => {});
    return () => { active = false; };
  }, []);
  const generate = async () => {
    setPending(true);
    p.onStarted();
    try {
      await cmd.generateSummary(p.meetingId, p.templateId || undefined, agent || undefined);
    } catch (e) { p.onError(String(e)); }
    finally { setPending(false); }
  };
  return (
    <>
      <select aria-label="Summary template" value={p.templateId} disabled={writing || pending} onChange={(e) => p.onTemplateChange(e.target.value)} className="focus-ring h-8 max-w-[220px] rounded-md border border-line-2 bg-canvas px-2 text-[12px]">
        {!p.templateId && <option value="">Default template</option>}
        {p.templates.map((t) => <option key={t.id} value={t.id}>{t.name}</option>)}
      </select>
      <select aria-label="Summary provider" value={agent} disabled={writing || pending} onChange={(e) => setAgent(e.target.value as SummaryAgent | "")} className="focus-ring h-8 rounded-md border border-line-2 bg-canvas px-2 text-[12px]">
        <option value="">Configured language model</option>
        {(["codex", "claude"] as const).map((id) => {
          const installed = available.some((a) => a.agent === id && a.installed);
          return <option key={id} value={id} disabled={!installed}>{id === "codex" ? "Codex" : "Claude Code"}{installed ? " · subscription" : " · unavailable"}</option>;
        })}
      </select>
      <Button variant="outline" size="sm" disabled={p.disabled || writing || pending} onClick={() => void generate()}><Sparkles size={14} /> {p.hasSummary ? "Regenerate" : "Generate"}</Button>
      {agent && writing && <Button variant="outline" size="sm" onClick={() => void cmd.cancelSummary(p.meetingId).catch((e) => p.onError(String(e)))}>Cancel summary</Button>}
      {agent && <p className="basis-full text-[12px] text-ink-3">Generate sends this transcript and template to {agent === "codex" ? "OpenAI through Codex" : "Anthropic through Claude Code"} using your existing subscription login. Plan limits apply. Maximum input: 2 MiB; timeout: 10 minutes.</p>}
    </>
  );
}

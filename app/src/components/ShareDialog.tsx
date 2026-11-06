import { useTranslation } from "react-i18next";
import { open, save } from "@tauri-apps/plugin-dialog";
import { Check, Copy, Link2, Scissors, X } from "lucide-react";
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { displayError } from "@/lib/i18n";
import { safeFileName } from "@/lib/export";
import { fmtDuration } from "@/lib/format";
import { cmd, type ShareInfo } from "@/lib/ipc";
import { Button, Input, Toggle } from "./ui";
import { OperationProgress } from "./ui/OperationProgress";

interface ShareDialogProps {
  meetingId: string;
  title: string;
  durationMs: number;
  hasVideo: boolean;
  hasAudio?: boolean;
  /** Current player position, used as the default clip start. */
  currentMs: number;
  onClose: () => void;
}

type ShareFeedback =
  | { kind: "error"; error: unknown }
  | { kind: "invalidRange"; reason: "format" | "order" | "bounds" }
  | { kind: "saved" | "savedBundle"; path: string };

type ShareAction = "link" | "cliplink" | "clipfile" | "bundle" | "html" | "toggle" | "delete" | "copy" | "reload";
type ShareOperation = { action: ShareAction; startedAt: number | null };

const fmtInput = (ms: number) => fmtDuration(ms);
const parseInput = (s: string): number | null => {
  const fields = s.trim().split(":");
  if (fields.length < 2 || fields.length > 3 || fields.some((part) => !/^\d+$/.test(part))) return null;
  const parts = fields.map(Number);
  if (parts.slice(1).some((part) => part >= 60)) return null;
  const ms = parts.reduce((seconds, part) => seconds * 60 + part, 0) * 1000;
  return Number.isSafeInteger(ms) ? ms : null;
};

/** Share links (LAN), clip export, bundle export and standalone HTML. */
export function ShareDialog({ meetingId, title, durationMs, hasVideo, hasAudio = true, currentMs, onClose }: ShareDialogProps) {
  const { t: tr } = useTranslation("meeting");
  const canClip = hasAudio || hasVideo;
  const headingId = useId();
  const panel = useRef<HTMLDivElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const [shares, setShares] = useState<ShareInfo[]>([]);
  const [reloadFailed, setReloadFailed] = useState(false);
  const [operation, setOperation] = useState<ShareOperation | null>(null);
  const busy = operation !== null;
  const [msg, setMsg] = useState<ShareFeedback | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [clipStart, setClipStart] = useState(fmtInput(Math.max(0, currentMs - 15000)));
  const [clipEnd, setClipEnd] = useState(fmtInput(Math.min(durationMs, currentMs + 30000)));
  const [includeVideo, setIncludeVideo] = useState(true);
  const generation = useRef(0);
  const loadRequest = useRef(0);
  const running = useRef<number | null>(null);
  const copyTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const reload = async (request: number) => {
    const load = ++loadRequest.current;
    try {
      const all = await cmd.listShares();
      if (generation.current !== request || loadRequest.current !== load) return;
      setShares(all.filter((s) => s.share.meeting_id === meetingId));
      setReloadFailed(false);
    } catch (error) {
      if (generation.current !== request || loadRequest.current !== load) return;
      setReloadFailed(true);
      throw error;
    }
  };
  useEffect(() => {
    const request = ++generation.current;
    running.current = null;
    setShares([]);
    setOperation(null);
    setMsg(null);
    setCopied(null);
    setReloadFailed(false);
    void reload(request).catch((error) => {
      if (generation.current === request) setMsg({ kind: "error", error });
    });
    return () => { generation.current++; clearTimeout(copyTimer.current); };
  }, [meetingId]);
  useEffect(() => {
    const previous = document.activeElement;
    closeButton.current?.focus();
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, []);

  const onDialogKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose();
    } else if (event.key === "Tab") {
      const controls = Array.from(panel.current?.querySelectorAll<HTMLElement>(
        "button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]",
      ) ?? []).filter((element) => element.tabIndex >= 0 && !element.closest("[hidden]"));
      const target = event.shiftKey ? controls.at(-1) : controls[0];
      if (target && document.activeElement === (event.shiftKey ? controls[0] : controls.at(-1))) {
        event.preventDefault();
        target.focus();
      }
    }
  };

  const run = async (action: ShareAction, fn: (begin: () => boolean) => Promise<ShareFeedback | void>, refresh = true) => {
    if (running.current !== null) return;
    const request = generation.current;
    running.current = request;
    const choosingDestination = action === "clipfile" || action === "bundle" || action === "html";
    setOperation({ action, startedAt: choosingDestination ? null : performance.now() });
    if (!choosingDestination) setMsg(null);
    const begin = () => {
      if (generation.current !== request) return false;
      setOperation({ action, startedAt: performance.now() });
      setMsg(null);
      return true;
    };
    try {
      const r = await fn(begin);
      if (generation.current !== request) return;
      if (refresh) await reload(request);
      if (generation.current !== request) return;
      if (r) setMsg(r);
    } catch (e) {
      if (generation.current === request) setMsg({ kind: "error", error: e });
    } finally {
      if (generation.current === request) {
        running.current = null;
        setOperation(null);
      }
    }
  };

  const copy = (text: string) => run("copy", async () => {
    const request = generation.current;
    setCopied(null);
    await navigator.clipboard.writeText(text);
    if (generation.current !== request) return;
    setCopied(text);
    clearTimeout(copyTimer.current);
    copyTimer.current = setTimeout(() => setCopied(null), 1500);
  }, false);

  const clipRange = (): [number, number] | null => {
    const a = parseInput(clipStart);
    const b = parseInput(clipEnd);
    if (a == null || b == null) {
      setMsg({ kind: "invalidRange", reason: "format" });
      return null;
    }
    if (b <= a) {
      setMsg({ kind: "invalidRange", reason: "order" });
      return null;
    }
    if (durationMs > 0 && a >= durationMs) {
      setMsg({ kind: "invalidRange", reason: "bounds" });
      return null;
    }
    return [a, Math.min(b, durationMs || b)];
  };

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-ink/30 p-6" onClick={onClose}>
      <div ref={panel} role="dialog" aria-modal="true" aria-labelledby={headingId} aria-busy={busy} onKeyDown={onDialogKey} className="rise max-h-[calc(100dvh-3rem)] w-full max-w-lg overflow-y-auto rounded-xl border border-line bg-canvas p-5 shadow-float" onClick={(e) => e.stopPropagation()}>
        <div className="mb-4 flex items-start justify-between">
          <div>
            <h2 id={headingId} className="font-sans font-semibold text-[22px] leading-tight">{tr("share.heading")}</h2>
            <p className="text-[12px] text-ink-3">{tr("share.hint")}</p>
          </div>
          <button ref={closeButton} onClick={onClose} aria-label={tr("actions.close")} className="focus-ring rounded text-ink-3 hover:text-ink"><X size={18} /></button>
        </div>

        <section className="mb-5">
          <h3 className="mb-2 text-[11px] font-semibold tracking-[0.12em] text-ink-3 uppercase">{tr("share.network")}</h3>
          <div className="flex gap-2">
            <Button variant="primary" size="sm" disabled={!canClip || busy} onClick={() => void run("link", async () => { await cmd.createShare(meetingId); })}>
              <Link2 size={14} /> {tr("share.whole")}
            </Button>
          </div>
          {shares.length > 0 && (
            <ul className="mt-3 flex flex-col gap-1.5">
              {shares.map((s) => (
                <li key={s.share.token} className="flex items-center gap-2 rounded-md border border-line px-2 py-1.5 text-[12px]">
                  <span className="shrink-0 rounded bg-canvas-3 px-1.5 py-px text-[10px] font-semibold uppercase">{s.share.kind === "clip" ? tr("share.clip") : tr("share.meeting")}</span>
                  <span className="min-w-0 flex-1 truncate font-mono text-ink-2">{s.url}</span>
                  {s.share.kind === "clip" && <span className="shrink-0 text-ink-3">{fmtDuration(s.share.start_ms ?? 0)}-{fmtDuration(s.share.end_ms ?? 0)}</span>}
                  <button disabled={busy} onClick={() => void copy(s.url)} className="text-ink-3 hover:text-ink" title={tr("share.copyLink")}>{copied === s.url ? <Check size={14} className="text-moss" /> : <Copy size={14} />}</button>
                  <Toggle checked={s.share.enabled} onChange={(v) => void run("toggle", async () => { await cmd.setShareEnabled(s.share.token, v); })} label="" ariaLabel={tr("share.enabled")} />
                  <button disabled={busy} onClick={() => void run("delete", async () => { await cmd.deleteShare(s.share.token); })} className="text-ink-3 hover:text-ember" title={tr("share.deleteLink")}><X size={14} /></button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="mb-5">
          <h3 className="mb-2 text-[11px] font-semibold tracking-[0.12em] text-ink-3 uppercase">{tr("share.clip")}</h3>
          <div className="flex flex-wrap items-center gap-2">
            <Input value={clipStart} onChange={(e) => setClipStart(e.target.value)} disabled={!canClip || busy} className="w-24 font-mono" aria-label={tr("share.clipStart")} />
            <span className="text-ink-3">{tr("share.to")}</span>
            <Input value={clipEnd} onChange={(e) => setClipEnd(e.target.value)} disabled={!canClip || busy} className="w-24 font-mono" aria-label={tr("share.clipEnd")} />
            <Button variant="outline" size="sm" disabled={!canClip || busy} onClick={() => { const r = clipRange(); if (r) void run("cliplink", async () => { await cmd.createShare(meetingId, r[0], r[1]); }); }}>
              <Link2 size={14} /> {tr("share.link")}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={!canClip || busy}
              onClick={() => {
                const r = clipRange();
                if (!r) return;
                void run("clipfile", async (begin) => {
                  const ext = hasVideo ? "mp4" : "mp3";
                  const out = await save({ defaultPath: `${safeFileName(title)}-clip.${ext}`, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
                  if (!out || !begin()) return;
                  await cmd.exportClip(meetingId, r[0], r[1], out);
                  return { kind: "saved", path: out };
                }, false);
              }}
            >
              <Scissors size={14} /> {tr("share.saveFile")}
            </Button>
          </div>
        </section>

        <section>
          <h3 className="mb-2 text-[11px] font-semibold tracking-[0.12em] text-ink-3 uppercase">{tr("share.export")}</h3>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" disabled={!hasAudio || busy} onClick={() => void run("bundle", async (begin) => {
              const out = await save({ defaultPath: `${safeFileName(title)}.bazza`, filters: [{ name: tr("share.bundleFilter"), extensions: ["bazza"] }] });
              if (!out || !begin()) return;
              await cmd.exportMeetings([meetingId], out, includeVideo);
              return { kind: "savedBundle", path: out };
            }, false)}>
              {tr("share.bundle")}
            </Button>
            <Button variant="outline" size="sm" disabled={!canClip || busy} onClick={() => void run("html", async (begin) => {
              const dir = await open({ directory: true, title: tr("share.htmlFolder") });
              if (!dir || !begin()) return;
              const page = await cmd.exportHtml(meetingId, dir as string);
              return { kind: "saved", path: page };
            }, false)}>
              {tr("share.html")}
            </Button>
            {hasVideo && (
              <label className="ml-auto flex items-center gap-2 text-[12px] text-ink-2">
                <input type="checkbox" checked={includeVideo} disabled={busy} onChange={(e) => setIncludeVideo(e.target.checked)} /> {tr("share.includeVideo")}
              </label>
            )}
          </div>
        </section>

        {operation && operation.startedAt !== null && (
          <OperationProgress className="mt-4" label={tr(`share.progress.${operation.action}`)} startedAt={operation.startedAt} announceStage />
        )}
        {!busy && msg && <div role={msg.kind === "error" || msg.kind === "invalidRange" ? "alert" : "status"} className="mt-4 break-words text-[12px] text-ink-2">
          {msg.kind === "error" ? displayError(msg.error) : msg.kind === "invalidRange" ? tr(msg.reason === "order" ? "share.rangeOrder" : msg.reason === "bounds" ? "share.rangeBounds" : "share.invalidRange", { duration: fmtDuration(durationMs) }) : tr(msg.kind === "savedBundle" ? "share.savedBundle" : "share.saved", { path: msg.path })}
          {reloadFailed && <Button variant="ghost" size="sm" onClick={() => void run("reload", async () => {})}>{tr("actions.retry")}</Button>}
        </div>}
      </div>
    </div>
  );
}

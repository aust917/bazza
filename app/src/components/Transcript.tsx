import { useTranslation } from "react-i18next";
import { Pencil } from "lucide-react";
import { memo, useCallback, useEffect, useId, useMemo, useRef, useState, type RefObject } from "react";
import { cn } from "@/lib/cn";
import { fmtStamp, speakerLabel } from "@/lib/format";
import { displayError } from "@/lib/i18n";
import type { MeetingSpeaker, Segment, TranscriptPlaybackTiming } from "@/lib/ipc";
import { SpeakerChip } from "./SpeakerChip";

export type TranscriptLine = Pick<Segment, "id" | "start_ms" | "end_ms" | "speaker" | "text" | "clean_text" | "clean_text_manual" | "provenance"> & {
  /** Keep a live speaker's provisional line mounted while the words change. */
  provisionalKey?: string;
};

export interface TranscriptProps {
  segments: TranscriptLine[];
  currentMs: number;
  onSeek?: (ms: number) => void;
  /** When present, only rows mapped to the recording can seek or follow playback. */
  playbackTimings?: ReadonlyMap<number, TranscriptPlaybackTiming>;
  useClean?: boolean;
  autoScroll?: boolean;
  className?: string;
  speakers?: MeetingSpeaker[];
  /** Names offered in the rename box (attendees + known speakers). */
  nameOptions?: string[];
  onRenameSpeaker?: (label: string, name: string) => void | Promise<void>;
  onAcceptSuggestion?: (label: string) => void | Promise<void>;
  onDismissSuggestion?: (label: string) => void;
  onSetCorrection?: (segmentId: number, text: string | null) => void | Promise<void>;
}

export const SPEAKER_COLORS = ["text-ember", "text-moss", "text-amber", "text-ink-2"];
const NO_SPEAKERS: MeetingSpeaker[] = [];
const NO_NAMES: string[] = [];
type TranscriptSegment = TranscriptProps["segments"][number];

const TranscriptRow = memo(function TranscriptRow({ segment: s, active, activeRef, showSpeaker, color, suggestion, useClean, nameOptions, playbackTiming, requiresPlaybackTiming, onSeek, onRenameSpeaker, onAcceptSuggestion, onDismissSuggestion, editable, saving, onEdit }: {
  segment: TranscriptSegment;
  active: boolean;
  activeRef: RefObject<HTMLButtonElement | null>;
  showSpeaker: boolean;
  color: string;
  suggestion: { name: string; score: number } | null;
  useClean: boolean;
  nameOptions: string[];
  playbackTiming?: TranscriptPlaybackTiming;
  requiresPlaybackTiming: boolean;
  onSeek: TranscriptProps["onSeek"];
  onRenameSpeaker: TranscriptProps["onRenameSpeaker"];
  onAcceptSuggestion: TranscriptProps["onAcceptSuggestion"];
  onDismissSuggestion: TranscriptProps["onDismissSuggestion"];
  editable: boolean;
  saving: boolean;
  onEdit: (segment: TranscriptSegment, trigger: HTMLButtonElement) => void;
}) {
  const { t: tr } = useTranslation("meeting");
  const cleaned = useClean && s.clean_text !== null && s.clean_text !== s.text;
  const text = useClean ? (s.clean_text ?? s.text) : s.text;
  const seekMs = requiresPlaybackTiming || s.provenance?.source === "bot" ? playbackTiming?.start_ms : s.start_ms;
  const canSeek = onSeek !== undefined && seekMs !== undefined;
  const editLabel = editable ? tr("transcript.editAt", { time: fmtStamp(s.start_ms) }) : undefined;
  return <>
    {s.provenance && <div className="mb-0.5 flex flex-wrap gap-1 text-[11px] text-ink-3">
      <span className="rounded border border-line px-1.5">{tr(`versions.provenance.${s.provenance.source}`)}{s.provenance.provider ? ` · ${s.provenance.provider === "meet" ? "Google Meet" : "Discord"}` : ""}</span>
      {s.provenance.uncertain && <span className="rounded border border-amber px-1.5 text-amber">{tr("versions.provenance.uncertain")}</span>}
    </div>}
    {showSpeaker && s.speaker && (
      onRenameSpeaker ? (
        <SpeakerChip
          label={s.speaker}
          colorClass={color}
          suggestion={suggestion}
          onRename={(name) => onRenameSpeaker(s.speaker!, name)}
          onAcceptSuggestion={() => onAcceptSuggestion?.(s.speaker!)}
          onDismissSuggestion={() => onDismissSuggestion?.(s.speaker!)}
          nameOptions={nameOptions}
        />
      ) : <div className={cn("mb-0.5 text-[12px] font-semibold", color)}>{speakerLabel(s.speaker)}</div>
    )}
    <div className="flex items-start gap-1">
      <button
        type="button"
        ref={active ? activeRef : null}
        onClick={() => { if (canSeek) onSeek(seekMs); }}
        disabled={!canSeek}
        data-active={active || undefined}
        className={cn("focus-ring group flex min-w-0 flex-1 gap-3 rounded-md px-2 py-1 text-left transition-colors", canSeek ? "cursor-pointer" : "cursor-default", active ? "bg-ember-soft" : canSeek && "hover:bg-canvas-2")}
      >
        <span className={cn("mt-0.5 w-12 shrink-0 font-mono text-[11px] tabular-nums", active ? "text-ember" : "text-ink-3",
          playbackTiming && canSeek && "underline underline-offset-2")}>{fmtStamp(seekMs ?? s.start_ms)}</span>
        <span
          className={cn("min-w-0 whitespace-pre-wrap break-words text-[14.5px] leading-6", active ? "text-ink" : s.provisionalKey ? "text-ink-2" : "text-ink/90", cleaned && "decoration-line-2 decoration-dotted underline-offset-4 hover:underline")}
          title={cleaned ? tr("transcript.original", { text: s.text }) : undefined}
        >
          {text === "" ? <span className="text-ink-3">{tr("transcript.blankCorrection")}</span> : text}
        </span>
      </button>
      {editable && <button
        type="button"
        className="focus-ring mt-1 shrink-0 rounded p-1 text-ink-3 hover:text-ink disabled:opacity-50"
        disabled={saving}
        aria-label={editLabel}
        title={editLabel}
        onClick={(event) => onEdit(s, event.currentTarget)}
      ><Pencil size={13} /></button>}
    </div>
    {s.clean_text_manual === true && <p className="ml-[4.5rem] text-[11px] text-ink-3">{tr("transcript.manualCorrection")}</p>}
  </>;
});

export function Transcript({
  segments,
  currentMs,
  onSeek,
  playbackTimings,
  useClean = false,
  autoScroll = true,
  className,
  speakers = NO_SPEAKERS,
  nameOptions = NO_NAMES,
  onRenameSpeaker,
  onAcceptSuggestion,
  onDismissSuggestion,
  onSetCorrection,
}: TranscriptProps) {
  const { t: tr } = useTranslation("meeting");
  const editorId = useId();
  const [editor, setEditor] = useState<{ segmentId: number; draft: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<unknown>(null);
  const mounted = useRef(false);
  const operation = useRef(0);
  const inFlight = useRef(false);
  const correctionTrigger = useRef<HTMLButtonElement | null>(null);
  const correctionEditor = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; operation.current++; };
  }, []);
  useEffect(() => {
    if (editor && !segments.some((segment) => segment.id === editor.segmentId)) {
      operation.current++;
      inFlight.current = false;
      setEditor(null);
      setSaving(false);
      setSaveError(null);
    }
  }, [segments, editor]);
  useEffect(() => {
    if (editor || !correctionTrigger.current) return;
    if (correctionTrigger.current.isConnected) correctionTrigger.current.focus();
    correctionTrigger.current = null;
  }, [editor]);
  useEffect(() => {
    if (mounted.current && editor && !saving && saveError !== null) correctionEditor.current?.focus();
  }, [editor, saving, saveError]);
  const commitCorrection = useCallback(async (text: string | null) => {
    if (!editor || !onSetCorrection || inFlight.current) return;
    const request = ++operation.current;
    inFlight.current = true;
    setSaving(true);
    setSaveError(null);
    try {
      await onSetCorrection(editor.segmentId, text);
      if (mounted.current && operation.current === request) setEditor(null);
    } catch (error) {
      if (mounted.current && operation.current === request) setSaveError(error);
    } finally {
      if (mounted.current && operation.current === request) {
        inFlight.current = false;
        setSaving(false);
      }
    }
  }, [editor, onSetCorrection]);
  const cancelCorrection = useCallback(() => {
    if (inFlight.current) return;
    operation.current++;
    setEditor(null);
    setSaveError(null);
  }, []);
  const startCorrection = useCallback((segment: TranscriptSegment, trigger: HTMLButtonElement) => {
    operation.current++;
    correctionTrigger.current = trigger;
    setEditor({ segmentId: segment.id, draft: segment.clean_text ?? segment.text });
    setSaveError(null);
  }, []);
  const playbackOrder = useMemo(() => playbackTimings && segments.flatMap((segment, index) => {
    const timing = playbackTimings.get(segment.id);
    return timing ? [{ ...timing, index }] : [];
  }).sort((a, b) => a.start_ms - b.start_ms), [segments, playbackTimings]);
  const activeIdx = useMemo(() => {
    if (!onSeek) return -1;
    const timeline = playbackOrder ?? segments;
    let low = 0, high = timeline.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (timeline[mid].start_ms <= currentMs) low = mid + 1;
      else high = mid;
    }
    const idx = low - 1;
    if (idx < 0 || currentMs > timeline[idx].end_ms + 1500) return -1;
    if (!playbackOrder && segments[idx].provenance?.source === "bot") return -1;
    return playbackOrder ? playbackOrder[idx].index : idx;
  }, [segments, currentMs, onSeek, playbackOrder]);

  const speakerIndex = useMemo(() => {
    const map = new Map<string, number>();
    for (const s of segments) {
      if (s.speaker && !map.has(s.speaker)) map.set(s.speaker, map.size);
    }
    return map;
  }, [segments]);

  const suggestions = useMemo(() => {
    const m = new Map<string, { name: string; score: number }>();
    for (const sp of speakers) {
      if (sp.suggested_name && sp.suggested_score != null && !sp.speaker_id) m.set(sp.label, { name: sp.suggested_name, score: sp.suggested_score });
    }
    return m;
  }, [speakers]);

  const activeRef = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    if (autoScroll && activeRef.current) {
      activeRef.current.scrollIntoView({ block: "center", behavior: "smooth" });
    }
  }, [activeIdx, autoScroll]);

  const rows = useMemo(() => {
    let lastSpeaker: string | null | undefined;
    return segments.map((s, i) => {
        const showSpeaker = s.provisionalKey !== undefined || s.speaker !== lastSpeaker;
        lastSpeaker = s.speaker;
        const active = i === activeIdx;
        const color = s.speaker ? SPEAKER_COLORS[(speakerIndex.get(s.speaker) ?? 0) % SPEAKER_COLORS.length] : "text-ink-3";
        const editingCorrection = editor?.segmentId === s.id ? editor : null;
        return (
          <li key={s.provisionalKey ?? s.id} aria-live={s.provisionalKey ? "polite" : undefined} className={cn(showSpeaker && i > 0 && "mt-3")}>
            <TranscriptRow segment={s} active={active} activeRef={activeRef} showSpeaker={showSpeaker} color={color}
              suggestion={s.speaker ? suggestions.get(s.speaker) ?? null : null} useClean={useClean} nameOptions={nameOptions}
              playbackTiming={playbackTimings?.get(s.id)} requiresPlaybackTiming={playbackTimings !== undefined}
              onSeek={onSeek} onRenameSpeaker={onRenameSpeaker} onAcceptSuggestion={onAcceptSuggestion} onDismissSuggestion={onDismissSuggestion}
              editable={!!onSetCorrection} saving={saving} onEdit={startCorrection} />
            {editingCorrection && (
              <form
                className="my-2 ml-2 rounded-md border border-line-2 bg-canvas-2 p-3 font-sans"
                onSubmit={(event) => { event.preventDefault(); void commitCorrection(editingCorrection.draft); }}
              >
                <label htmlFor={editorId} className="mb-1 block text-[12px] font-medium text-ink">{tr("transcript.correctedText")}</label>
                <textarea
                  ref={correctionEditor}
                  id={editorId}
                  autoFocus
                  rows={3}
                  value={editingCorrection.draft}
                  disabled={saving}
                  aria-invalid={saveError !== null || undefined}
                  aria-describedby={`${editorId}-hint${saveError !== null ? ` ${editorId}-error` : ""}`}
                  className="focus-ring w-full resize-y rounded border border-line-2 bg-canvas p-2 font-sans text-[14px] leading-6 text-ink"
                  onChange={(event) => { setEditor({ segmentId: s.id, draft: event.target.value }); setSaveError(null); }}
                  onKeyDown={(event) => {
                    if (event.nativeEvent.isComposing) return;
                    if (event.key === "Escape") { event.preventDefault(); cancelCorrection(); }
                    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) { event.preventDefault(); void commitCorrection(editingCorrection.draft); }
                  }}
                />
                <p id={`${editorId}-hint`} className="mt-1 text-[11px] leading-5 text-ink-3">{tr("transcript.correctionHint")}</p>
                <div className="mt-2 text-[12px] leading-5 text-ink-2">
                  <span className="font-medium">{tr("transcript.originalLabel")}</span>
                  <p className="whitespace-pre-wrap break-words">{s.text}</p>
                </div>
                {saveError !== null && <p id={`${editorId}-error`} role="alert" className="mt-2 text-[12px] text-ember">{displayError(saveError)}</p>}
                <div className="mt-3 flex flex-wrap gap-2 text-[12px]">
                  <button type="submit" disabled={saving} aria-label={tr("transcript.saveCorrection")} className="focus-ring rounded bg-ink px-2.5 py-1.5 text-canvas disabled:opacity-50">
                    {saving ? tr("transcript.savingCorrection") : tr("actions.save")}
                  </button>
                  <button type="button" disabled={saving} onClick={cancelCorrection} aria-label={tr("transcript.cancelCorrection")} className="focus-ring rounded border border-line-2 px-2.5 py-1.5 text-ink-2 disabled:opacity-50">{tr("actions.cancel")}</button>
                  {s.clean_text_manual === true && <button type="button" disabled={saving} onClick={() => void commitCorrection(null)} className="focus-ring rounded px-2.5 py-1.5 text-ink-2 underline disabled:opacity-50">{tr("transcript.resetCorrection")}</button>}
                </div>
              </form>
            )}
          </li>
        );
      });
  }, [segments, activeIdx, speakerIndex, suggestions, useClean, nameOptions, onSeek, playbackTimings, onRenameSpeaker, onAcceptSuggestion, onDismissSuggestion,
    onSetCorrection, saving, startCorrection, editor, editorId, saveError, commitCorrection, cancelCorrection, tr]);

  if (segments.length === 0) return <p className={cn("text-sm text-ink-3", className)}>{tr("transcript.empty")}</p>;
  return <ol className={cn("flex flex-col gap-1", className)} data-testid="transcript">{rows}</ol>;
}

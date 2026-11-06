import { Trans, useTranslation } from "react-i18next";
import { Check, Pencil, X } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { speakerLabel } from "@/lib/format";
import { cn } from "@/lib/cn";
import { displayError } from "@/lib/i18n";

export interface SpeakerChipProps {
  label: string;
  colorClass: string;
  suggestion?: { name: string; score: number } | null;
  onRename: (name: string) => void | Promise<void>;
  onAcceptSuggestion?: () => void | Promise<void>;
  onDismissSuggestion?: () => void;
  /** Names to offer while typing: attendees seen in the meeting and known speakers. */
  nameOptions?: string[];
}

/** Speaker label in the transcript: click to rename; shows a confirmable "Is this X?" chip. */
export function SpeakerChip({ label, colorClass, suggestion, onRename, onAcceptSuggestion, onDismissSuggestion, nameOptions = [] }: SpeakerChipProps) {
  const { t: tr } = useTranslation("meeting");
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(label);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const listId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const editor = useRef<HTMLInputElement>(null);
  const restoreFocus = useRef(false);
  const inFlight = useRef(false);
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    if (!editing && restoreFocus.current) {
      trigger.current?.focus();
      restoreFocus.current = false;
    }
  }, [editing]);
  useEffect(() => {
    if (editing && !saving && error !== null) editor.current?.focus();
  }, [editing, saving, error]);

  const cancel = () => {
    if (inFlight.current) return;
    setError(null);
    setEditing(false);
  };

  const commit = async () => {
    if (inFlight.current) return;
    const name = draft.trim();
    if (!name || name === label) { setEditing(false); return; }
    inFlight.current = true;
    setSaving(true);
    setError(null);
    try {
      await onRename(name);
      if (mounted.current) setEditing(false);
    } catch (failure) {
      if (mounted.current) setError(failure);
    } finally {
      inFlight.current = false;
      if (mounted.current) setSaving(false);
    }
  };

  const acceptSuggestion = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setSaving(true);
    setError(null);
    try { await onAcceptSuggestion?.(); }
    catch (failure) { if (mounted.current) setError(failure); }
    finally {
      inFlight.current = false;
      if (mounted.current) setSaving(false);
    }
  };

  if (editing) {
    return (
      <form
        className="mb-0.5 inline-flex items-center gap-1"
        aria-busy={saving || undefined}
        onSubmit={(e) => {
          e.preventDefault();
          void commit();
        }}
      >
        <input
          ref={editor}
          autoFocus
          aria-label={tr("speakers.name")}
          value={draft}
          disabled={saving}
          aria-invalid={error !== null || undefined}
          aria-describedby={error !== null ? `${listId}-error` : undefined}
          onChange={(e) => { setDraft(e.target.value); setError(null); }}
          onKeyDown={(e) => { if (!e.nativeEvent.isComposing && e.key === "Escape") { e.preventDefault(); cancel(); } }}
          list={nameOptions.length ? listId : undefined}
          className="focus-ring h-6 w-40 rounded border border-line-2 bg-canvas px-2 text-[12px] font-semibold"
        />
        <button type="submit" disabled={saving} className="focus-ring rounded text-moss" title={tr("actions.save")} aria-label={tr("speakers.saveName")}>
          <Check size={14} />
        </button>
        <button type="button" disabled={saving} onClick={cancel} className="focus-ring rounded text-ink-3" title={tr("actions.cancel")} aria-label={tr("actions.cancel")}>
          <X size={14} />
        </button>
        {nameOptions.length > 0 && (
          <datalist id={listId}>
            {nameOptions.map((n) => <option key={n} value={n} />)}
          </datalist>
        )}
        {error !== null && <span id={`${listId}-error`} role="alert" className="text-[12px] font-normal text-ember">{displayError(error)}</span>}
      </form>
    );
  }

  return (
    <div className="mb-0.5 flex flex-wrap items-center gap-2">
      <button
        type="button"
        ref={trigger}
        disabled={saving}
        onClick={() => {
          setDraft(label);
          setError(null);
          restoreFocus.current = true;
          setEditing(true);
        }}
        className={cn("group/chip inline-flex items-center gap-1 rounded text-[12px] font-semibold hover:underline", colorClass)}
        title={tr("speakers.rename")}
      >
        {speakerLabel(label)}
        <Pencil size={11} className="opacity-0 transition-opacity group-hover/chip:opacity-60" />
      </button>
      {suggestion && (
        <span className="inline-flex items-center gap-1 rounded-full bg-amber-soft px-2 py-0.5 text-[11px] text-ink">
          <Trans t={tr} i18nKey="speakers.suggestion" values={{ name: suggestion.name }} components={{ name: <b /> }} />
          <button type="button" disabled={saving} onClick={() => void acceptSuggestion()} className="ml-1 font-semibold text-moss hover:underline">
            {tr("actions.yes")}
          </button>
          <button type="button" disabled={saving} onClick={onDismissSuggestion} className="font-semibold text-ink-3 hover:underline">
            {tr("actions.no")}
          </button>
        </span>
      )}
      {error !== null && <span role="alert" className="text-[12px] font-normal text-ember">{displayError(error)}</span>}
    </div>
  );
}

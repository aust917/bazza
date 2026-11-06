import { Trans, useTranslation } from "react-i18next";
import { Check, Merge, Pencil, X } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { cn } from "@/lib/cn";
import { fmtDuration, speakerLabel } from "@/lib/format";
import type { MeetingSpeaker, SpeakerStat } from "@/lib/ipc";
import { SPEAKER_COLORS } from "./Transcript";
import { SectionTitle } from "./ui";

interface SpeakersPanelProps {
  stats: SpeakerStat[];
  speakers: MeetingSpeaker[];
  /** Attendees and known speakers, offered while typing. */
  nameOptions?: string[];
  onRename?: (label: string, name: string) => void | Promise<void>;
  onAcceptSuggestion?: (label: string) => void | Promise<void>;
  onDismissSuggestion?: (label: string) => void;
  className?: string;
}

/**
 * Everyone Bazza heard in this meeting. Naming two rows the same person merges
 * them - which is how you fix one voice that was split into two speakers.
 */
export function SpeakersPanel({ stats, speakers, nameOptions = [], onRename, onAcceptSuggestion, onDismissSuggestion, className }: SpeakersPanelProps) {
  const { t: tr } = useTranslation("meeting");
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const listId = useId();

  // A row that disappears (merged away) must not keep the editor open.
  useEffect(() => {
    if (editing && !stats.some((s) => s.label === editing)) setEditing(null);
  }, [stats, editing]);

  if (stats.length === 0) return null;

  const suggestionFor = (label: string) => {
    const row = speakers.find((s) => s.label === label);
    return row && row.suggested_name && row.suggested_score != null && !row.speaker_id
      ? { name: row.suggested_name, score: row.suggested_score }
      : null;
  };

  const commit = async (label: string) => {
    const name = draft.trim();
    setEditing(null);
    if (name && name !== label) await onRename?.(label, name);
  };

  const options = Array.from(new Set([...stats.map((s) => s.label), ...nameOptions])).filter((n) => !/^Speaker \d+$/.test(n));

  return (
    <div className={className}>
      <SectionTitle hint={onRename && stats.length > 1 ? tr("speakers.sameName") : undefined}>{tr("speakers.heading")}</SectionTitle>
      <ul className="flex flex-col divide-y divide-line rounded-lg border border-line">
        {stats.map((s, i) => {
          const color = SPEAKER_COLORS[i % SPEAKER_COLORS.length];
          const suggestion = suggestionFor(s.label);
          const others = stats.filter((o) => o.label !== s.label);
          return (
            <li key={s.label} className="px-3 py-2.5">
              <div className="flex items-center gap-2">
                <span className={cn("h-2 w-2 shrink-0 rounded-full bg-current", color)} aria-hidden />
                {editing === s.label ? (
                  <form
                    className="flex min-w-0 flex-1 items-center gap-1"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void commit(s.label);
                    }}
                  >
                    <input
                      autoFocus
                      aria-label={tr("speakers.nameFor", { name: speakerLabel(s.label) })}
                      value={draft}
                      list={options.length ? listId : undefined}
                      onChange={(e) => setDraft(e.target.value)}
                      onKeyDown={(e) => e.key === "Escape" && setEditing(null)}
                      className="focus-ring h-7 min-w-0 flex-1 rounded border border-line-2 bg-canvas px-2 text-[13px] font-medium"
                    />
                    <button type="submit" aria-label={tr("speakers.saveName")} title={tr("actions.save")} className="focus-ring rounded text-moss">
                      <Check size={14} />
                    </button>
                    <button type="button" aria-label={tr("actions.cancel")} title={tr("actions.cancel")} onClick={() => setEditing(null)} className="focus-ring rounded text-ink-3">
                      <X size={14} />
                    </button>
                  </form>
                ) : onRename ? (
                  <button
                    type="button"
                    onClick={() => {
                      setDraft(s.label);
                      setEditing(s.label);
                    }}
                    title={tr("speakers.renameThis")}
                    className="focus-ring group/name inline-flex min-w-0 items-center gap-1.5 rounded text-[13px] font-medium hover:underline"
                  >
                    <span className="truncate">{speakerLabel(s.label)}</span>
                    <Pencil size={11} className="shrink-0 text-ink-3 opacity-0 transition-opacity group-hover/name:opacity-100" />
                  </button>
                ) : <span className="truncate text-[13px] font-medium">{speakerLabel(s.label)}</span>}
                <span className="ml-auto shrink-0 text-[11px] text-ink-3" title={tr("speakers.speechDuration", { duration: fmtDuration(s.ms) })}>
                  {tr("speakers.lines", { count: s.segments })}
                </span>
              </div>

              {suggestion && (
                <div className="mt-1.5 flex items-center gap-1.5 pl-4 text-[12px]">
                  <span className="text-ink-3">
                    <Trans t={tr} i18nKey="speakers.suggestionScore" values={{ name: suggestion.name, score: Math.round(suggestion.score * 100) }} components={{ name: <span className="font-medium text-ink-2" /> }} />
                  </span>
                  {onAcceptSuggestion && <button
                    onClick={() => void onAcceptSuggestion(s.label)}
                    className="focus-ring rounded px-1 text-moss hover:underline"
                    aria-label={tr("speakers.confirm", { name: suggestion.name })}
                  >
                    {tr("actions.yes")}
                  </button>}
                  {onDismissSuggestion && <button onClick={() => onDismissSuggestion(s.label)} className="focus-ring rounded px-1 text-ink-3 hover:underline" aria-label={tr("speakers.dismissSuggestion")}>
                    {tr("actions.no")}
                  </button>}
                </div>
              )}

              {onRename && others.length > 0 && (
                <label className="mt-1.5 flex items-center gap-1.5 pl-4 text-[11px] text-ink-3">
                  <Merge size={11} className="shrink-0" />
                  <span className="sr-only sm:not-sr-only">{tr("speakers.sameAs")}</span>
                  <select
                    aria-label={tr("speakers.merge", { name: speakerLabel(s.label) })}
                    value=""
                    onChange={(e) => e.target.value && void onRename(s.label, e.target.value)}
                    className="focus-ring h-6 min-w-0 max-w-[60%] truncate rounded border border-line-2 bg-canvas px-1 text-[11px] text-ink-2"
                  >
                    <option value="">{tr("speakers.nobody")}</option>
                    {others.map((o) => (
                      <option key={o.label} value={o.label}>
                        {speakerLabel(o.label)}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </li>
          );
        })}
      </ul>
      {onRename && options.length > 0 && (
        <datalist id={listId}>
          {options.map((n) => (
            <option key={n} value={n} />
          ))}
        </datalist>
      )}
      {onRename && stats.length > 1 && (
        <p className="mt-2 text-[12px] text-ink-3">
          {tr("speakers.mergeHint")}
        </p>
      )}
    </div>
  );
}

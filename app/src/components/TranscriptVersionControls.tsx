import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Select } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { fmtDate, fmtTime } from "@/lib/format";
import { displayError } from "@/lib/i18n";
import type { BotTranscriptProvider, ModelRow, TranscriptVersion, TranscriptVersionOptions } from "@/lib/ipc";
import { LANGUAGES } from "@/lib/languages";
import { ORIGINAL_TRANSCRIPT, type useTranscriptVersions } from "@/lib/useTranscriptVersions";

export type TranscriptChoice = { kind: "version"; id: string } | { kind: "bot"; id: BotTranscriptProvider };

// The classic select truncates the selected text within its padded field.
const singleLineSelect = "native-picker min-w-0 truncate";

export function TranscriptVersionControls({ state, botSources, selectedBot, onSelect, models, hasAudio, hasOriginalText, disabled, open, onOpen }: {
  state: ReturnType<typeof useTranscriptVersions>;
  botSources: BotTranscriptProvider[];
  selectedBot: BotTranscriptProvider | null;
  onSelect: (choice: TranscriptChoice) => void;
  models: ModelRow[];
  hasAudio: boolean;
  hasOriginalText: boolean;
  disabled: boolean;
  open: boolean;
  onOpen: (open: boolean) => void;
}) {
  const { t: tr, i18n } = useTranslation("meeting");
  const [source, setSource] = useState<TranscriptVersionOptions["source"]>();
  const [model, setModel] = useState("");
  const [language, setLanguage] = useState("");
  const { list, selected, loading, loadError, selectionError, generationError, startedAt, phase, progress,
    terminal, completion, canCancel, generate, refresh, cancel } = state;
  const generating = startedAt !== undefined;
  const originalListed = list.versions.some((version) => version.id === ORIGINAL_TRANSCRIPT);
  const canUseParticipants = list.participant_source !== null;
  const canGenerate = hasAudio || canUseParticipants;
  const actualSource = source === "recording" && hasAudio ? source
    : source === "combined" && canUseParticipants && hasAudio ? source : canUseParticipants ? "participants" : "recording";
  const languageNames = new Intl.DisplayNames([i18n.resolvedLanguage ?? "en"], { type: "language" });
  const providerName = (provider: BotTranscriptProvider) => provider === "meet" ? "Google Meet" : "Discord";
  const languageLabel = (code: string) => code === "auto" ? tr("versions.autoLanguage") : languageNames.of(code) ?? code;

  function versionLabel(version: TranscriptVersion, index: number) {
    const modelName = models.find((entry) => entry.spec.id === version.model)?.spec.name ?? version.model;
    const sourceName = tr(`versions.sources.${version.source}`);
    const initial = version.id === ORIGINAL_TRANSCRIPT;
    const parts: string[] = initial
      ? [tr("versions.initial"), sourceName]
      : [tr("versions.numbered", { number: index + (originalListed ? 0 : 1) }), sourceName];
    if (modelName) parts.push(modelName);
    if (version.language) parts.push(languageLabel(version.language));
    if (!initial) parts.push(`${fmtDate(version.created_at)} ${fmtTime(version.created_at)}`);
    if (version.partial) parts.push(tr("versions.incomplete"));
    return parts.join(" · ");
  }

  const selectedKey = selectedBot ? `bot:${selectedBot}` : `version:${selected?.id ?? list.effective_id ?? ORIGINAL_TRANSCRIPT}`;
  const choices: { choice: TranscriptChoice; label: string }[] = list.versions.map((version, index) => ({
    choice: { kind: "version", id: version.id }, label: versionLabel(version, index),
  }));
  if (hasOriginalText && !choices.some(({ choice }) => choice.id === ORIGINAL_TRANSCRIPT)) {
    choices.unshift({ choice: { kind: "version", id: ORIGINAL_TRANSCRIPT }, label: tr("botTranscript.main") });
  }
  for (const provider of botSources) choices.push({
    choice: { kind: "bot", id: provider }, label: tr("botTranscript.providerParticipants", { provider: providerName(provider) }),
  });
  const hasOriginal = choices.some(({ choice }) => choice.kind === "version" && choice.id === ORIGINAL_TRANSCRIPT);
  const selectedLabel = choices.find(({ choice }) => `${choice.kind}:${choice.id}` === selectedKey)?.label;
  const sourceLabel = actualSource === "participants" && list.participant_source
    ? tr("versions.participantSource", { provider: providerName(list.participant_source) })
    : tr(`versions.sources.${actualSource === "combined" ? "combined" : "recording"}`);
  const modelLabel = models.find(({ spec }) => spec.id === model)?.spec.name ?? tr("versions.currentModel");

  return (
    <div className="mb-3 space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        {(list.versions.length > 0 || botSources.length > 0) && (
          <Select className={`${singleLineSelect} max-w-full flex-[1_1_15rem]`} aria-label={tr("versions.picker")} aria-busy={loading || undefined}
            title={selectedLabel}
            value={!hasOriginal && selectedKey === `version:${ORIGINAL_TRANSCRIPT}` ? "" : selectedKey}
            onChange={(event) => {
              if (event.target.value === "") return onSelect({ kind: "version", id: ORIGINAL_TRANSCRIPT });
              const next = choices.find(({ choice }) => `${choice.kind}:${choice.id}` === event.target.value);
              if (next) onSelect(next.choice);
            }}>
            {!hasOriginal && <option value="">{tr("versions.choose")}</option>}
            {choices.map(({ choice, label }) => <option key={`${choice.kind}:${choice.id}`} value={`${choice.kind}:${choice.id}`}>{label}</option>)}
          </Select>
        )}
        {(canGenerate || open) && <Button type="button" variant="outline" size="sm" className="h-auto min-h-8 max-w-full whitespace-normal" aria-expanded={open} disabled={disabled || generating}
          onClick={() => onOpen(!open)}>{tr("versions.new")}</Button>}
      </div>
      {open && <form className="space-y-2 rounded-lg border border-line bg-canvas-2 p-3" onSubmit={(event) => {
        event.preventDefault();
        if (!canGenerate || disabled || generating) return;
        onOpen(false);
        void generate({ source: actualSource, ...(model ? { model } : {}), ...(language ? { language } : {}) });
      }}>
        <p className="text-[12px] text-ink-2">{tr("versions.keepCurrent")}</p>
        <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,12rem),1fr))] gap-2">
          <label className="col-span-full min-w-0 space-y-1 text-xs text-ink-2">{tr("versions.source")}
            <Select className={singleLineSelect} title={sourceLabel} value={actualSource} onChange={(event) => setSource(event.target.value === "participants" ? "participants" : event.target.value === "combined" ? "combined" : "recording")}>
              <option value="recording" disabled={!hasAudio}>{tr("versions.sources.recording")}</option>
              {list.participant_source && <option value="participants">{tr("versions.participantSource", { provider: providerName(list.participant_source) })}</option>}
              {list.participant_source && hasAudio && <option value="combined">{tr("versions.sources.combined")}</option>}
            </Select>
          </label>
          <label className="min-w-0 space-y-1 text-xs text-ink-2">{tr("versions.model")}
            <Select className={singleLineSelect} title={modelLabel} value={model} onChange={(event) => setModel(event.target.value)}>
              <option value="">{tr("versions.currentModel")}</option>
              {models.map(({ spec }) => <option key={spec.id} value={spec.id}>{spec.name}</option>)}
            </Select>
          </label>
          <label className="min-w-0 space-y-1 text-xs text-ink-2">{tr("versions.language")}
            <Select className={singleLineSelect} title={language ? languageLabel(language) : tr("versions.currentLanguage")} value={language} onChange={(event) => setLanguage(event.target.value)}>
              <option value="">{tr("versions.currentLanguage")}</option>
              {LANGUAGES.map(({ code }) => <option key={code ?? "auto"} value={code ?? "auto"}>{languageLabel(code ?? "auto")}</option>)}
            </Select>
          </label>
        </div>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" size="sm" onClick={() => onOpen(false)}>{tr("actions.cancel")}</Button>
          <Button type="submit" size="sm" disabled={!canGenerate || disabled || generating}>{tr("versions.generate")}</Button>
        </div>
      </form>}
      {completion ? <OperationProgress label={tr("versions.progress.succeeded")} announceStage value={1} {...completion} />
        : generating && <div className="flex items-center gap-2">
          <OperationProgress className="flex-1" label={tr(`versions.progress.${phase ?? "queued"}`)} announceStage
            startedAt={startedAt} value={progress == null ? undefined : Math.min(progress, 0.99)} />
          {canCancel && <Button type="button" variant="outline" size="sm" onClick={() => void cancel()}>{tr("actions.cancel")}</Button>}
        </div>}
      {terminal === "cancelled" && <p role="status" className="text-xs text-ink-3">{tr("versions.progress.cancelled")}</p>}
      {loading && <p role="status" className="text-xs text-ink-3">{tr("versions.loadingVersion")}</p>}
      {loadError != null && <div className="flex items-center gap-2">
        <p role="alert" className="text-xs text-ember">{tr("versions.loadFailed")} {displayError(loadError)}</p>
        <Button type="button" variant="ghost" size="sm" onClick={() => void refresh()}>{tr("actions.retry")}</Button>
      </div>}
      {selectionError != null && <p role="alert" className="text-xs text-ember">{tr("versions.selectionFailed")} {displayError(selectionError)}</p>}
      {generationError != null && <p role="alert" className="text-xs text-ember">{tr("versions.generationFailed")} {displayError(generationError)}</p>}
    </div>
  );
}

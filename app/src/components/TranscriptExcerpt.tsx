import { useState } from "react";
import { useTranslation } from "react-i18next";
import { segmentText } from "@/lib/export";
import type { Segment } from "@/lib/ipc";
import { stampToMs } from "./Markdown";
import { Button, Input } from "./ui";

function parseStamp(value: string) {
  const fields = value.trim().split(":");
  if (fields.slice(1).some((field) => Number(field) >= 60)) return null;
  const ms = stampToMs(value);
  return ms !== null && Number.isSafeInteger(ms) ? ms : null;
}

interface Props {
  segments: Segment[];
  useClean: boolean;
  disabled: boolean;
  canPreview?: boolean;
  hint: string;
  previewLabel: string;
  onPreview: (segments: Segment[]) => void;
}

export function TranscriptExcerpt({ segments, useClean, disabled, canPreview = true, hint, previewLabel, onPreview }: Props) {
  const { t } = useTranslation("meeting");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [error, setError] = useState<"range" | "empty" | null>(null);

  const preview = () => {
    const start = parseStamp(from);
    const end = parseStamp(to);
    if (start === null || end === null || start >= end) {
      setError("range");
      return;
    }
    const selected = segments.filter((segment) => (
      segment.start_ms >= start && segment.start_ms < end && segmentText(segment, useClean)
    ));
    if (selected.length === 0) {
      setError("empty");
      return;
    }
    setError(null);
    onPreview(selected);
  };

  return (
    <fieldset className="space-y-2 text-[13px]" disabled={disabled}>
      <legend>{t("subscription.excerpt")}</legend>
      <p className="text-[12px] text-ink-3">{hint}</p>
      <div className="flex flex-wrap items-end gap-2">
        <label className="block">
          {t("subscription.excerptFrom")}
          <Input className="block w-24" value={from} placeholder="0:00" onChange={(event) => { setFrom(event.target.value); setError(null); }} />
        </label>
        <label className="block">
          {t("subscription.excerptTo")}
          <Input className="block w-24" value={to} placeholder="1:30" onChange={(event) => { setTo(event.target.value); setError(null); }} />
        </label>
        <Button type="button" variant="outline" size="sm" disabled={!canPreview || !from.trim() || !to.trim()} onClick={preview}>
          {previewLabel}
        </Button>
      </div>
      {error && <p role="alert" className="text-ember">{t(error === "range" ? "subscription.excerptInvalid" : "subscription.excerptEmpty")}</p>}
    </fieldset>
  );
}

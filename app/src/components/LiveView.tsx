import { useTranslation } from "react-i18next";
// The Meeting page while its recording is still running: captions as they
// land (provisional lines refine in place) and running notes that the
// local model keeps updating.
import { Radio } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "@/components/Markdown";
import { Transcript, type TranscriptLine } from "@/components/Transcript";
import { fmtDuration } from "@/lib/format";
import type { Caption } from "@/lib/ipc";
import { captionDisplayName, captionSpeakerKey } from "@/lib/useLiveFeed";

export { useLiveFeed } from "@/lib/useLiveFeed";

function line(caption: Caption, id: number, recordingAudioLabel: string): TranscriptLine {
  return { id, start_ms: caption.start_ms, end_ms: caption.end_ms, speaker: captionDisplayName(caption, recordingAudioLabel), text: caption.text, clean_text: null };
}

export function LiveTranscript({ finals, partials, autoFollow = true }: { finals: Caption[]; partials: Caption[]; autoFollow?: boolean }) {
  const { t: tr } = useTranslation(["meeting", "recording"]);
  const recordingAudioLabel = tr("recording:bar.recordingAudio");
  const end = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const finalized = useMemo(() => finals.map((caption, i) => line(caption, i, recordingAudioLabel)).sort((a, b) => a.start_ms - b.start_ms), [finals, recordingAudioLabel]);
  const segments = useMemo(() => [...finalized, ...partials.map((caption, i) => ({
    ...line(caption, -i - 1, recordingAudioLabel), provisionalKey: `partial-${captionSpeakerKey(caption)}`,
  }))], [finalized, partials, recordingAudioLabel]);
  useEffect(() => {
    if (autoFollow && follow) end.current?.scrollIntoView({ block: "end" });
  }, [segments, autoFollow, follow]);

  if (finals.length === 0 && partials.length === 0) {
    return (
      <p className="inline-flex items-center gap-2 text-sm text-ink-3">
        <Radio size={14} className="text-ember" /> {tr("live.listening")}
      </p>
    );
  }
  return (
    <div
      className="max-h-[70vh] overflow-y-auto pr-1"
      onScroll={(e) => {
        const el = e.currentTarget;
        setFollow(el.scrollHeight - el.scrollTop - el.clientHeight < 40);
      }}
    >
      <Transcript segments={segments} currentMs={0} autoScroll={false} />
      <div ref={end} />
    </div>
  );
}

export function LiveNotes({ notes, updatedMs }: { notes: string; updatedMs: number | null }) {
  const { t: tr } = useTranslation("meeting");
  if (!notes) {
    return (
      <p className="text-sm text-ink-3">
        {tr("live.notesHint")}
      </p>
    );
  }
  return (
    <div>
      <div className="mb-3 inline-flex items-center gap-2 rounded-full bg-ember-soft px-2.5 py-0.5 text-[11px] font-semibold text-ember">
        <Radio size={12} /> {updatedMs != null ? tr("live.notesAt", { time: fmtDuration(updatedMs) }) : tr("live.notes")}
      </div>
      <Markdown text={notes} />
    </div>
  );
}

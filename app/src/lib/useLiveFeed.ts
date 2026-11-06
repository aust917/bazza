import { useEffect, useState } from "react";
import { cmd, on, subscribe, type Caption, type LiveSnapshot } from "./ipc";

type LiveFeed = Omit<LiveSnapshot, "partial" | "partials"> & { partials: Caption[] };

export function captionSpeakerKey(caption: Caption) {
  if (caption.speaker_id != null) return `id:${caption.speaker_id}`;
  return caption.speaker_name != null ? `name:${caption.speaker_name}` : "";
}

export function captionDisplayName(caption: Caption, recordingAudioLabel: string) {
  return caption.speaker_name ?? (caption.speaker_id === "local:mix" ? recordingAudioLabel : null);
}

function clearOnly(caption: Caption) {
  return caption.is_final && caption.text === "" && captionSpeakerKey(caption) !== "";
}

function applyCaption(feed: LiveFeed, caption: Caption, maxFinals: number): LiveFeed {
  const speaker = captionSpeakerKey(caption);
  if (caption.is_final) {
    return {
      ...feed,
      finals: clearOnly(caption) ? feed.finals : [...feed.finals, caption].slice(-maxFinals),
      partials: feed.partials.filter((partial) => captionSpeakerKey(partial) !== speaker),
    };
  }
  const index = feed.partials.findIndex((partial) => captionSpeakerKey(partial) === speaker);
  const partials = [...feed.partials];
  if (index === -1) partials.push(caption);
  else partials[index] = caption;
  return { ...feed, partials };
}

function finalKey(caption: Caption) {
  return JSON.stringify([captionSpeakerKey(caption), caption.start_ms, caption.end_ms, caption.text]);
}

function mergeSnapshot(snapshot: LiveSnapshot, buffered: Caption[], maxFinals: number): LiveFeed {
  const { partial, partials, ...feed } = snapshot;
  const snapshotPartials = partial ? [partial, ...(partials ?? [])] : partials ?? [];
  let result: LiveFeed = { ...feed, finals: feed.finals.slice(-maxFinals), partials: snapshotPartials };
  if (buffered.length === 0) return result;

  const knownFinals = new Set<string>();
  const finalEnd = new Map<string, number>();
  const partialEnd = new Map(snapshotPartials.map((caption) => [captionSpeakerKey(caption), caption.end_ms]));
  for (const caption of feed.finals) {
    const speaker = captionSpeakerKey(caption);
    knownFinals.add(finalKey(caption));
    finalEnd.set(speaker, Math.max(finalEnd.get(speaker) ?? -Infinity, caption.end_ms));
  }
  for (const caption of buffered) {
    const speaker = captionSpeakerKey(caption);
    const snapshotPartialEnd = partialEnd.get(speaker) ?? -Infinity;
    // An event can already be represented in the snapshot taken after subscribing.
    if (caption.is_final) {
      if (clearOnly(caption) ? caption.end_ms < snapshotPartialEnd : knownFinals.has(finalKey(caption))) continue;
    } else if (caption.end_ms <= Math.max(finalEnd.get(speaker) ?? -Infinity, snapshotPartialEnd)) {
      continue;
    }
    result = applyCaption(result, caption, maxFinals);
  }
  return result;
}

export function useLiveFeed(meetingId: string | null, maxFinals = Infinity) {
  const [feed, setFeed] = useState<LiveFeed | null>(null);
  useEffect(() => {
    if (!meetingId) {
      setFeed(null);
      return;
    }
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let buffered: Caption[] | null = [];
    const empty: LiveFeed = { meeting_id: meetingId, finals: [], partials: [], notes: "", notes_updated_ms: null };
    const stopNotes = subscribe("live_notes", (update) => {
      if (cancelled || update.meeting_id !== meetingId) return;
      setFeed((current) => ({
        ...(current?.meeting_id === meetingId ? current : empty),
        notes: update.notes,
        notes_updated_ms: update.updated_ms,
      }));
    });
    void on("caption", (caption) => {
      if (cancelled || (caption.meeting_id != null && caption.meeting_id !== meetingId)) return;
      buffered?.push(caption);
      setFeed((current) => applyCaption(current?.meeting_id === meetingId ? current : empty, caption, maxFinals));
    }).then(async (stop) => {
      if (cancelled) return stop();
      unlisten = stop;
      const snapshot = await cmd.liveSnapshot();
      if (cancelled || snapshot?.meeting_id !== meetingId) return;
      const merged = mergeSnapshot(snapshot, buffered ?? [], maxFinals);
      setFeed((current) => current?.meeting_id === meetingId && current.notes_updated_ms != null
        && current.notes_updated_ms > (merged.notes_updated_ms ?? -Infinity)
        ? { ...merged, notes: current.notes, notes_updated_ms: current.notes_updated_ms }
        : merged);
    }).catch(() => {}).finally(() => { buffered = null; });
    return () => {
      cancelled = true;
      buffered = null;
      unlisten?.();
      stopNotes();
    };
  }, [meetingId, maxFinals]);
  return meetingId != null && feed?.meeting_id === meetingId ? feed : null;
}

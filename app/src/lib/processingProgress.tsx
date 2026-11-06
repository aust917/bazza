import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { subscribe, type Events } from "./ipc";

type ProcessingEvent = Events["processing"];
export type ProcessingSnapshot = ProcessingEvent & { startedAt: number; finishedAt?: number; reconciled: boolean };
const RETAINED_MEETINGS = 32;

function useProcessingFeed(enabled: boolean, meetingId?: string) {
  const [snapshots, setSnapshots] = useState(() => new Map<string, ProcessingSnapshot>());
  const report = useCallback((event: ProcessingEvent, expected?: ProcessingSnapshot | null) => {
    const now = performance.now();
    setSnapshots((current) => {
      const previous = current.get(event.meeting_id) ?? null;
      if (expected !== undefined && previous !== expected) return current;
      const terminal = event.stage === "done" || event.stage === "failed";
      if (terminal && previous?.finishedAt !== undefined && event.stage === previous.stage && event.error === previous.error) {
        return current;
      }
      const next = new Map(current);
      next.delete(event.meeting_id);
      next.set(event.meeting_id, {
        ...event,
        startedAt: terminal || previous?.finishedAt === undefined ? previous?.startedAt ?? now : now,
        finishedAt: terminal ? now : undefined,
        reconciled: terminal && expected !== undefined,
      });
      if (next.size > RETAINED_MEETINGS) {
        const oldest = next.keys().next().value;
        if (oldest !== undefined) next.delete(oldest);
      }
      return next;
    });
  }, []);
  useEffect(() => {
    if (!enabled) return;
    return subscribe("processing", (event) => {
      if (meetingId === undefined || event.meeting_id === meetingId) report(event);
    });
  }, [enabled, meetingId, report]);
  return { snapshots, report };
}

const ProcessingContext = createContext<ReturnType<typeof useProcessingFeed> | null>(null);

export function ProcessingProgressProvider({ children }: { children: ReactNode }) {
  const feed = useProcessingFeed(true);
  return <ProcessingContext.Provider value={feed}>{children}</ProcessingContext.Provider>;
}

export function useProcessingProgress(meetingId: string) {
  const shared = useContext(ProcessingContext);
  const local = useProcessingFeed(shared === null, meetingId);
  const { snapshots, report } = shared ?? local;
  return { snapshot: snapshots.get(meetingId) ?? null, report };
}

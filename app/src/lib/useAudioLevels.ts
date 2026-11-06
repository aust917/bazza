import { useEffect, useRef, useState } from "react";
import { cmd } from "./ipc";

const POLL_DELAY_MS = 600;

/** Device probes open native audio streams, so leave a gap even when a probe fails. */
export function useAudioLevels({ enabled, mic = null, loopback = null, onlyWhenIdle = false }: {
  enabled: boolean;
  mic?: string | null;
  loopback?: string | null;
  onlyWhenIdle?: boolean;
}): [number, number] {
  const [levels, setLevels] = useState<[number, number]>([-100, -100]);
  // Keep these across effect restarts so a device change cannot overlap an old probe.
  const inFlight = useRef(false);
  const nextPollAt = useRef(0);

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const active = () => !disposed && !document.hidden;
    const schedule = (delay = nextPollAt.current - Date.now()) => {
      clearTimeout(timer);
      if (active()) timer = setTimeout(() => void poll(), Math.max(0, delay));
    };
    const poll = async () => {
      if (!active()) return;
      if (inFlight.current) {
        schedule(POLL_DELAY_MS);
        return;
      }
      if (nextPollAt.current > Date.now()) {
        schedule();
        return;
      }
      inFlight.current = true;
      try {
        if (onlyWhenIdle) {
          const status = await cmd.recordingStatus();
          // The window may have closed or hidden while waiting for the status.
          if (!active() || status.state !== "idle") return;
        }
        const result = await cmd.sampleLevels(mic, loopback);
        if (active()) setLevels(result);
      } catch {
        // An unavailable device should leave the UI usable and retry at the same pace.
      } finally {
        inFlight.current = false;
        nextPollAt.current = Date.now() + POLL_DELAY_MS;
        schedule();
      }
    };
    const visibilityChanged = () => {
      if (document.hidden) clearTimeout(timer);
      else schedule();
    };
    document.addEventListener("visibilitychange", visibilityChanged);
    void poll();
    return () => {
      disposed = true;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", visibilityChanged);
    };
  }, [enabled, mic, loopback, onlyWhenIdle]);

  return levels;
}

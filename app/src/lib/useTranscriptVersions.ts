import { useCallback, useEffect, useEffectEvent, useRef, useState } from "react";
import { cmd, subscribe, type MeetingStatus, type TranscriptVersion, type TranscriptVersionJob, type TranscriptVersionOptions, type TranscriptVersionProgress, type TranscriptVersionSegment, type TranscriptVersions } from "./ipc";

export const ORIGINAL_TRANSCRIPT = "original";
const EMPTY: TranscriptVersions = { versions: [], initial_id: null, participant_source: null };

const active = (job: TranscriptVersionJob | null) => job !== null
  && job.phase !== "succeeded" && job.phase !== "failed" && job.phase !== "cancelled";

export function useTranscriptVersions(id: string, status?: MeetingStatus, onGenerated?: () => void) {
  const [list, setList] = useState(EMPTY);
  const [listResolvedAt, setListResolvedAt] = useState<MeetingStatus>();
  const [selected, setSelected] = useState<{ id: string; segments: TranscriptVersionSegment[] } | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [selectionError, setSelectionError] = useState<unknown>(null);
  const [generationError, setGenerationError] = useState<unknown>(null);
  const [localStart, setLocalStart] = useState<number>();
  const [localProgress, setLocalProgress] = useState<TranscriptVersionProgress | null>(null);
  const [localCompletion, setLocalCompletion] = useState<{ startedAt: number; finishedAt: number }>();
  const [snapshot, setSnapshot] = useState<{ job: TranscriptVersionJob; observedAt: number; startedAt: number } | null>(null);
  const [jobRefresh, setJobRefresh] = useState(0);
  const mounted = useRef(false);
  const running = useRef(false);
  const ignoredJob = useRef<string | undefined>(undefined);
  const revealedVersion = useRef<string | undefined>(undefined);
  const requestList = useRef(0);
  const requestSelection = useRef(0);
  const listRef = useRef(EMPTY);
  const explicitChoice = useRef<string | null>(null);
  const choiceRevision = useRef(0);
  const generationChoice = useRef<number | undefined>(undefined);

  const select = useCallback(async (versionId: string, explicit = true) => {
    if (explicit) {
      explicitChoice.current = versionId;
      choiceRevision.current++;
    }
    const request = ++requestSelection.current;
    setSelectionError(null);
    // Old libraries use the canonical transcript for the initial version. Once
    // another version is effective, "original" means its immutable archive.
    if (versionId === ORIGINAL_TRANSCRIPT && !listRef.current.effective_id) {
      setSelected(null);
      setLoading(false);
      return true;
    }
    setLoading(true);
    try {
      const segments = await cmd.transcriptVersion(id, versionId);
      if (!mounted.current || requestSelection.current !== request) return false;
      setSelected({ id: versionId, segments });
      return true;
    } catch (error) {
      if (mounted.current && requestSelection.current === request) setSelectionError(error);
      return false;
    } finally {
      if (mounted.current && requestSelection.current === request) setLoading(false);
    }
  }, [id]);

  const refresh = useCallback(async () => {
    const request = ++requestList.current;
    try {
      const result = await cmd.transcriptVersions(id);
      if (!mounted.current || requestList.current !== request) return;
      listRef.current = result;
      setList(result);
      setListResolvedAt(status);
      setLoadError(null);
      await select(explicitChoice.current ?? result.effective_id ?? ORIGINAL_TRANSCRIPT, false);
    } catch (error) {
      if (mounted.current && requestList.current === request) setLoadError(error);
    }
  }, [id, status, select]);

  useEffect(() => {
    mounted.current = true;
    const stop = subscribe("transcript_version_progress", (update) => {
      if (update.meeting_id === id && running.current) setLocalProgress(update);
    });
    return () => {
      mounted.current = false;
      requestList.current++;
      requestSelection.current++;
      stop();
    };
  }, [id]);
  useEffect(() => {
    if (status && status !== "recording") void refresh();
  }, [status, refresh]);

  const reveal = async (version: TranscriptVersion, intendedRevision?: number) => {
    if (revealedVersion.current === version.id) return;
    revealedVersion.current = version.id;
    setList((current) => ({ ...current, versions: [...current.versions.filter((item) => item.id !== version.id), version] }));
    const requestedHere = intendedRevision !== undefined;
    const canReveal = requestedHere ? intendedRevision === choiceRevision.current
      : explicitChoice.current === null && version.source !== "combined"
        && (!listRef.current.effective_id || listRef.current.effective_id === ORIGINAL_TRANSCRIPT);
    if (canReveal && await select(version.id)) onGenerated?.();
    await refresh();
  };

  const acceptSnapshot = useEffectEvent((job: TranscriptVersionJob | null) => {
    if (!job || job.id === ignoredJob.current) return false;
    const observedAt = performance.now();
    setSnapshot((current) => ({ job, observedAt,
      startedAt: current?.job.id === job.id ? current.startedAt : observedAt - job.elapsed_ms }));
    if (job.phase === "succeeded" && job.version) void reveal(job.version, generationChoice.current);
    return active(job);
  });

  useEffect(() => {
    let cancelled = false;
    let first = true;
    let knownActive = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const job = await cmd.transcriptVersionJob(id);
        if (!cancelled) knownActive = acceptSnapshot(job);
      } catch { /* The invoking tab still receives progress events and command errors. */ }
      if (!cancelled && (first || knownActive || running.current || status === "processing")) timer = setTimeout(() => void poll(), 650);
      first = false;
    };
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [id, status, localStart, jobRefresh]);

  const job = snapshot?.job ?? null;
  const nativeActive = active(job);
  const startedAt = localStart ?? (nativeActive ? snapshot?.startedAt : undefined);

  async function cancel() {
    if (!job?.can_cancel) return;
    try {
      if (await cmd.cancelTranscriptVersion(id, job.id)) {
        setSnapshot((current) => current?.job.id === job.id
          ? { ...current, job: { ...current.job, phase: "cancelling", progress: null, can_cancel: false } }
          : current);
      }
    } catch (error) {
      if (mounted.current) setGenerationError(error);
    } finally {
      if (mounted.current) setJobRefresh((value) => value + 1);
    }
  }

  async function generate(options: TranscriptVersionOptions) {
    if (running.current || nativeActive) return;
    running.current = true;
    const intendedRevision = choiceRevision.current;
    generationChoice.current = intendedRevision;
    const startedAt = performance.now();
    setLocalStart(startedAt);
    setLocalProgress(null);
    setLocalCompletion(undefined);
    setGenerationError(null);
    ignoredJob.current = job?.id;
    setSnapshot(null);
    try {
      const version = await cmd.regenerateTranscript(id, options);
      if (mounted.current) {
        await reveal(version, intendedRevision);
        if (mounted.current) setLocalCompletion({ startedAt, finishedAt: performance.now() });
      }
    } catch (error) {
      if (mounted.current && !(typeof error === "object" && error && "code" in error && error.code === "native.transcriptVersionCancelled")) {
        setGenerationError(error);
      }
    } finally {
      running.current = false;
      generationChoice.current = undefined;
      if (mounted.current) setLocalStart(undefined);
    }
  }

  const terminal = job?.phase === "succeeded" || job?.phase === "cancelled" || job?.phase === "failed" ? job.phase : undefined;
  const completion = job?.phase === "succeeded" && snapshot
    ? { startedAt: snapshot.observedAt - job.elapsed_ms, finishedAt: snapshot.observedAt }
    : localCompletion;
  const phase = job?.phase === "cancelling" ? job.phase : localStart !== undefined && localProgress ? localProgress.phase : job?.phase;
  const progress = job?.phase === "cancelling" ? null : localStart !== undefined && localProgress ? localProgress.progress : job?.progress;

  const updateCorrection = useCallback((segmentId: number, cleanText: string | null, manual: boolean) => {
    setSelected((current) => current && current.id === listRef.current.effective_id ? {
      ...current, segments: current.segments.map((row) => row.id === segmentId ? {
        ...row, clean_text: cleanText, clean_text_manual: manual,
      } : row),
    } : current);
  }, []);

  return { list, listResolved: status !== undefined && listResolvedAt === status,
    selected, loading, loadError, selectionError, generationError: job?.error ?? generationError,
    startedAt, phase, progress, terminal, completion, canCancel: job?.can_cancel && phase !== "saving", refresh, select, generate, cancel, updateCorrection };
}

import { Trans, useTranslation } from "react-i18next";
import { displayError } from "@/lib/i18n";
import { save } from "@tauri-apps/plugin-dialog";
import { AlertTriangle, ArrowLeft, Check, Copy, Download, FolderOpen, Mail, Pencil, RefreshCw, Share2, Tag as TagIcon, Trash2, Users, X } from "lucide-react";
import { useCallback, useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ChatPanel } from "@/components/ChatPanel";
import { AttendeeTracks } from "@/components/AttendeeTracks";
import { BotRecordingStatus } from "@/components/BotRecordingStatus";
import { BotTranscripts } from "@/components/BotTranscripts";
import { LiveNotes, LiveTranscript, useLiveFeed } from "@/components/LiveView";
import { Markdown } from "@/components/Markdown";
import { Player, type PlayerHandle } from "@/components/Player";
import { ShareDialog } from "@/components/ShareDialog";
import { ServiceSummary } from "@/components/ServiceSummary";
import { SpeakersPanel } from "@/components/SpeakersPanel";
import { SubscriptionChat } from "@/components/SubscriptionChat";
import { SummaryControls } from "@/components/SummaryControls";
import { Transcript } from "@/components/Transcript";
import { TranscriptVersionControls, type TranscriptChoice } from "@/components/TranscriptVersionControls";
import { Badge, Button, Input, Menu, SectionTitle, Spinner } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { safeFileName, transcriptMd, transcriptSrt, transcriptTxt } from "@/lib/export";
import { appLabel, fmtDate, fmtDuration, fmtTime, speakerLabel } from "@/lib/format";
import { cmd, subscribe, type BotTranscriptProvider, type Caption, type Folder, type MeetingDetail, type ModelRow, type SttTranscript, type Summary, type Tag, type Template, type TranscriptPlaybackTiming } from "@/lib/ipc";
import { useProcessingProgress, type ProcessingSnapshot } from "@/lib/processingProgress";
import { talkTime } from "@/lib/talktime";
import { ORIGINAL_TRANSCRIPT, useTranscriptVersions } from "@/lib/useTranscriptVersions";

const TALK_COLORS = ["bg-ink", "bg-moss", "bg-amber", "bg-ember", "bg-ink-3", "bg-line-2"];
const STAGES = [
  ["mux", "stages.mux"],
  ["transcribe", "stages.transcribe"],
  ["supplement", "stages.supplement"],
  ["diarize", "stages.diarize"],
  ["cleanup", "stages.cleanup"],
  ["embed", "stages.embed"],
  ["title", "stages.title"],
  ["summary", "stages.summary"],
] as const;
type TextKind = "transcript" | "summary" | "subtitles" | "markdown" | "email";
type FileFeedback = { kind: "copied" | "saved"; item: TextKind };

export function MeetingPage({ serviceDesktop = false }: { serviceDesktop?: boolean } = {}) {
  const { id = "" } = useParams();
  return <MeetingView key={id} serviceDesktop={serviceDesktop} />;
}

function releaseMedia(lease: string | undefined) {
  if (lease) void cmd.releaseMeetingMedia(lease).catch(() => {});
}

function MeetingView({ serviceDesktop }: { serviceDesktop: boolean }) {
  const { t: tr } = useTranslation("meeting");
  const { id = "" } = useParams();
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [currentMs, setCurrentMs] = useState(0);
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState("");
  const { snapshot, report: reportProgress } = useProcessingProgress(id);
  const [confirmedCompletion, setConfirmedCompletion] = useState<ProcessingSnapshot>();
  const [summaryStartedAt, setSummaryStartedAt] = useState<number>();
  const stage = snapshot?.finishedAt === undefined ? snapshot?.stage ?? null : null;
  const [view, setView] = useState<"clean" | "raw" | null>(null);
  const [botTranscript, setBotTranscript] = useState<BotTranscriptProvider | null>(null);
  const [availableBotTranscripts, setAvailableBotTranscripts] = useState<BotTranscriptProvider[]>([]);
  const [versionSettings, setVersionSettings] = useState(false);
  const [dismissed, setDismissed] = useState<string[]>([]);
  const [tab, setTab] = useState<"transcript" | "summary" | "chat">("transcript");
  const [summary, setSummary] = useState<Summary | null>(null);
  const [stt, setStt] = useState<SttTranscript | null | undefined>();
  const [templates, setTemplates] = useState<Template[]>([]);
  const [templateId, setTemplateId] = useState<string>("");
  const [share, setShare] = useState(false);
  const [tags, setTags] = useState<Tag[]>([]);
  const [folders, setFolders] = useState<Folder[]>([]);
  const [tagDraft, setTagDraft] = useState("");
  const [asrModels, setAsrModels] = useState<ModelRow[]>([]);
  const [knownSpeakers, setKnownSpeakers] = useState<string[]>([]);
  const [followup, setFollowup] = useState<string | null>(null);
  const [followupBusy, setFollowupBusy] = useState(false);
  const [flash, setFlash] = useState<FileFeedback | null>(null);
  const [stopping, setStopping] = useState(false);
  const [correcting, setCorrecting] = useState(false);
  const player = useRef<PlayerHandle>(null);
  const mounted = useRef(true);
  const detailRevision = useRef(0);
  const correctionRequest = useRef(0);
  const initialLoad = useRef(false);
  const latestProgress = useRef(snapshot);
  const processingStage = useRef<string | undefined>(undefined);
  const correctionInFlight = useRef<number | null>(null);
  const currentMedia = useRef<Pick<MeetingDetail, "media" | "meeting"> | null>(null);
  const lastLiveFinals = useRef<Caption[]>([]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      initialLoad.current = false;
      detailRevision.current++;
      correctionRequest.current++;
      releaseMedia(currentMedia.current?.media.lease);
      currentMedia.current = null;
    };
  }, []);
  const live = useLiveFeed(detail?.meeting.status === "recording" ? id : null);
  const watchedStatus = detail?.meeting.id === id ? detail.meeting.status : undefined;
  const versions = useTranscriptVersions(id, watchedStatus, () => setBotTranscript(null));
  const displayedVersion = versions.list.versions.find((version) => version.id === (versions.selected?.id ?? versions.list.effective_id ?? ORIGINAL_TRANSCRIPT));
  const displayedSource = displayedVersion?.source;
  const participantVersion = displayedSource === "meet" || displayedSource === "discord";
  const combinedVersion = displayedSource === "combined";
  const recordingDuration = detail?.meeting.duration_ms ?? 0;
  const participantTimings = useMemo(() => {
    if (!participantVersion && !combinedVersion) return undefined;
    const timings = new Map<number, TranscriptPlaybackTiming>();
    if (!versions.selected || !Number.isFinite(recordingDuration)) return timings;
    for (const { id, playback: mapped, provenance, start_ms, end_ms } of versions.selected.segments) {
      const playback = mapped ?? (combinedVersion && provenance && provenance.source !== "bot" ? { start_ms, end_ms } : undefined);
      if (playback && Number.isSafeInteger(playback.start_ms) && Number.isSafeInteger(playback.end_ms)
        && playback.start_ms >= 0 && playback.start_ms < recordingDuration
        && playback.end_ms >= playback.start_ms && playback.end_ms <= recordingDuration) {
        timings.set(id, playback);
      }
    }
    return timings;
  }, [participantVersion, combinedVersion, versions.selected, recordingDuration]);
  const refreshCompletedVersions = useEffectEvent(() => {
    // A processing -> ready transition already refreshes the version list. A
    // completion while the page was ready still needs to discover promotion.
    if (watchedStatus === "ready" || watchedStatus === "failed") void versions.refresh();
  });
  useEffect(() => {
    if (snapshot?.finishedAt !== undefined) refreshCompletedVersions();
  }, [snapshot?.finishedAt]);
  useEffect(() => {
    if (live?.finals.length) lastLiveFinals.current = live.finals;
    else if (watchedStatus === "ready" || watchedStatus === "failed") lastLiveFinals.current = [];
  }, [live?.finals, watchedStatus]);
  useEffect(() => { setCurrentMs(0); }, [detail?.media.audio, detail?.media.video]);

  const say = (msg: FileFeedback) => {
    setFlash(msg);
    setTimeout(() => setFlash(null), 1800);
  };

  const load = useCallback(() => {
    const request = ++detailRevision.current;
    const loading = cmd.getMeeting(id).then((d) => {
      if (!mounted.current || request !== detailRevision.current) {
        releaseMedia(d.media.lease);
        return;
      }
      if (serviceDesktop) {
        const previous = currentMedia.current;
        if (previous?.media.lease && d.media.lease
          && previous.meeting.id === d.meeting.id
          && previous.meeting.lifecycle_revision === d.meeting.lifecycle_revision
          && Boolean(previous.media.audio) === Boolean(d.media.audio)
          && Boolean(previous.media.video) === Boolean(d.media.video)) {
          releaseMedia(d.media.lease);
          d = { ...d, media: previous.media };
        } else {
          releaseMedia(previous?.media.lease);
        }
        currentMedia.current = { media: d.media, meeting: d.meeting };
      }
      setDetail(d);
      setTitle(d.meeting.title);
      setError(null);
      return d.meeting.status;
    }, (e) => {
      if (!mounted.current || request !== detailRevision.current) return;
      releaseMedia(currentMedia.current?.media.lease);
      currentMedia.current = null;
      setError(e);
    });
    if (serviceDesktop) return loading;
    void cmd.getSummary(id).then((s) => {
      setSummary(s);
      if (s) setTemplateId((t) => t || s.template_id);
    });
    void cmd.sttTranscript(id).then(setStt).catch(() => setStt(null));
    void cmd.meetingTags(id).then(setTags);
    void cmd.listFolders().then(setFolders);
    return loading;
  }, [id, serviceDesktop]);

  useEffect(() => {
    void cmd.listModels().then((ms) => setAsrModels(ms.filter((m) => m.spec.kind === "asr" && m.present))).catch(() => {});
    if (serviceDesktop) return;
    void cmd.listTemplates().then(setTemplates);
    void cmd.listKnownSpeakers().then((ks) => setKnownSpeakers((ks ?? []).map((k) => k.name))).catch(() => {});
  }, [serviceDesktop]);

  useEffect(() => subscribe("participants", (p) => {
    if (p.meeting_id === id) setDetail((d) => (d ? { ...d, meeting: { ...d.meeting, participants: p.participants } } : d));
  }), [id]);

  // Stop takes a few seconds (last captions, video trailer); the page says so until the status flips.
  useEffect(() => subscribe("recording_state", (s) => {
    if (s.state === "idle") {
      setStopping(true);
      load();
    }
  }), [load]);
  useEffect(() => {
    if (detail && detail.meeting.status !== "recording") setStopping(false);
  }, [detail]);

  useEffect(() => {
    latestProgress.current = snapshot;
    // Re-transcription can replace rows (including reusing their IDs).
    if (processingStage.current !== snapshot?.stage && (stage === "mux" || stage === "transcribe")) correctionRequest.current++;
    processingStage.current = snapshot?.stage;
    if (initialLoad.current && (snapshot?.reconciled || (snapshot?.finishedAt === undefined && stage !== "title"))) return;
    initialLoad.current = true;
    void load().then((status) => {
      if (status === "ready" && snapshot?.finishedAt !== undefined) setConfirmedCompletion(snapshot);
    });
  }, [load, snapshot, stage]);

  useEffect(() => {
    if (watchedStatus === "processing" && !snapshot) {
      reportProgress({ meeting_id: id, stage: "queued", progress: 0, error: null }, null);
    }
  }, [id, reportProgress, snapshot, watchedStatus]);

  useEffect(() => {
    if (params.get("tab") === "chat") setTab("chat");
  }, [params]);

  useEffect(() => {
    const checkRecorder = !serviceDesktop && watchedStatus === "recording";
    if (!checkRecorder && watchedStatus !== "processing") return;
    let active = true;
    let refresh: number;
    const reconcile = async () => {
      if (checkRecorder) {
        const current = await cmd.recordingStatus();
        if (!active || current.meeting_id === id) return;
        setStopping(true);
      }
      const observed = latestProgress.current;
      const status = await load();
      if (!active || latestProgress.current !== observed) return;
      if (status === "ready" && observed?.finishedAt !== undefined) {
        setConfirmedCompletion(observed);
      } else if (status === "failed" || (status === "ready" && (!observed || ["queued", "mux", "transcribe", "diarize"].includes(observed.stage)))) {
        // Ready can precede optional cleanup, indexing and summary. Only
        // reconcile the initial pipeline stages from the persisted status.
        reportProgress({ meeting_id: id, stage: status === "ready" ? "done" : "failed", progress: 1, error: null }, observed);
      }
    };
    const schedule = () => {
      if (active) refresh = window.setTimeout(() => void reconcile().then(schedule, schedule), 3_000);
    };
    schedule();
    return () => { active = false; window.clearTimeout(refresh); };
  }, [id, load, reportProgress, serviceDesktop, watchedStatus]);

  // Deep link from search: ?t=<ms>
  useEffect(() => {
    const t = Number(params.get("t"));
    if (detail && t > 0) setTimeout(() => player.current?.seek(t), 300);
  }, [detail, params]);

  const activeTranscription = stage === "transcribe" ? snapshot?.transcription : undefined;
  const stageText = activeTranscription
    ? tr(`stages.transcription.${activeTranscription.phase}`)
    : tr(STAGES.find(([id]) => id === stage)?.[1] ?? (summaryStartedAt !== undefined ? "stages.summary" : "statuses.processing"));
  const stageProgress = activeTranscription?.progress ?? (stage === "cleanup" && snapshot && snapshot.progress >= 0.75
    ? (snapshot.progress - 0.75) / 0.25 : null);

  const onTime = useCallback((ms: number) => setCurrentMs(ms), []);
  const seek = useCallback((ms: number) => player.current?.seek(ms), []);
  const dismissSuggestion = useCallback((label: string) => setDismissed((d) => [...d, label]), []);
  const hasClean = useMemo(() => detail?.segments.some((s) => s.clean_text !== null) ?? false, [detail?.segments]);
  const speakers = useMemo(() => detail?.speakers.filter((sp) => !dismissed.includes(sp.label)) ?? [], [detail?.speakers, dismissed]);
  const platformSpeakerLabels = useMemo(() => new Set(stt?.live_speaker_labels ?? []), [stt]);
  const nameOptions = useMemo(() => Array.from(new Set([...(detail?.meeting.participants ?? []), ...knownSpeakers])).filter((n) => n.toLowerCase() !== "me"), [detail?.meeting.participants, knownSpeakers]);
  const shares = useMemo(() => talkTime(detail?.segments ?? []), [detail?.segments]);

  const renameSpeaker = useCallback(async (label: string, name: string) => {
    await cmd.renameSpeaker(id, label, name);
    load();
  }, [id, load]);
  const acceptSuggestion = useCallback(async (label: string) => {
    await cmd.acceptSpeakerSuggestion(id, label);
    load();
  }, [id, load]);
  const setCorrection = useCallback(async (segmentId: number, text: string | null) => {
    if (correctionInFlight.current !== null) return;
    const meetingId = id;
    const original = detail?.segments.find((segment) => segment.id === segmentId);
    if (!original) throw { code: "native.transcriptCorrectionSegment", args: {} };
    const request = ++correctionRequest.current;
    correctionInFlight.current = request;
    setCorrecting(true);
    detailRevision.current++;
    try {
      const segment = await cmd.setTranscriptCorrection(meetingId, segmentId, text);
      if (!mounted.current || request !== correctionRequest.current) return;
      if (segment.meeting_id !== meetingId || segment.id !== segmentId) throw { code: "native.transcriptCorrectionSegment", args: {} };
      detailRevision.current++; // Reject reads started while the write was pending.
      setDetail((current) => current?.meeting.id === meetingId && current.meeting.status === "ready" ? {
        ...current,
        segments: current.segments.map((row) => row.id === segmentId && row.text === original.text && row.start_ms === original.start_ms && row.end_ms === original.end_ms ? {
          ...row,
          clean_text: segment.clean_text,
          clean_text_manual: segment.clean_text_manual ?? false,
        } : row),
      } : current);
      versions.updateCorrection(segmentId, segment.clean_text, segment.clean_text_manual ?? false);
      setView("clean");
    } finally {
      if (mounted.current && correctionInFlight.current === request) {
        correctionInFlight.current = null;
        setCorrecting(false);
      }
    }
  }, [id, detail, versions.updateCorrection]);

  if (error !== null) {
    return (
      <div className="p-8 text-sm text-ember">
        {displayError(error)} <Link to="/" className="underline">{tr("page.back")}</Link>
      </div>
    );
  }
  if (!detail) return <div className="p-8"><Spinner /></div>;

  const m = detail.meeting;
  const processing = m.status === "processing" || stage !== null || summaryStartedAt !== undefined;
  const completed = !processing && m.status === "ready" && snapshot != null && (snapshot.reconciled || snapshot === confirmedCompletion)
    && snapshot.stage === "done" && snapshot.error == null;
  const recording = m.status === "recording";
  const hasAudio = detail.media.audio !== null;
  const hasMedia = hasAudio || detail.media.video !== null;
  const initialSource = displayedSource ?? versions.list.versions.find((version) => version.id === ORIGINAL_TRANSCRIPT)?.source;
  const defaultView = initialSource !== "live" && versions.listResolved && (!processing || initialSource) ? "clean" : "raw";
  const useClean = (view ?? defaultView) === "clean" && hasClean;
  const originalSelected = versions.list.effective_id
    ? versions.selected?.id === versions.list.effective_id : versions.selected === null;
  const canEditSpeakerLabels = !serviceDesktop && stt !== undefined
    && !detail.speaker_stats.some(({ label }) => platformSpeakerLabels.has(label));
  const displayedSegments = versions.selected?.segments ?? detail.segments;
  const displayedSeek = hasMedia && ((originalSelected && !combinedVersion) || displayedSource === "live" || displayedSource === "recording" || participantTimings?.size) ? seek : undefined;
  const participantTimingHint = displayedSeek && participantTimings?.size
    ? participantTimings.size === displayedSegments.length ? "participantRecordingTiming" : "participantMixedTiming"
    : "participantTiming";
  const canGenerateVersion = hasAudio || versions.list.participant_source !== null;
  const selectTranscript = async (choice: TranscriptChoice) => {
    if (choice.kind === "bot") {
      void versions.select(ORIGINAL_TRANSCRIPT);
      setBotTranscript(choice.id);
    } else if (await versions.select(choice.id)) {
      setBotTranscript(null);
    }
  };

  const saveTitle = async () => {
    const next = title.trim();
    if (next && next !== m.title) {
      try {
        const updated = serviceDesktop
          ? await cmd.renameMeeting(m.id, next, m.metadata_revision, crypto.randomUUID())
          : await cmd.renameMeeting(m.id, next);
        setDetail({ ...detail, meeting: updated });
      } catch (error) {
        alert(displayError(error));
        return;
      }
    }
    setEditing(false);
  };

  const remove = async () => {
    if (!confirm(tr("page.deleteConfirm", { title: m.title }))) return;
    try {
      if (serviceDesktop) await cmd.deleteMeeting(m.id, m.lifecycle_revision, crypto.randomUUID());
      else await cmd.deleteMeeting(m.id);
      navigate("/");
    } catch (error) {
      alert(displayError(error));
    }
  };

  const dateLine = tr("page.dateTime", { date: fmtDate(m.started_at), time: fmtTime(m.started_at) });
  const textLabels: Record<TextKind, string> = { transcript: tr("tabs.transcript"), summary: tr("tabs.summary"), subtitles: tr("page.subtitles"), markdown: "Markdown", email: tr("page.email") };
  const copyText = async (text: string, what: TextKind) => {
    try {
      await navigator.clipboard.writeText(text);
      say({ kind: "copied", item: what });
    } catch (e) {
      alert(tr("feedback.copyFailed", { error: displayError(e) }));
    }
  };
  const saveText = async (ext: string, text: string, what: TextKind) => {
    const out = await save({ defaultPath: `${safeFileName(m.title)}.${ext}`, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
    if (!out) return;
    try {
      await cmd.saveTextFile(out, text);
      say({ kind: "saved", item: what });
    } catch (e) {
      alert(displayError(e));
    }
  };
  const draftFollowup = async () => {
    setFollowupBusy(true);
    try {
      setFollowup(await cmd.draftFollowup(m.id));
    } catch (e) {
      alert(displayError(e));
    } finally {
      setFollowupBusy(false);
    }
  };
  const hasText = detail.segments.length > 0;

  return (
    <div className="mx-auto max-w-5xl px-8 py-6">
      {share && <ShareDialog meetingId={m.id} title={m.title} durationMs={m.duration_ms} hasAudio={hasAudio} hasVideo={!!detail.media.video} currentMs={currentMs} onClose={() => setShare(false)} />}
      <Link to="/" className="focus-ring inline-flex items-center gap-1.5 rounded text-[13px] text-ink-3 hover:text-ink">
        <ArrowLeft size={14} /> {tr("page.library")}
      </Link>

      <header className="mt-3 mb-6 flex items-start justify-between gap-6">
        <div className="min-w-0 flex-1">
          {editing ? (
            <form
              className="flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                void saveTitle();
              }}
            >
              <Input aria-label={tr("page.rename")} autoFocus value={title} onChange={(e) => setTitle(e.target.value)} className="h-10 max-w-lg font-sans font-semibold text-[22px]" />
              <Button variant="primary" size="sm" type="submit">
                <Check size={14} /> {tr("actions.save")}
              </Button>
            </form>
          ) : (
            <h1 className="group flex items-center gap-2 font-sans font-semibold text-[32px] leading-tight tracking-tight">
              <span className="truncate">{m.title}</span>
              <button className="focus-ring rounded text-ink-3 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 hover:text-ink" onClick={() => setEditing(true)} title={tr("actions.rename")} aria-label={tr("page.rename")}>
                <Pencil size={16} />
              </button>
            </h1>
          )}
          <div className="mt-1 flex flex-wrap items-center gap-3 text-[13px] text-ink-3">
            <span>{appLabel(m.app)}</span>
            <span>{tr("page.dateTime", { date: fmtDate(m.started_at), time: fmtTime(m.started_at) })}</span>
            <span className="font-mono">{fmtDuration(m.duration_ms)}</span>
            <Badge tone={m.status === "ready" ? "moss" : m.status === "failed" ? "ember" : "amber"}>{tr(`statuses.${m.status}`)}</Badge>
            {!serviceDesktop && <label className="inline-flex items-center gap-1">
              <FolderOpen size={13} />
              <select
                aria-label={tr("page.folder")}
                value={m.folder_id ?? ""}
                onChange={(e) => void cmd.moveMeeting(m.id, e.target.value || null).then(load)}
                className="focus-ring rounded border border-transparent bg-transparent text-[12px] hover:border-line-2"
              >
                <option value="">{tr("page.unfiled")}</option>
                {folders.map((f) => <option key={f.id} value={f.id}>{f.name}</option>)}
              </select>
            </label>}
          </div>
          {!serviceDesktop && <div className="mt-2 flex flex-wrap items-center gap-1.5">
            {tags.map((t) => (
              <span key={t.id} className="inline-flex items-center gap-1 rounded-full bg-canvas-3 px-2 py-0.5 text-[12px] text-ink-2">
                {t.name}
                <button onClick={() => void cmd.untagMeeting(m.id, t.id).then(() => cmd.meetingTags(m.id)).then(setTags)} aria-label={tr("page.removeTag", { name: t.name })} className="focus-ring rounded text-ink-3 hover:text-ember"><X size={11} /></button>
              </span>
            ))}
            <form
              className="inline-flex items-center gap-1"
              onSubmit={(e) => {
                e.preventDefault();
                const name = tagDraft.trim();
                if (!name) return;
                void cmd.tagMeeting(m.id, name).then(() => cmd.meetingTags(m.id)).then((t) => { setTags(t); setTagDraft(""); });
              }}
            >
              <TagIcon size={12} className="text-ink-3" />
              <input aria-label={tr("page.addTag")} value={tagDraft} onChange={(e) => setTagDraft(e.target.value)} placeholder={tr("page.addTag")} className="w-20 bg-transparent text-[12px] outline-none placeholder:text-ink-3" />
            </form>
          </div>}
          {m.participants.length > 0 && (
            <div className="mt-2 flex flex-wrap items-center gap-1.5 text-[12px] text-ink-3" title={tr("page.attendees")}>
              <Users size={12} />
              {m.participants.map((p) => (
                <span key={p} className="rounded-full border border-line px-2 py-0.5 text-ink-2">{p}</span>
              ))}
            </div>
          )}
          {stt && originalSelected && botTranscript === null && <p className="mt-2 text-[12px] text-ink-3">{tr("page.transcriptSource", { provider: stt.provider === "soniox" ? tr("page.soniox") : tr("page.whisper"), model: stt.model })}{stt.language ? ` · ${stt.language}` : ""}{stt.status === "incomplete" ? tr("page.incomplete") : stt.status === "streaming" ? tr("page.streaming") : ""}</p>}
          {shares.length >= 2 && (
            <div className="mt-3 max-w-lg" title={tr("page.talkTimeHint")}>
              <div className="flex h-1.5 w-full overflow-hidden rounded-full bg-line" role="img" aria-label={tr("page.talkTime", { shares: shares.map((s) => `${speakerLabel(s.name)} ${s.pct}%`).join(", ") })}>
                {shares.map((s, i) => (
                  <span key={s.name} className={`block h-full ${TALK_COLORS[i % TALK_COLORS.length]}`} style={{ width: `${s.pct}%` }} />
                ))}
              </div>
              <div className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-ink-3">
                {shares.map((s, i) => (
                  <span key={s.name} className="inline-flex items-center gap-1">
                    <span className={`inline-block h-2 w-2 rounded-full ${TALK_COLORS[i % TALK_COLORS.length]}`} />
                    {speakerLabel(s.name)} {s.pct}% <span className="font-mono">({fmtDuration(s.ms)})</span>
                  </span>
                ))}
              </div>
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {flash && <span className="mr-1 text-[12px] text-moss">{tr(flash.kind === "copied" ? "feedback.copied" : "feedback.saved", { item: textLabels[flash.item] })}</span>}
          {!serviceDesktop && <Button variant="outline" size="sm" onClick={() => setShare(true)} disabled={m.status === "recording"}>
            <Share2 size={14} /> {tr("page.share")}
          </Button>}
          <Menu
            label={<><Download size={14} /> {tr("page.export")}</>}
            disabled={!hasText}
            items={[
              { label: <><Copy size={13} /> {tr("page.copyTranscript")}</>, onSelect: () => void copyText(transcriptTxt(detail.segments, useClean), "transcript") },
              ...(!serviceDesktop ? [
                { label: <><Copy size={13} /> {tr("page.copySummary")}</>, onSelect: () => void copyText(summary?.markdown ?? "", "summary"), disabled: !summary },
                { label: tr("page.saveTxt"), onSelect: () => void saveText("txt", transcriptTxt(detail.segments, useClean), "transcript") },
                { label: tr("page.saveSrt"), onSelect: () => void saveText("srt", transcriptSrt(detail.segments, useClean), "subtitles") },
                { label: tr("page.saveMd"), onSelect: () => void saveText("md", transcriptMd(m.title, dateLine, detail.segments, useClean, summary?.markdown), "markdown") },
              ] : []),
            ]}
          />
          {!serviceDesktop && <Menu
            label={<><RefreshCw size={14} /> {tr("page.rerun")}</>}
            disabled={processing || correcting || m.status === "recording"}
            items={[
              { label: tr("page.onlySpeakers"), onSelect: () => void cmd.rediarize(m.id), disabled: !hasAudio || detail.segments.length === 0 },
              { label: tr("page.onlyCleanup"), onSelect: () => void cmd.rerunCleanup(m.id), disabled: detail.segments.length === 0 },
              { label: tr("page.onlyIndex"), onSelect: () => void cmd.reembed(m.id), disabled: detail.segments.length === 0 },
              { label: tr("page.summaryMenu"), onSelect: () => setTab("summary"), disabled: detail.segments.length === 0 },
            ]}
          />}
          <Button variant="danger" size="sm" disabled={serviceDesktop && (recording || processing)} onClick={() => void remove()}>
            <Trash2 size={14} /> {tr("actions.delete")}
          </Button>
        </div>
      </header>

      {m.error?.startsWith("no-llm:") && !processing && (
        <div className="mb-5 flex items-center gap-3 rounded-lg border border-line bg-canvas-2 px-4 py-2.5 text-[13px] text-ink-2">
          <Spinner className="h-3.5 w-3.5" /> <Trans t={tr} i18nKey="page.waitModel" components={{ models: <Link to="/settings" className="underline" /> }} />
        </div>
      )}
      {(m.status === "failed" || (m.error && !m.error.startsWith("no-llm:"))) && !processing && (
        <div className="mb-5 flex items-start gap-3 rounded-lg border border-ember/30 bg-ember-soft/40 px-4 py-3 text-[13px]">
          <AlertTriangle size={16} className="mt-0.5 shrink-0 text-ember" />
          <div className="min-w-0 flex-1">
            <div className="font-medium text-ink">{m.status === "failed" ? tr("page.processingFailed") : tr("page.stepFailed")}</div>
            <div className="mt-0.5 break-words text-ink-2">{m.error ? displayError(m.error) : tr("page.unknownError")}</div>
            <div className="mt-1 text-[12px] text-ink-3"><Trans t={tr} i18nKey="page.logsHint" components={{ logs: <code /> }} /></div>
          </div>
          <Button variant="outline" size="sm" disabled={!canGenerateVersion || correcting || versions.startedAt !== undefined} onClick={() => {
            setTab("transcript");
            setVersionSettings(true);
          }}>
            <RefreshCw size={14} /> {tr("versions.retryTranscription")}
          </Button>
        </div>
      )}
      {!processing && !m.error && m.status !== "failed" && snapshot?.finishedAt !== undefined && snapshot.error && (
        <p role="alert" className="mb-5 break-words rounded-lg border border-ember/30 bg-ember-soft/40 px-4 py-3 text-[13px] text-ember">
          {displayError(snapshot.error)}
        </p>
      )}
      {(processing || completed) && (
        <OperationProgress label={completed ? tr("stages.done") : stageText} announceStage
          className="mb-5 rounded-lg border border-line bg-canvas-2 px-4 py-2.5"
          value={completed ? 1 : stageProgress === null ? undefined : Math.min(0.99, stageProgress)}
          detail={activeTranscription?.model}
          startedAt={stage !== null || completed ? snapshot?.startedAt : summaryStartedAt ?? snapshot?.startedAt}
          finishedAt={completed ? snapshot.finishedAt : undefined} />
      )}

      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-8">
        <div className="sticky top-6 self-start">
          {recording ? (
            <div className="rounded-lg border border-line bg-canvas-2 p-5">
              <div className="inline-flex items-center gap-2 text-[13px] font-medium text-ember"><span className="h-2 w-2 animate-pulse rounded-full bg-ember" /> {tr("page.recording")}</div>
              <p className="mt-2 text-[13px] text-ink-2">{tr("page.recordingHint")}</p>
              <BotRecordingStatus meetingId={m.id} />
              {stopping ? (
                <div className="mt-4 inline-flex items-center gap-2 text-[13px] text-ink-2"><Spinner className="h-3.5 w-3.5" /> {tr("page.stopping")}</div>
              ) : (
                <Button variant="ember" size="sm" className="mt-4" onClick={() => { setStopping(true); void cmd.stopRecording().catch((e) => { setStopping(false); alert(displayError(e)); }); }}>{tr("page.stop")}</Button>
              )}
            </div>
          ) : hasMedia ? (
            <Player ref={player} audio={detail.media.audio} video={detail.media.video} highlights={detail.highlights} onTime={onTime} />
          ) : (
            <p className="text-sm text-ink-3">{tr("page.noMedia")}</p>
          )}
          {!recording && m.app === "meet" && <AttendeeTracks meetingId={m.id} />}
          {!recording && originalSelected && botTranscript === null && detail.speaker_stats.length > 0 && (
            <SpeakersPanel
              className="mt-6"
              stats={detail.speaker_stats}
              speakers={speakers}
              nameOptions={nameOptions}
              onRename={canEditSpeakerLabels ? renameSpeaker : undefined}
              onAcceptSuggestion={canEditSpeakerLabels ? acceptSuggestion : undefined}
              onDismissSuggestion={canEditSpeakerLabels ? dismissSuggestion : undefined}
            />
          )}
          {detail.highlights.length > 0 && (
            <div className="mt-6">
              <SectionTitle>{tr("page.highlights")}</SectionTitle>
              <div className="flex flex-wrap gap-2">
                {detail.highlights.map((h) => (
                  <button key={h} disabled={!hasMedia} onClick={() => player.current?.seek(Math.max(0, h - 5000))} className="focus-ring rounded-full bg-amber-soft px-2.5 py-1 font-mono text-[12px] text-ink hover:bg-amber/40 disabled:cursor-default disabled:hover:bg-amber-soft">
                    {fmtDuration(h)}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>

        <div className="flex min-h-[60vh] flex-col">
          <div className="mb-3 flex items-center justify-between">
            <div className="inline-flex rounded-md bg-canvas-3 p-0.5 text-[12px] font-medium">
              {(["transcript", "summary", "chat"] as const).map((t) => (
                <button key={t} onClick={() => setTab(t)} className={`rounded px-3 py-1 capitalize ${tab === t ? "bg-canvas text-ink shadow-card" : "text-ink-3 hover:text-ink"}`}>
                  {serviceDesktop && t === "chat" ? tr("subscription.chatTitle") : tr(`tabs.${t}`)}
                </button>
              ))}
            </div>
            {tab === "transcript" && recording && (
              <span className="inline-flex items-center gap-2 text-[12px] text-ember"><span className="h-2 w-2 animate-pulse rounded-full bg-ember" /> {tr("statuses.recording")}</span>
            )}
            {tab === "transcript" && !recording && (processing ? (
              <span className="inline-flex items-center gap-2 text-[12px] text-ink-3"><Spinner className="h-3 w-3" /> {stageText}</span>
            ) : botTranscript !== null ? null : originalSelected && hasClean ? (
              <span className="inline-flex rounded-md bg-canvas-3 p-0.5 text-[11px] font-medium">
                {(["clean", "raw"] as const).map((v) => (
                  <button key={v} onClick={() => setView(v)} aria-pressed={useClean === (v === "clean")} className={`rounded px-2 py-0.5 ${useClean === (v === "clean") ? "bg-canvas text-ink shadow-card" : "text-ink-3"}`}>
                    {v === "clean" ? tr("page.cleaned") : tr("page.raw")}
                  </button>
                ))}
              </span>
            ) : (
              <span className="text-[12px] text-ink-3">{tr("page.segments", { count: displayedSegments.length })}</span>
            ))}
          </div>

          {tab === "transcript" && recording && <LiveTranscript finals={live?.finals ?? lastLiveFinals.current} partials={stopping ? [] : live?.partials ?? []} />}
          {tab === "summary" && recording && <LiveNotes notes={live?.notes ?? ""} updatedMs={live?.notes_updated_ms ?? null} />}
          {tab === "chat" && recording && (
            <p className="text-sm text-ink-3">{tr("page.chatLater")}</p>
          )}

          {tab === "transcript" && !recording && <TranscriptVersionControls state={versions} botSources={availableBotTranscripts}
            selectedBot={botTranscript} onSelect={(choice) => void selectTranscript(choice)} models={asrModels} hasAudio={hasAudio}
            hasOriginalText={detail.segments.length > 0} disabled={processing || correcting} open={versionSettings} onOpen={setVersionSettings} />}
          {tab === "transcript" && botTranscript === null && participantVersion
            && <p className="mb-2 text-xs text-ink-3">{tr(`versions.${participantTimingHint}`)}</p>}
          {tab === "transcript" && botTranscript === null && displayedVersion?.partial
            && <p role="status" className="mb-2 rounded border border-amber p-2 text-xs text-amber">{tr("versions.partial")}</p>}

          {(m.status === "ready" || m.status === "failed" || availableBotTranscripts.length > 0) && !recording && (
            <BotTranscripts meetingId={m.id} visible={tab === "transcript"} selected={botTranscript} setSelected={setBotTranscript}
              onAvailableSources={setAvailableBotTranscripts}
              showLoadError={m.app === "meet" || m.app === "discord"}
              recordingDurationMs={m.duration_ms} onSeek={hasMedia ? seek : undefined} />
          )}

          {tab === "transcript" && !recording && botTranscript === null && (
            displayedSegments.length === 0 && processing ? (
              lastLiveFinals.current.length > 0 ? <LiveTranscript finals={lastLiveFinals.current} partials={[]} autoFollow={false} />
                : <p className="text-sm text-ink-3">{tr("page.transcribingHint")}</p>
            ) : displayedSegments.length === 0 ? (
              <p className="text-sm text-ink-3">
                {canGenerateVersion ? tr("page.noTranscript") : tr("transcript.empty")}
              </p>
            ) : (
              <Transcript
                key={versions.selected?.id ?? m.id}
                segments={displayedSegments}
                currentMs={currentMs}
                onSeek={displayedSeek}
                playbackTimings={hasMedia ? participantTimings : undefined}
                useClean={originalSelected && useClean}
                speakers={originalSelected ? speakers : undefined}
                nameOptions={originalSelected ? nameOptions : undefined}
                onRenameSpeaker={originalSelected && canEditSpeakerLabels ? renameSpeaker : undefined}
                onAcceptSuggestion={originalSelected && canEditSpeakerLabels ? acceptSuggestion : undefined}
                onDismissSuggestion={originalSelected && canEditSpeakerLabels ? dismissSuggestion : undefined}
                onSetCorrection={!serviceDesktop && originalSelected && m.status === "ready" && !processing ? setCorrection : undefined}
              />
            )
          )}

          {tab === "summary" && !recording && (serviceDesktop ? (
            <ServiceSummary key={`${m.id}:${processing}`} meetingId={m.id} segments={detail.segments} canGenerate={!processing} onSeek={hasMedia ? seek : undefined} />
          ) : (
            <div>
              <div className="mb-4 flex flex-wrap items-center gap-2">
                <SummaryControls meetingId={m.id} templates={templates} templateId={templateId || summary?.template_id || "general"} onTemplateChange={setTemplateId} disabled={detail.segments.length === 0} writing={stage === "summary" || summaryStartedAt !== undefined} hasSummary={!!summary} onStarted={() => setSummaryStartedAt((current) => current ?? performance.now())} onFinished={() => { setSummaryStartedAt(undefined); load(); }} onError={(e) => { setSummaryStartedAt(undefined); alert(e); }} />
                <Button variant="outline" size="sm" disabled={followupBusy || !hasText} onClick={() => void draftFollowup()} title={tr("page.emailTitle")}>
                  {followupBusy ? <Spinner className="h-3.5 w-3.5" /> : <Mail size={14} />} {tr("page.followup")}
                </Button>
                {summary && <span className="ml-auto text-[11px] text-ink-3">{summary.model}</span>}
              </div>
              {followup !== null && (
                <div className="mb-5 rounded-lg border border-line bg-canvas-2 p-3">
                  <div className="mb-2 flex items-center gap-2 text-[12px] text-ink-3">
                    <Mail size={13} /> {tr("page.emailHint")}
                    <div className="ml-auto flex items-center gap-1">
                      <Button variant="outline" size="sm" onClick={() => void copyText(followup, "email")}><Copy size={13} /> {tr("actions.copy")}</Button>
                      <Button variant="outline" size="sm" disabled={followupBusy} onClick={() => void draftFollowup()}><RefreshCw size={13} /> {tr("actions.redo")}</Button>
                      <Button variant="outline" size="sm" onClick={() => setFollowup(null)} aria-label={tr("page.closeDraft")}><X size={13} /></Button>
                    </div>
                  </div>
                  <textarea
                    value={followup}
                    onChange={(e) => setFollowup(e.target.value)}
                    rows={Math.min(18, Math.max(6, followup.split("\n").length + 1))}
                    className="focus-ring w-full resize-y rounded-md border border-line-2 bg-canvas p-3 font-sans text-[13px] leading-relaxed text-ink"
                    aria-label={tr("page.emailDraft")}
                  />
                </div>
              )}
              {summary ? (
                <Markdown text={summary.markdown} onSeek={hasMedia ? seek : undefined} />
              ) : (
                <p className="text-sm text-ink-3">{tr("page.noSummary")}</p>
              )}
            </div>
          ))}

          {tab === "chat" && !recording && (
            serviceDesktop ? (
              <div className="space-y-3">
                <p className="text-[13px] text-ink-3">{tr("subscription.serviceChat")}</p>
                <SubscriptionChat chatKey={`meeting/${m.id}`} transcript={{ segments: detail.segments, useClean }} />
              </div>
            ) : <ChatPanel scopeKind="meeting" scopeId={m.id} onSeek={hasMedia ? seek : undefined} transcript={{ segments: detail.segments, useClean }} className="min-h-[60vh] flex-1" />
          )}
        </div>
      </div>
    </div>
  );
}

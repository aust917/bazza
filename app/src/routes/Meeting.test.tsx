import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { Link, MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MeetingPage } from "./Meeting";
import type { TranscriptProps } from "@/components/Transcript";
import { ProcessingProgressProvider } from "@/lib/processingProgress";
import type { BotTranscript, BotTranscriptJob, Caption, Events, MeetingStatus, Segment, TranscriptVersion, TranscriptVersionJob, TranscriptVersionSegment, TranscriptVersions } from "@/lib/ipc";

type ProcessingEvent = Events["processing"];

const api = vi.hoisted(() => ({
  getMeeting: vi.fn(), getSummary: vi.fn(), getServiceSummary: vi.fn(), sttTranscript: vi.fn(), meetingTags: vi.fn(),
  getServiceSummaryJob: vi.fn(), prepareServiceSummary: vi.fn(), generateServiceSummary: vi.fn(), cancelServiceSummary: vi.fn(),
  listFolders: vi.fn(), listTemplates: vi.fn(), listModels: vi.fn(), listKnownSpeakers: vi.fn(),
  summaryJob: vi.fn(), subscriptionAgents: vi.fn(), getSubscriptionJob: vi.fn(), startSubscription: vi.fn(), cancelSubscription: vi.fn(), generateSummary: vi.fn(), cancelSummary: vi.fn(),
  setTranscriptCorrection: vi.fn(), retranscribe: vi.fn(),
  transcriptVersions: vi.fn(), transcriptVersion: vi.fn(), regenerateTranscript: vi.fn(),
  transcriptVersionJob: vi.fn(), cancelTranscriptVersion: vi.fn(),
  releaseMeetingMedia: vi.fn(), attendeeTracks: vi.fn(), attendeeOpenTrack: vi.fn(),
  botTranscriptSources: vi.fn(), botTranscriptJobs: vi.fn(), cancelBotTranscript: vi.fn(), transcribeBotTracks: vi.fn(),
  attendeeRecordingStatus: vi.fn(), discordRecordingStatus: vi.fn(), recordingStatus: vi.fn(),
  seekRecording: vi.fn(),
  botListeners: new Map<string, (payload: unknown) => void>(),
  versionListeners: new Map<string, (payload: unknown) => void>(),
  live: null as { finals: Caption[]; partials: Caption[]; notes: string; notes_updated_ms: number | null } | null,
  processingListener: undefined as ((payload: ProcessingEvent) => void) | undefined,
}));
vi.mock("@/lib/ipc", () => ({
  cmd: api,
  on: async (event: string, callback: (payload: unknown) => void) => {
    api.botListeners.set(event, callback);
    return () => { api.botListeners.delete(event); };
  },
  subscribe: (event: string, callback: (payload: ProcessingEvent) => void) => {
    if (event === "processing") api.processingListener = callback;
    else api.versionListeners.set(event, callback as (payload: unknown) => void);
    return () => {
      if (event === "processing" && api.processingListener === callback) api.processingListener = undefined;
      else api.versionListeners.delete(event);
    };
  },
}));
vi.mock("@/components/Player", async () => {
  const { forwardRef, useImperativeHandle } = await import("react");
  return { Player: forwardRef<{ seek: (ms: number) => void }>(function FakePlayer(_, ref) {
    useImperativeHandle(ref, () => ({ seek: api.seekRecording }));
    return <div>Media player</div>;
  }) };
});
vi.mock("@/components/Transcript", () => ({ Transcript: ({ segments, useClean, onSeek, playbackTimings, onSetCorrection }: TranscriptProps) => <div data-testid="transcript-test" data-seekable={Boolean(onSeek)}>
  Saved transcript
  <p>{useClean ? (segments[0]?.clean_text ?? segments[0]?.text) : segments[0]?.text}</p>
  <span>{segments[0]?.speaker}</span>
  {onSeek && playbackTimings && [...playbackTimings].map(([id, timing]) => <button key={id} onClick={() => onSeek(timing.start_ms)}>Test seek for line {id}</button>)}
  {onSetCorrection && <button onClick={() => void Promise.resolve(onSetCorrection(segments[0].id, "Edited control text")).catch(() => {})}>Test save correction</button>}
</div>, SPEAKER_COLORS: ["text-ink"] }));
vi.mock("@/components/LiveView", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/components/LiveView")>();
  return { ...actual, useLiveFeed: (meetingId: string | null) => meetingId ? api.live : null };
});

const detail = (id: string) => ({
  meeting: { id, title: id === "first" ? "First meeting" : "Second meeting", app: null,
    started_at: "2026-11-01T00:00:00Z", ended_at: "2026-11-01T00:00:02Z", duration_ms: 2000,
    has_video: false, status: "ready", folder_id: null, created_at: "2026-11-01T00:00:00Z",
    error: null, title_auto: false, participants: [], calendar_uid: null },
  segments: [{ id: 1, meeting_id: id, start_ms: 0, end_ms: 1000, text: "Control text", clean_text: null, speaker: null }],
  media: { audio: "test.wav", video: null }, highlights: [], speakers: [], speaker_stats: [],
});

beforeEach(() => {
  vi.resetAllMocks();
  api.processingListener = undefined;
  api.botListeners.clear();
  api.versionListeners.clear();
  api.live = null;
  HTMLElement.prototype.scrollIntoView ??= vi.fn();
  api.getMeeting.mockImplementation(async (id: string) => detail(id));
  api.getSummary.mockImplementation(async (id: string) => id === "first" ? {
    meeting_id: id, template_id: "general", markdown: "First meeting's existing summary", structured: null,
    model: "Codex CLI", created_at: "2026-11-01T00:00:00Z",
  } : null);
  api.sttTranscript.mockResolvedValue(null);
  api.meetingTags.mockResolvedValue([]);
  api.listFolders.mockResolvedValue([]);
  api.listTemplates.mockResolvedValue([{ id: "general", name: "General", description: "", body: "{{transcript}}", builtin: true }]);
  api.listModels.mockResolvedValue([]);
  api.listKnownSpeakers.mockResolvedValue([]);
  api.summaryJob.mockImplementation(async (id: string) => ({ running: id === "first", agent: id === "first" ? "codex" : null }));
  api.subscriptionAgents.mockResolvedValue([{ agent: "codex", status: "ready" }, { agent: "claude", status: "missing" }]);
  api.getSubscriptionJob.mockResolvedValue(null);
  api.getServiceSummary.mockResolvedValue({ markdown: "Saved service summary" });
  api.getServiceSummaryJob.mockResolvedValue(null);
  api.retranscribe.mockResolvedValue(undefined);
  api.transcriptVersions.mockResolvedValue({ versions: [], initial_id: null, participant_source: null });
  api.transcriptVersionJob.mockResolvedValue(null);
  api.releaseMeetingMedia.mockResolvedValue(undefined);
  api.attendeeTracks.mockResolvedValue([]);
  api.botTranscriptSources.mockResolvedValue([]);
  api.botTranscriptJobs.mockResolvedValue([]);
  api.cancelBotTranscript.mockResolvedValue(true);
  api.attendeeRecordingStatus.mockResolvedValue(null);
  api.discordRecordingStatus.mockResolvedValue(null);
  api.recordingStatus.mockResolvedValue({ state: "idle", meeting_id: null, elapsed_ms: 0 });
});

it("restores native participant progress and cancels that exact job", async () => {
  const job: BotTranscriptJob = {
    id: "job-1", source: "discord", stage: "transcribing", elapsed_ms: 12000,
    completed: 14, total: 40, model: "whisper-large-v3-turbo", backend: "cuda", backend_reason: null, can_cancel: true, error: null,
  };
  api.botTranscriptSources.mockResolvedValue([{ source: "discord", track_count: 16, transcript: null }]);
  api.transcriptVersions.mockResolvedValue({ versions: [], initial_id: null, participant_source: "discord" });
  api.botTranscriptJobs.mockResolvedValue([job]);
  api.cancelBotTranscript.mockImplementation(async () => {
    api.botTranscriptJobs.mockResolvedValue([{ ...job, stage: "cancelled", can_cancel: false }]);
    return true;
  });
  renderPage();

  const bar = await screen.findByRole("progressbar", { name: "Transcribing participant tracks" });
  expect(bar).toHaveAttribute("aria-valuenow", "35");
  expect(screen.getByText("whisper-large-v3-turbo · CUDA")).toBeVisible();
  expect(screen.getByText("Control text")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Transcribe tracks from Discord" })).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(api.cancelBotTranscript).toHaveBeenCalledExactlyOnceWith("first", "discord", "job-1");
  expect(await screen.findByText("Transcription cancelled", {}, { timeout: 2000 })).toBeVisible();
  expect(screen.getByRole("button", { name: "New transcript version" })).toBeEnabled();
});

it("shows ordinary transcription as indeterminate until Whisper reports progress", async () => {
  renderPage();
  await screen.findByText("Control text");
  const base = { meeting_id: "first", stage: "transcribe", progress: 0.1, error: null };
  act(() => api.processingListener?.({ ...base, transcription: { phase: "reading", progress: null, model: null } }));
  expect(screen.getByRole("progressbar", { name: "Preparing audio" })).not.toHaveAttribute("aria-valuenow");
  act(() => api.processingListener?.({ ...base, transcription: { phase: "decoding", progress: 0.37, model: "whisper-small" } }));
  expect(screen.getByRole("progressbar", { name: "Transcribing" })).toHaveAttribute("aria-valuenow", "37");
});

it("retains the current phase and elapsed time across routes and waits for ready data before 100%", async () => {
  vi.useFakeTimers();
  try {
    const meeting = detail("first");
    api.getMeeting.mockResolvedValue({ ...meeting, meeting: { ...meeting.meeting, status: "processing" } });
    await act(async () => {
      render(<StrictMode><ProcessingProgressProvider>
        <MemoryRouter initialEntries={["/meeting/first"]}>
          <nav><Link to="/">Outside meeting</Link><Link to="/meeting/first">Open first meeting</Link></nav>
          <Routes><Route path="/meeting/:id" element={<MeetingPage />} /><Route path="/" element={<p>Library fixture</p>} /></Routes>
        </MemoryRouter>
      </ProcessingProgressProvider></StrictMode>);
    });
    const event = { meeting_id: "first", stage: "transcribe", progress: 0.1, error: null };
    act(() => api.processingListener?.({ ...event, transcription: { phase: "reading", progress: null, model: null } }));
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    fireEvent.click(screen.getByRole("link", { name: "Outside meeting" }));
    expect(screen.getByText("Library fixture")).toBeVisible();
    act(() => api.processingListener?.({ ...event, transcription: { phase: "decoding", progress: 0.64, model: null } }));
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    await act(async () => { fireEvent.click(screen.getByRole("link", { name: "Open first meeting" })); });
    expect(screen.getByRole("progressbar", { name: "Transcribing" })).toHaveAttribute("aria-valuenow", "64");
    expect(screen.getByLabelText("Elapsed 0:03")).toBeVisible();

    let finish!: (result: ReturnType<typeof detail>) => void;
    api.getMeeting.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    act(() => api.processingListener?.({ ...event, stage: "done", progress: 1 }));
    expect(screen.queryByRole("progressbar", { name: "Processing complete" })).not.toBeInTheDocument();
    await act(async () => { finish(meeting); });
    expect(screen.getByRole("progressbar", { name: "Processing complete" })).toHaveAttribute("aria-valuenow", "100");
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(screen.getByLabelText("Elapsed 0:03")).toBeVisible();
  } finally {
    vi.useRealTimers();
  }
});

it("shows a failed step without marking the operation complete", async () => {
  renderPage();
  await screen.findByText("Control text");
  await act(async () => api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: "Cleanup failed" }));
  expect(screen.getByRole("alert")).toHaveTextContent("Cleanup failed");
  expect(screen.queryByRole("progressbar", { name: "Processing complete" })).not.toBeInTheDocument();
});

it("names the automatic participant and additional-speech stage", async () => {
  renderPage();
  await screen.findByText("Control text");
  await act(async () => api.processingListener?.({ meeting_id: "first", stage: "supplement", progress: 0.1, error: null }));
  expect(screen.getByRole("progressbar", { name: "Participants and additional speech" })).toBeVisible();
});

it("explains that finalized live captions are saved first", async () => {
  const value = detail("first");
  api.getMeeting.mockResolvedValue({ ...value, meeting: { ...value.meeting, status: "recording" }, segments: [] });
  renderPage();
  expect(await screen.findByText(/Finalized live captions become the initial transcript/)).toBeVisible();
  expect(api.retranscribe).not.toHaveBeenCalled();
});

it("explains the empty main transcript and keeps participant-track transcription available", async () => {
  const value = detail("first");
  api.getMeeting.mockResolvedValue({ ...value, meeting: { ...value.meeting, app: "meet" }, media: { audio: null, video: null }, segments: [] });
  api.botTranscriptSources.mockResolvedValue([{ source: "meet", track_count: 2, transcript: null }]);
  api.transcriptVersions.mockResolvedValue({ versions: [], initial_id: null, participant_source: "meet" });
  renderPage();
  expect(await screen.findByText(/No transcript was saved\. Choose New transcript version/)).toBeVisible();
  expect(screen.queryByRole("button", { name: "Transcribe tracks from Google Meet" })).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "New transcript version" }));
  expect(screen.getByRole("combobox", { name: "Audio source" })).toHaveValue("participants");
  expect(screen.getByRole("option", { name: "Recording audio" })).toBeDisabled();
  expect(api.retranscribe).not.toHaveBeenCalled();
  expect(api.transcribeBotTracks).not.toHaveBeenCalled();
  expect(api.regenerateTranscript).not.toHaveBeenCalled();
});
afterEach(() => { vi.restoreAllMocks(); });

function renderPage() {
  return render(<MemoryRouter initialEntries={["/meeting/first"]}><Routes><Route path="/meeting/:id" element={<MeetingPage />} /></Routes></MemoryRouter>);
}

it.each([true, false])("offers speaker merging for diarized labels only (platform labels: %s)", async (platformLabels) => {
  const value = detail("first");
  const labels = platformLabels ? ["Alice", "Bob"] : ["Speaker 1", "Speaker 2"];
  api.getMeeting.mockResolvedValue({
    ...value,
    meeting: { ...value.meeting, app: "discord" },
    speaker_stats: labels.map((label, index) => ({ label, segments: 1, ms: 1000, first_ms: index * 1000 })),
  });
  api.sttTranscript.mockResolvedValue({
    provider: "local", model: "whisper-small", language: null, status: "complete", error: null,
    captions: ["First line", "Second line"].map((text, index) => ({
      start_ms: index * 1000, end_ms: (index + 1) * 1000, is_final: true, text,
    })),
    live_speaker_labels: ["Alice", "Bob"],
  });
  renderPage();

  await screen.findByText(/whisper-small/);
  if (platformLabels) {
    expect(screen.getByText(labels[0])).toBeVisible();
    expect(screen.queryByLabelText(/Merge .* into another speaker/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Split one person into two/)).not.toBeInTheDocument();
  } else {
    expect(screen.getByLabelText("Merge Speaker 1 into another speaker")).toBeVisible();
    expect(screen.getByText(/Split one person into two/)).toBeVisible();
  }
});

it("reads the managed summary and keeps the subscription question isolated from meeting data", async () => {
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Routes><Route path="/meeting/:id" element={<MeetingPage serviceDesktop />} /></Routes>
  </MemoryRouter>);
  fireEvent.click(await screen.findByRole("button", { name: "Summary" }));
  expect(await screen.findByText("Saved service summary")).toBeVisible();
  expect(api.getServiceSummary).toHaveBeenCalledExactlyOnceWith("first");
  expect(api.getSummary).not.toHaveBeenCalled();
  expect(screen.queryByRole("button", { name: "Regenerate" })).not.toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Ask Codex or Claude Code" }));
  expect(await screen.findByRole("option", { name: "Codex · signed in" })).toBeEnabled();
  expect(screen.getByLabelText("Context to share (optional)")).toHaveValue("");
  expect(api.getSubscriptionJob).toHaveBeenCalledWith("meeting/first");
  expect(api.startSubscription).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "Transcript" }));
  api.getSubscriptionJob.mockResolvedValue({
    request_id: "subscription-1", agent: "codex", status: "succeeded", error: null,
    reply: { markdown: "Saved in the current app session", model: "Codex CLI" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Ask Codex or Claude Code" }));
  expect(await screen.findByText("Saved in the current app session")).toBeVisible();
});

it("requires a managed summary preview and reloads the saved result after generating", async () => {
  const prompt = "Instructions and selected Control text";
  const prepared = { preview_id: "preview-1", agent: "codex", status: "prepared", prompt, error: null };
  api.prepareServiceSummary.mockImplementation(async () => {
    api.getServiceSummaryJob.mockResolvedValue(prepared);
    return { preview_id: prepared.preview_id, prompt };
  });
  api.generateServiceSummary.mockImplementation(async () => {
    const completed = { ...prepared, status: "succeeded", prompt: null };
    api.getServiceSummaryJob.mockResolvedValue(completed);
    api.getServiceSummary.mockResolvedValue({ markdown: "Persisted managed summary" });
    return { ...completed, status: "running" };
  });
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Routes><Route path="/meeting/:id" element={<MeetingPage serviceDesktop />} /></Routes>
  </MemoryRouter>);
  fireEvent.click(await screen.findByRole("button", { name: "Summary" }));
  expect(await screen.findByText("Saved service summary")).toBeVisible();
  const preview = screen.getByRole("button", { name: "Review what will be sent" });
  expect(preview).toBeDisabled();
  fireEvent.change(screen.getByLabelText("From"), { target: { value: "0:00" } });
  fireEvent.change(screen.getByLabelText("To"), { target: { value: "0:01" } });
  expect(preview).toBeDisabled();
  await screen.findByRole("option", { name: "Codex · signed in" });
  fireEvent.change(screen.getByLabelText("Summary provider"), { target: { value: "codex" } });
  await waitFor(() => expect(preview).toBeEnabled());
  fireEvent.click(preview);
  expect(api.prepareServiceSummary).toHaveBeenCalledExactlyOnceWith("first", "codex", [1]);
  expect(await screen.findByLabelText("Text to send")).toHaveValue(prompt);
  expect(screen.getByText(/Generate sends the text below to OpenAI through Codex/)).toBeVisible();
  expect(api.generateServiceSummary).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "Transcript" }));
  fireEvent.click(screen.getByRole("button", { name: "Summary" }));
  expect(await screen.findByLabelText("Text to send")).toHaveValue(prompt);
  const generate = screen.getByRole("button", { name: "Regenerate" });
  await waitFor(() => expect(generate).toBeEnabled());
  fireEvent.click(generate);
  expect(api.generateServiceSummary).toHaveBeenCalledExactlyOnceWith("preview-1");
  expect(await screen.findByText("Persisted managed summary")).toBeVisible();
  expect(screen.getByRole("progressbar", { name: "Summary saved" })).toHaveAttribute("aria-valuenow", "100");
  expect(api.generateSummary).not.toHaveBeenCalled();
});

it("keeps the managed summary available while a subscription request runs and when sign-in fails", async () => {
  api.subscriptionAgents.mockResolvedValue([{ agent: "codex", status: "ready" }, { agent: "claude", status: "sign_in_required" }]);
  const preview = { preview_id: "preview-1", agent: "codex", status: "prepared", prompt: "Instructions and excerpt", error: null };
  const running = { ...preview, status: "running", prompt: null };
  api.getServiceSummaryJob.mockResolvedValue(preview);
  api.generateServiceSummary.mockImplementation(async () => {
    api.getServiceSummaryJob.mockResolvedValue(running);
    return running;
  });
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Routes><Route path="/meeting/:id" element={<MeetingPage serviceDesktop />} /></Routes>
  </MemoryRouter>);

  fireEvent.click(await screen.findByRole("button", { name: "Summary" }));
  expect(await screen.findByRole("option", { name: "Claude Code · sign in to the CLI" })).toBeDisabled();
  expect(await screen.findByText("Saved service summary")).toBeVisible();
  const generate = screen.getByRole("button", { name: "Regenerate" });
  await waitFor(() => expect(generate).toBeEnabled());
  fireEvent.click(generate);
  const progress = await screen.findByRole("progressbar", { name: "Waiting for a reply from OpenAI through Codex…" });
  expect(progress).not.toHaveAttribute("aria-valuenow");
  expect(screen.getByText("Saved service summary")).toBeVisible();

  api.getServiceSummaryJob.mockResolvedValue({ ...running, status: "failed", error: "Sign in to Codex and try again." });
  expect(await screen.findByRole("alert")).toHaveTextContent("Sign in to Codex and try again.");
  expect(screen.getByText("Saved service summary")).toBeVisible();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
});

it("opens the named Google Meet WAV using the meeting and track IDs", async () => {
  api.getMeeting.mockImplementation(async (id: string) => {
    const base = detail(id);
    return { ...base, meeting: { ...base.meeting, app: "meet" } };
  });
  api.attendeeTracks.mockResolvedValue([{ id: 2, name: "Алиса" }, { id: 4, name: null }]);
  api.attendeeOpenTrack.mockResolvedValue(undefined);
  renderPage();

  fireEvent.click(await screen.findByRole("button", { name: "Open WAV for Алиса" }));
  expect(api.attendeeTracks).toHaveBeenCalledExactlyOnceWith("first");
  expect(api.attendeeOpenTrack).toHaveBeenCalledExactlyOnceWith("first", 2);
  fireEvent.click(screen.getByRole("button", { name: "Open WAV for Participant 4" }));
  expect(api.attendeeOpenTrack).toHaveBeenLastCalledWith("first", 4);
});

it("offers one version-creation path for Discord tracks without launching the historical job", async () => {
  const version: TranscriptVersion = {
    id: "discord-version", source: "discord", model: "whisper-small", language: "en", created_at: "2026-11-01T00:03:00Z",
  };
  api.botTranscriptSources.mockResolvedValue([{ source: "discord", track_count: 1, transcript: null }]);
  api.transcriptVersions.mockResolvedValue({ versions: [], initial_id: null, participant_source: "discord" });
  api.regenerateTranscript.mockImplementation(async () => {
    api.transcriptVersions.mockResolvedValue({ versions: [version], initial_id: null, participant_source: "discord" });
    return version;
  });
  const original = detail("first").segments[0];
  api.transcriptVersion.mockResolvedValue([
    { ...original, id: -1, speaker: "Anna", text: "From the bot", playback: { start_ms: 700, end_ms: 1700 } },
    { ...original, id: -2 },
    { ...original, id: -3, playback: { start_ms: 700, end_ms: 2100 } },
  ]);
  renderPage();

  await screen.findByRole("heading", { name: "Discord participant tracks saved" });
  expect(screen.getByText("Control text")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Transcribe tracks from Discord" })).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "New transcript version" }));
  expect(screen.getByRole("combobox", { name: "Audio source" })).toHaveValue("participants");
  fireEvent.click(screen.getByRole("button", { name: "Generate version" }));
  expect(api.regenerateTranscript).toHaveBeenCalledExactlyOnceWith("first", { source: "participants" });
  expect(api.transcribeBotTracks).not.toHaveBeenCalled();

  expect(await screen.findByText("From the bot")).toBeVisible();
  const picker = screen.getByRole("combobox", { name: "Transcript version" });
  expect(picker).toHaveValue("version:discord-version");
  expect(screen.getByText("Anna")).toBeVisible();
  expect(screen.getAllByRole("button", { name: /Test seek for line/ })).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Test seek for line -1" }));
  expect(api.seekRecording).toHaveBeenCalledExactlyOnceWith(700);
  fireEvent.change(picker, { target: { value: "version:original" } });
  expect(await screen.findByText("Control text")).toBeVisible();
  expect(screen.queryByText("From the bot")).not.toBeInTheDocument();
});

it.each([true, false])("seeks only on verified recording timestamps when media is available (%s)", async (hasMedia) => {
  api.getMeeting.mockImplementation(async (id: string) => {
    const base = detail(id);
    return {
      ...base, meeting: { ...base.meeting, duration_ms: 6000, has_video: hasMedia },
      media: { audio: null, video: hasMedia ? "test.mp4" : null },
    };
  });
  const transcript: BotTranscript = {
    timing: "approximate", partial: true,
    segments: [
      { speaker_id: "1", speaker_name: "Anna", start_ms: 200, end_ms: 600, text: "Aligned", playback_start_ms: 2800, playback_end_ms: 3200 },
      { speaker_id: "1", speaker_name: "Anna", start_ms: 1100, end_ms: 1500, text: "Bot time only" },
      { speaker_id: "1", speaker_name: "Anna", start_ms: 2500, end_ms: 2700, text: "Outside the recording", playback_start_ms: 7000 },
    ],
  };
  api.botTranscriptSources.mockResolvedValue([{ source: "meet", track_count: 1, transcript }]);
  renderPage();
  await screen.findByRole("option", { name: "By participant · Google Meet" });
  fireEvent.change(screen.getByRole("combobox", { name: "Transcript version" }), { target: { value: "bot:meet" } });
  expect(await screen.findByText(/Some speech may be missing/)).toBeVisible();
  const seekButtons = screen.queryAllByRole("button", { name: /Play recording at/ });
  if (hasMedia) {
    expect(seekButtons).toHaveLength(1);
    expect(seekButtons[0]).toHaveAccessibleName("Play recording at 0:02");
    expect(screen.getByText(/Other times are relative to the bot tracks/)).toBeVisible();
    fireEvent.click(seekButtons[0]);
    expect(api.seekRecording).toHaveBeenCalledExactlyOnceWith(2800);
  } else {
    expect(seekButtons).toHaveLength(0);
    expect(screen.getByText(/Approximate times are relative to the bot tracks/)).toBeVisible();
    expect(api.seekRecording).not.toHaveBeenCalled();
  }
  expect(screen.getByText("0:01", { selector: "time" })).toBeVisible();
  expect(screen.getByText("Outside the recording")).toBeVisible();
});

it("opens a saved historical participant transcript after revisiting its meeting", async () => {
  let historical: BotTranscript | null = null;
  api.botTranscriptSources.mockImplementation(async (id: string) => id === "first"
    ? [{ source: "meet", track_count: 1, transcript: historical }]
    : []);
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Link to="/meeting/first">Open first meeting</Link><Link to="/meeting/second">Open second meeting</Link>
    <Routes><Route path="/meeting/:id" element={<MeetingPage />} /></Routes>
  </MemoryRouter>);

  await screen.findByRole("heading", { name: "Google Meet participant tracks saved" });
  fireEvent.click(screen.getByRole("link", { name: "Open second meeting" }));
  await screen.findByRole("heading", { name: "Second meeting" });
  expect(screen.queryByRole("option", { name: "By participant · Google Meet" })).not.toBeInTheDocument();
  historical = { timing: "approximate", partial: false, segments: [] };
  fireEvent.click(screen.getByRole("link", { name: "Open first meeting" }));
  await screen.findByRole("option", { name: "By participant · Google Meet" });
  expect(api.transcribeBotTracks).not.toHaveBeenCalled();
  expect(screen.getByText("Control text")).toBeVisible();
  fireEvent.change(screen.getByRole("combobox", { name: "Transcript version" }), { target: { value: "bot:meet" } });
  expect(await screen.findByText("No speech could be transcribed from these tracks.")).toBeVisible();
});

it("shows the active meeting's bot failure separately from the computer recording", async () => {
  api.getMeeting.mockImplementation(async (id: string) => {
    const base = detail(id);
    return { ...base, meeting: { ...base.meeting, status: "recording" } };
  });
  api.discordRecordingStatus.mockResolvedValue({ meeting_id: "first", state: "connected" });
  const { unmount } = renderPage();

  expect(await screen.findByRole("status")).toHaveTextContent("waiting for sound");
  act(() => api.botListeners.get("discord_recording_status")?.({ meeting_id: "another", state: "unavailable" }));
  expect(screen.getByRole("status")).toHaveTextContent("waiting for sound");
  act(() => api.botListeners.get("discord_recording_status")?.({ meeting_id: "first", state: "unavailable" }));
  expect(screen.getByRole("status")).toHaveTextContent("Discord tracks are unavailable");
  expect(screen.getByText("Recording in progress")).toBeVisible();
  unmount();
  expect(api.botListeners.size).toBe(0);
});

it("recovers a legacy meeting when stop and processing completion events are missed", async () => {
  vi.useFakeTimers();
  try {
    let status: MeetingStatus = "recording";
    api.getMeeting.mockImplementation(async (id: string) => {
      const base = detail(id);
      return { ...base, meeting: { ...base.meeting, status } };
    });
    api.recordingStatus.mockResolvedValue({ state: "recording", meeting_id: "first", elapsed_ms: 2_000 });

    await act(async () => { renderPage(); });
    expect(screen.getByText("Recording in progress")).toBeVisible();

    await act(async () => { await vi.advanceTimersByTimeAsync(3_000); });
    expect(api.getMeeting).toHaveBeenCalledExactlyOnceWith("first");

    api.recordingStatus.mockResolvedValue({ state: "idle", meeting_id: null, elapsed_ms: 0 });
    await act(async () => { await vi.advanceTimersByTimeAsync(3_000); });
    expect(screen.getByText("Finishing the recording...")).toBeVisible();

    status = "processing";
    api.recordingStatus.mockResolvedValue({ state: "recording", meeting_id: "second", elapsed_ms: 0 });
    await act(async () => { await vi.advanceTimersByTimeAsync(3_000); });
    expect(screen.queryByText("Recording in progress")).not.toBeInTheDocument();
    act(() => api.processingListener?.({ meeting_id: "first", stage: "mux", progress: 0.1, error: null }));
    expect(screen.getAllByText("Finalising video")[0]).toBeVisible();

    status = "ready";
    await act(async () => { await vi.advanceTimersByTimeAsync(3_000); });
    expect(screen.getByText("Control text")).toBeVisible();
    expect(screen.queryAllByText("Finalising video")).toHaveLength(0);
    expect(api.getMeeting).toHaveBeenCalledTimes(4);
    expect(api.recordingStatus).toHaveBeenCalledTimes(3);

    await act(async () => { await vi.advanceTimersByTimeAsync(6_000); });
    expect(api.getMeeting).toHaveBeenCalledTimes(4);
    expect(api.recordingStatus).toHaveBeenCalledTimes(3);
  } finally {
    vi.useRealTimers();
  }
});

it("uses the ordinary detail in service mode and releases its media lease on navigation", async () => {
  api.getMeeting.mockImplementation(async (id: string) => ({
    ...detail(id),
    media: { audio: `bazza-media:${id}-audio`, video: null, lease: id },
  }));
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Link to="/meeting/second">Open second meeting</Link>
    <Routes><Route path="/meeting/:id" element={<MeetingPage serviceDesktop />} /></Routes>
  </MemoryRouter>);
  await screen.findByRole("heading", { name: "First meeting" });
  expect(screen.getByText("Media player")).toBeInTheDocument();
  expect(screen.getByText("Control text")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Test save correction" })).not.toBeInTheDocument();
  expect(api.getSummary).not.toHaveBeenCalled();
  expect(api.meetingTags).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("link", { name: "Open second meeting" }));
  await screen.findByRole("heading", { name: "Second meeting" });
  expect(api.releaseMeetingMedia).toHaveBeenCalledExactlyOnceWith("first");
});

it("keeps another meeting's active summary job and saved summary out of the new route", async () => {
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Link to="/meeting/second">Open second meeting</Link>
    <Routes><Route path="/meeting/:id" element={<MeetingPage />} /></Routes>
  </MemoryRouter>);
  await screen.findByRole("heading", { name: "First meeting" });
  fireEvent.click(screen.getByRole("button", { name: /^summary$/i }));
  await screen.findByRole("button", { name: "Cancel summary" });
  expect(screen.getByRole("button", { name: "Regenerate" })).toBeDisabled();
  expect(screen.getByText("First meeting's existing summary")).toBeInTheDocument();

  fireEvent.click(screen.getByRole("link", { name: "Open second meeting" }));
  await screen.findByRole("heading", { name: "Second meeting" });
  fireEvent.click(screen.getByRole("button", { name: /^summary$/i }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Generate" })).toBeEnabled());
  expect(screen.queryByRole("button", { name: "Cancel summary" })).not.toBeInTheDocument();
  expect(screen.queryByText("First meeting's existing summary")).not.toBeInTheDocument();
  expect(screen.getByText("No summary yet. Pick a template and generate one.")).toBeInTheDocument();
  expect(api.generateSummary).not.toHaveBeenCalled();
});

it("switches to corrected text after native save and keeps Raw available without generating a summary", async () => {
  api.setTranscriptCorrection.mockImplementation(async (id: string, segmentId: number, text: string) => ({ ...detail(id).segments[0], id: segmentId, clean_text: text, clean_text_manual: true }));
  renderPage();
  await screen.findByRole("heading", { name: "First meeting" });
  fireEvent.click(screen.getByRole("button", { name: "Test save correction" }));
  await screen.findByText("Edited control text");
  expect(api.setTranscriptCorrection).toHaveBeenCalledExactlyOnceWith("first", 1, "Edited control text");
  fireEvent.click(screen.getByRole("button", { name: "Raw" }));
  expect(screen.getByText("Control text")).toBeInTheDocument();
  expect(api.generateSummary).not.toHaveBeenCalled();
});

it("ignores an old meeting's correction response after navigating to another meeting", async () => {
  let finish!: (segment: Segment) => void;
  api.setTranscriptCorrection.mockImplementation(() => new Promise<Segment>((resolve) => { finish = resolve; }));
  render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Link to="/meeting/second">Open second meeting</Link>
    <Routes><Route path="/meeting/:id" element={<MeetingPage />} /></Routes>
  </MemoryRouter>);
  await screen.findByRole("heading", { name: "First meeting" });
  fireEvent.click(screen.getByRole("button", { name: "Test save correction" }));
  fireEvent.click(screen.getByRole("link", { name: "Open second meeting" }));
  await screen.findByRole("heading", { name: "Second meeting" });
  await act(async () => { finish({ ...detail("first").segments[0], clean_text: "Old meeting response", clean_text_manual: true }); });
  expect(screen.queryByText("Old meeting response")).not.toBeInTheDocument();
  expect(screen.getByText("Control text")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Raw" })).not.toBeInTheDocument();
});

it("preserves a newer speaker name and blocks re-transcription while a correction is pending", async () => {
  let finish!: (segment: Segment) => void;
  api.setTranscriptCorrection.mockImplementation(() => new Promise<Segment>((resolve) => { finish = resolve; }));
  renderPage();
  await screen.findByRole("heading", { name: "First meeting" });
  fireEvent.click(screen.getByRole("button", { name: "Test save correction" }));
  expect(screen.getByRole("button", { name: "Re-run" })).toBeDisabled();
  api.getMeeting.mockResolvedValueOnce({ ...detail("first"), segments: [{ ...detail("first").segments[0], speaker: "Recently renamed speaker" }] });
  await act(async () => { api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: null }); });
  await screen.findByText("Recently renamed speaker");
  await act(async () => { finish({ ...detail("first").segments[0], clean_text: "Edited control text", clean_text_manual: true }); });
  await screen.findByText("Edited control text");
  expect(screen.getByText("Recently renamed speaker")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Re-run" })).toBeEnabled();
  expect(api.retranscribe).not.toHaveBeenCalled();
});

it("does not let a read started during save revert the accepted correction", async () => {
  let finish!: (segment: Segment) => void, finishRead!: (value: ReturnType<typeof detail>) => void;
  api.setTranscriptCorrection.mockImplementation(() => new Promise<Segment>((resolve) => { finish = resolve; }));
  renderPage();
  await screen.findByRole("heading", { name: "First meeting" });
  fireEvent.click(screen.getByRole("button", { name: "Test save correction" }));
  api.getMeeting.mockImplementationOnce(() => new Promise<ReturnType<typeof detail>>((resolve) => { finishRead = resolve; }));
  act(() => { api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: null }); });
  await act(async () => { finish({ ...detail("first").segments[0], clean_text: "Accepted correction", clean_text_manual: true }); });
  await screen.findByText("Accepted correction");
  await act(async () => { finishRead(detail("first")); });
  expect(screen.getByText("Accepted correction")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Raw" })).toBeInTheDocument();
});

const initialVersion: TranscriptVersion = {
  id: "original", source: "live", model: "whisper-small", language: "en", created_at: "2026-11-01T00:00:02Z",
};

it.each([false, true])("recovers progress, opens the completed version, and reports cancellation (managed: %s)", async (serviceDesktop) => {
  const active: TranscriptVersionJob = { id: "job-1", phase: "recognizing", progress: 0.4, elapsed_ms: 12_000, can_cancel: true, version: null, error: null };
  const version: TranscriptVersion = { ...initialVersion, id: "v1", source: "discord" };
  const renderMeeting = () => render(<MemoryRouter initialEntries={["/meeting/first"]}>
    <Routes><Route path="/meeting/:id" element={<MeetingPage serviceDesktop={serviceDesktop} />} /></Routes>
  </MemoryRouter>);
  api.transcriptVersionJob.mockResolvedValue(active);
  api.transcriptVersion.mockResolvedValue([{ ...detail("first").segments[0], text: "Recovered new version" }]);
  const opened = renderMeeting();
  expect(await screen.findByRole("progressbar", { name: "Transcribing the new version" })).toHaveAttribute("aria-valuenow", "40");
  expect(screen.getByText("Control text")).toBeVisible();
  opened.unmount();

  api.transcriptVersionJob.mockResolvedValue({ ...active, phase: "aligning", progress: 1, elapsed_ms: 17_000 });
  const reopened = renderMeeting();
  expect(await screen.findByRole("progressbar", { name: "Matching speakers and recording timestamps" })).toHaveAttribute("aria-valuenow", "99");
  expect(screen.getByText("0:17")).toBeVisible();
  expect(screen.getByText("Control text")).toBeVisible();
  api.transcriptVersionJob.mockResolvedValue({ ...active, phase: "succeeded", version, progress: 1, elapsed_ms: 18_000, can_cancel: false });
  expect(await screen.findByText("Recovered new version")).toBeVisible();
  expect(screen.getByRole("progressbar", { name: "New version saved" })).toHaveAttribute("aria-valuenow", "100");
  reopened.unmount();

  const second = { ...active, id: "job-2" };
  api.transcriptVersionJob.mockResolvedValue(second);
  api.cancelTranscriptVersion.mockImplementation(async () => {
    api.transcriptVersionJob.mockResolvedValue({ ...second, phase: "cancelled", progress: null, can_cancel: false });
    return true;
  });
  renderMeeting();
  fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(api.cancelTranscriptVersion).toHaveBeenCalledExactlyOnceWith("first", "job-2");
  expect(await screen.findByText("Version creation cancelled")).toBeVisible();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  expect(screen.getByText("Control text")).toBeVisible();
});

it("reopens a failed version job and keeps the saved transcript visible", async () => {
  api.transcriptVersionJob.mockResolvedValue({ id: "job-1", phase: "failed", progress: null, elapsed_ms: 400, can_cancel: false,
    version: null, error: "The model could not be loaded." } satisfies TranscriptVersionJob);
  renderPage();
  expect(await screen.findByRole("alert")).toHaveTextContent("The model could not be loaded.");
  expect(screen.getByText("Control text")).toBeVisible();
  expect(screen.getByRole("button", { name: "New transcript version" })).toBeEnabled();
});

it("keeps finalized live text visible until the initial version is available after Stop", async () => {
  let status: MeetingStatus = "recording";
  const caption = { start_ms: 0, end_ms: 800, text: "Final live line", speaker_name: "Alice", is_final: true };
  api.live = { finals: [caption], partials: [{ ...caption, start_ms: 900, text: "Provisional line", is_final: false }], notes: "", notes_updated_ms: null };
  api.getMeeting.mockImplementation(async () => {
    const value = detail("first");
    return { ...value, meeting: { ...value.meeting, status }, segments: status === "ready"
      ? [{ ...value.segments[0], text: caption.text, speaker: caption.speaker_name, clean_text: "Background cleanup" }]
      : [] };
  });
  renderPage();
  expect(await screen.findByText(caption.text)).toBeVisible();

  status = "processing";
  await act(async () => api.versionListeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 1000 }));
  expect(screen.getByText(caption.text)).toBeVisible();
  expect(screen.getByTestId("transcript-test")).toHaveAttribute("data-seekable", "false");

  let finishList!: (value: TranscriptVersions) => void;
  api.transcriptVersions.mockImplementationOnce(() => new Promise<TranscriptVersions>((resolve) => { finishList = resolve; }));
  status = "ready";
  await act(async () => api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: null }));
  expect(screen.getByText(caption.text)).toBeVisible();
  expect(screen.getByText("Alice")).toBeVisible();
  expect(screen.queryByText("Background cleanup")).not.toBeInTheDocument();
  await act(async () => finishList({ versions: [initialVersion], initial_id: "original", participant_source: "meet" }));
  expect(await screen.findByRole("option", { name: /Initial transcript.*Live captions/ })).toBeVisible();
  expect(screen.getByText(caption.text)).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Cleaned" }));
  expect(screen.getByText("Background cleanup")).toBeVisible();
  expect(api.transcriptVersion).not.toHaveBeenCalled();
});

it("creates two separate versions, keeps the current text during progress, and reopens both", async () => {
  const value = detail("first");
  api.getMeeting.mockResolvedValue({ ...value, segments: [{ ...value.segments[0], clean_text: "Manual correction", clean_text_manual: true }] });
  api.listModels.mockResolvedValue([{ present: true, spec: { kind: "asr", id: "whisper-small", name: "Whisper Small" } }]);
  const saved = [initialVersion];
  api.transcriptVersions.mockImplementation(async () => ({ versions: [...saved], initial_id: "original", participant_source: "meet" }));
  api.transcriptVersion.mockImplementation(async (_id: string, version: string) => [{
    ...value.segments[0], text: version === "v1" ? "Participant version" : "Recording version",
  }]);
  let complete!: (version: TranscriptVersion) => void;
  api.regenerateTranscript.mockImplementation(() => new Promise<TranscriptVersion>((resolve) => {
    complete = (version) => { saved.push(version); resolve(version); };
  }));
  const first = renderPage();
  const picker = await screen.findByRole("combobox", { name: "Transcript version" });
  fireEvent.click(screen.getByRole("button", { name: "Cleaned" }));
  fireEvent.click(screen.getByRole("button", { name: "New transcript version" }));
  fireEvent.change(screen.getByRole("combobox", { name: "Speech model" }), { target: { value: "whisper-small" } });
  fireEvent.change(screen.getByRole("combobox", { name: "Speech language" }), { target: { value: "auto" } });
  fireEvent.click(screen.getByRole("button", { name: "Generate version" }));
  expect(api.regenerateTranscript).toHaveBeenCalledExactlyOnceWith("first", { source: "participants", model: "whisper-small", language: "auto" });
  act(() => api.versionListeners.get("transcript_version_progress")?.({ meeting_id: "other", phase: "recognizing", progress: 0.7 }));
  expect(screen.getByRole("progressbar", { name: "Preparing a new version" })).not.toHaveAttribute("aria-valuenow");
  act(() => api.versionListeners.get("transcript_version_progress")?.({ meeting_id: "first", phase: "recognizing", progress: 0.4 }));
  expect(screen.getByRole("progressbar", { name: "Transcribing the new version" })).toHaveAttribute("aria-valuenow", "40");
  expect(screen.getByText("Manual correction")).toBeVisible();
  expect(picker).toBeEnabled();

  await act(async () => complete({ ...initialVersion, id: "v1", source: "meet" }));
  expect(await screen.findByText("Participant version")).toBeVisible();
  expect(picker).toHaveValue("version:v1");
  expect(screen.getByTestId("transcript-test")).toHaveAttribute("data-seekable", "false");
  expect(screen.queryByRole("button", { name: "Test save correction" })).not.toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "New transcript version" }));
  fireEvent.change(screen.getByRole("combobox", { name: "Audio source" }), { target: { value: "recording" } });
  fireEvent.click(screen.getByRole("button", { name: "Generate version" }));
  expect(api.regenerateTranscript).toHaveBeenLastCalledWith("first", { source: "recording", model: "whisper-small", language: "auto" });
  expect(screen.getByText("Participant version")).toBeVisible();
  await act(async () => complete({ ...initialVersion, id: "v2", source: "recording", created_at: "2026-11-01T00:01:00Z" }));
  expect(await screen.findByText("Recording version")).toBeVisible();
  expect(picker).toHaveValue("version:v2");
  expect(api.retranscribe).not.toHaveBeenCalled();

  first.unmount();
  renderPage();
  expect(await screen.findByRole("option", { name: /Google Meet tracks/ })).toBeInTheDocument();
  expect(screen.getByRole("option", { name: /Recording audio/ })).toBeInTheDocument();
  const reopened = screen.getByRole("combobox", { name: "Transcript version" });
  fireEvent.change(reopened, { target: { value: "version:v2" } });
  expect(await screen.findByText("Recording version")).toBeVisible();
  expect(screen.getByTestId("transcript-test")).toHaveAttribute("data-seekable", "true");
  fireEvent.change(reopened, { target: { value: "version:original" } });
  expect(await screen.findByText("Control text")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Cleaned" }));
  expect(await screen.findByText("Manual correction")).toBeVisible();
});

it("keeps a text-only transcript and exports while disabling audio-based reprocessing", async () => {
  api.getMeeting.mockImplementation(async (id: string) => ({ ...detail(id), media: { audio: null, video: null }, highlights: [1000] }));
  api.listModels.mockResolvedValue([{ present: true, spec: { kind: "asr", id: "whisper-small", name: "Whisper Small" } }]);
  renderPage();
  await screen.findByRole("heading", { name: "First meeting" });
  expect(screen.getByText("Control text")).toBeInTheDocument();
  expect(screen.getByText("No audio or video is available.")).toBeInTheDocument();
  expect(screen.queryByText("Media player")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "0:01" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Test save correction" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "Re-run" }));
  expect(screen.queryByRole("button", { name: "New transcript version" })).not.toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Speaker detection only" })).toBeDisabled();
  expect(screen.getByRole("menuitem", { name: "AI clean-up only" })).toBeEnabled();
  expect(api.retranscribe).not.toHaveBeenCalled();
});

it("opens the effective combined transcript and loads the original archive read-only", async () => {
  const combined: TranscriptVersion = { ...initialVersion, id: "combined-1", source: "combined" };
  const canonical = { ...detail("first").segments[0], text: "Combined canonical text" };
  api.getMeeting.mockResolvedValue({ ...detail("first"), segments: [canonical] });
  api.transcriptVersions.mockResolvedValue({ versions: [initialVersion, combined], initial_id: "original", effective_id: combined.id, participant_source: "discord" });
  api.transcriptVersion.mockImplementation(async (_id: string, versionId: string) => [{
    ...canonical, text: versionId === "original" ? "Archived live text" : canonical.text,
  }]);
  api.setTranscriptCorrection.mockResolvedValue({ ...canonical, clean_text: "Edited control text", clean_text_manual: true });
  renderPage();
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Transcript version" })).toHaveValue("version:combined-1"));
  expect(api.transcriptVersion).toHaveBeenCalledWith("first", "combined-1");
  fireEvent.click(await screen.findByRole("button", { name: "Test save correction" }));
  expect(await screen.findByText("Edited control text")).toBeVisible();
  fireEvent.change(screen.getByRole("combobox", { name: "Transcript version" }), { target: { value: "version:original" } });
  expect(await screen.findByText("Archived live text")).toBeVisible();
  expect(api.transcriptVersion).toHaveBeenCalledWith("first", "original");
  expect(screen.queryByRole("button", { name: "Test save correction" })).not.toBeInTheDocument();
});

it("follows a completed combined promotion when the viewer has not chosen another version", async () => {
  const combined: TranscriptVersion = { ...initialVersion, id: "combined-1", source: "combined" };
  let promoted = false;
  api.transcriptVersions.mockImplementation(async () => ({ versions: promoted ? [initialVersion, combined] : [initialVersion],
    initial_id: "original", effective_id: promoted ? combined.id : "original", participant_source: "meet" }));
  api.transcriptVersion.mockImplementation(async (_id: string, versionId: string) => [{
    ...detail("first").segments[0], text: versionId === "original" ? "Live first version" : "Completed combined version",
  }]);
  renderPage();
  expect(await screen.findByText("Live first version")).toBeVisible();
  promoted = true;
  await act(async () => api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: null }));
  expect(await screen.findByText("Completed combined version")).toBeVisible();
  expect(screen.getByRole("combobox", { name: "Transcript version" })).toHaveValue("version:combined-1");
});

it("preserves an explicit archive selection when a combined version is promoted", async () => {
  const combined: TranscriptVersion = { ...initialVersion, id: "combined-1", source: "combined" };
  const manual: TranscriptVersion = { ...initialVersion, id: "manual-1", source: "recording" };
  let promoted = false;
  api.transcriptVersions.mockImplementation(async () => ({ versions: [initialVersion, manual, ...(promoted ? [combined] : [])],
    initial_id: "original", effective_id: promoted ? combined.id : "original", participant_source: "meet" }));
  api.transcriptVersion.mockImplementation(async (_id: string, versionId: string) => [{
    ...detail("first").segments[0], text: versionId === manual.id ? "Explicit manual version" : versionId === combined.id ? "Automatic combined version" : "Archived live version",
  }]);
  renderPage();
  expect(await screen.findByText("Archived live version")).toBeVisible();
  fireEvent.change(screen.getByRole("combobox", { name: "Transcript version" }), { target: { value: "version:manual-1" } });
  expect(await screen.findByText("Explicit manual version")).toBeVisible();
  promoted = true;
  await act(async () => api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: null }));
  await screen.findByRole("option", { name: /Participants and system audio/ });
  expect(screen.getByText("Explicit manual version")).toBeVisible();
  expect(screen.queryByText("Automatic combined version")).not.toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "Transcript version" })).toHaveValue("version:manual-1");
});

it("does not let a late automatic version read replace a newer explicit choice", async () => {
  const combined: TranscriptVersion = { ...initialVersion, id: "combined-1", source: "combined" };
  let finish!: (rows: Segment[]) => void;
  api.transcriptVersions.mockResolvedValue({ versions: [initialVersion, combined], initial_id: "original", effective_id: combined.id, participant_source: "discord" });
  api.transcriptVersion.mockImplementation(async (_id: string, versionId: string) => versionId === combined.id
    ? new Promise<Segment[]>((resolve) => { finish = resolve; })
    : [{ ...detail("first").segments[0], text: "Explicit original archive" }]);
  renderPage();
  const picker = await screen.findByRole("combobox", { name: "Transcript version" });
  await waitFor(() => expect(api.transcriptVersion).toHaveBeenCalledWith("first", combined.id));
  fireEvent.change(picker, { target: { value: "version:original" } });
  expect(await screen.findByText("Explicit original archive")).toBeVisible();
  await act(async () => finish([{ ...detail("first").segments[0], text: "Late automatic response" }]));
  expect(screen.getByText("Explicit original archive")).toBeVisible();
  expect(screen.queryByText("Late automatic response")).not.toBeInTheDocument();
});

it("marks a partial combined result and seeks only verified bot or local system times", async () => {
  const combined: TranscriptVersion = { ...initialVersion, id: "combined-partial", source: "combined", partial: true };
  api.transcriptVersions.mockResolvedValue({ versions: [initialVersion, combined], initial_id: "original", effective_id: "original", participant_source: "meet" });
  const original = detail("first").segments[0];
  const rows: TranscriptVersionSegment[] = [
    { ...original, id: -1, text: "Unmapped bot words", provenance: { source: "bot", provider: "meet", source_start_ms: 0, source_end_ms: 900, uncertain: true } },
    { ...original, id: -2, start_ms: 1100, end_ms: 1500, provenance: { source: "system", source_start_ms: 1100, source_end_ms: 1500, uncertain: false } },
  ];
  api.transcriptVersion.mockImplementation(async (_id: string, versionId: string) => versionId === "original" ? [original] : rows);
  renderPage();
  const picker = await screen.findByRole("combobox", { name: "Transcript version" });
  fireEvent.change(picker, { target: { value: "version:combined-partial" } });
  expect(await screen.findByText(/This combined transcript may contain missing or uncertain speech/)).toBeVisible();
  expect(screen.getByRole("option", { name: /Participants and system audio.*Incomplete/ })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Test seek for line -1" })).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Test seek for line -2" }));
  expect(api.seekRecording).toHaveBeenCalledExactlyOnceWith(1100);
  expect(screen.queryByRole("button", { name: "Test save correction" })).not.toBeInTheDocument();
});

it("offers combined regeneration using the new source contract", async () => {
  const combined: TranscriptVersion = { ...initialVersion, id: "manual-combined", source: "combined" };
  api.transcriptVersions.mockResolvedValue({ versions: [initialVersion, combined], initial_id: "original", participant_source: "discord" });
  api.regenerateTranscript.mockResolvedValue(combined);
  api.transcriptVersion.mockResolvedValue([{ ...detail("first").segments[0], text: "Combined manual result" }]);
  renderPage();
  fireEvent.click(await screen.findByRole("button", { name: "New transcript version" }));
  expect(await screen.findByRole("option", { name: /^Participants and system audio$/ })).toBeEnabled();
  fireEvent.change(screen.getByRole("combobox", { name: "Audio source" }), { target: { value: "combined" } });
  fireEvent.click(screen.getByRole("button", { name: "Generate version" }));
  expect(api.regenerateTranscript).toHaveBeenCalledExactlyOnceWith("first", { source: "combined" });
  expect(await screen.findByText("Combined manual result")).toBeVisible();
  expect(screen.getByRole("combobox", { name: "Transcript version" })).toHaveValue("version:manual-combined");
});

it("removes the old player when a refreshed meeting no longer has media", async () => {
  renderPage();
  await screen.findByRole("heading", { name: "First meeting" });
  expect(screen.getByText("Media player")).toBeInTheDocument();
  api.getMeeting.mockResolvedValueOnce({ ...detail("first"), media: { audio: null, video: null } });
  act(() => { api.processingListener?.({ meeting_id: "first", stage: "done", progress: 1, error: null }); });
  await screen.findByText("No audio or video is available.");
  expect(screen.queryByText("Media player")).not.toBeInTheDocument();
  expect(screen.getByText("Control text")).toBeInTheDocument();
});

it("does not suggest retrying audio transcription when the recording is unavailable", async () => {
  api.getMeeting.mockImplementation(async (id: string) => ({ ...detail(id), meeting: { ...detail(id).meeting, error: "Recording unavailable" }, media: { audio: null, video: null } }));
  renderPage();
  await screen.findByRole("heading", { name: "First meeting" });
  const retry = screen.getByRole("button", { name: "Try transcription again" });
  expect(retry).toBeDisabled();
  fireEvent.click(retry);
  expect(api.retranscribe).not.toHaveBeenCalled();
});

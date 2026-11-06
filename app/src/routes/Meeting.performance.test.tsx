import { act, cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Events, MeetingDetail } from "@/lib/ipc";
import { MeetingPage } from "./Meeting";

const api = vi.hoisted(() => ({
  getMeeting: vi.fn(), getSummary: vi.fn(), sttTranscript: vi.fn(), meetingTags: vi.fn(),
  listFolders: vi.fn(), listTemplates: vi.fn(), listModels: vi.fn(), listKnownSpeakers: vi.fn(),
  botTranscriptSources: vi.fn(),
  transcriptVersions: vi.fn(),
}));
const player = vi.hoisted(() => ({ onTime: undefined as ((ms: number) => void) | undefined }));
const listeners = vi.hoisted(() => new Map<string, (payload: Events["processing"]) => void>());
const work = vi.hoisted(() => ({ stamp: vi.fn() }));
const originalScroll = Object.getOwnPropertyDescriptor(Element.prototype, "scrollIntoView");

vi.mock("@/lib/ipc", () => ({ cmd: api, subscribe: (name: string, callback: (payload: Events["processing"]) => void) => {
  listeners.set(name, callback);
  return () => { listeners.delete(name); };
} }));
vi.mock("@/lib/format", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/format")>();
  return { ...actual, fmtStamp: (ms: number) => { work.stamp(ms); return actual.fmtStamp(ms); } };
});
vi.mock("@/components/Player", async () => {
  const { forwardRef } = await import("react");
  return { Player: forwardRef((_props: { onTime: (ms: number) => void }, _ref) => {
    player.onTime = _props.onTime;
    return <div>Media player</div>;
  }) };
});
vi.mock("@/components/LiveView", () => ({ LiveTranscript: () => null, LiveNotes: () => null, useLiveFeed: () => null }));
vi.mock("@/components/SpeakersPanel", () => ({ SpeakersPanel: () => null }));

beforeEach(() => {
  vi.clearAllMocks();
  listeners.clear();
  player.onTime = undefined;
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  const detail: MeetingDetail = {
    meeting: {
      id: "long", title: "Long synthetic meeting", app: null, status: "ready",
      started_at: "2026-11-02T00:00:00Z", ended_at: "2026-11-02T02:05:00Z", duration_ms: 7_500_000,
      has_video: false, folder_id: null, created_at: "2026-11-02T00:00:00Z", error: null,
      title_auto: false, participants: [], calendar_uid: null,
    },
    segments: Array.from({ length: 1500 }, (_, index) => ({
      id: index + 1, meeting_id: "long", start_ms: index * 5000, end_ms: index * 5000 + 4500,
      text: `Synthetic statement ${index + 1}`, clean_text: null, clean_text_manual: false, speaker: null,
    })),
    media: { audio: "synthetic.wav", video: null }, highlights: [], speakers: [], speaker_stats: [],
  };
  api.getMeeting.mockResolvedValue(detail);
  api.getSummary.mockResolvedValue(null);
  api.sttTranscript.mockResolvedValue(null);
  api.botTranscriptSources.mockResolvedValue([]);
  api.transcriptVersions.mockResolvedValue({ versions: [], initial_id: null, participant_source: null });
  for (const query of [api.meetingTags, api.listFolders, api.listTemplates, api.listModels, api.listKnownSpeakers]) {
    query.mockResolvedValue([]);
  }
});
afterEach(() => {
  cleanup(); vi.restoreAllMocks();
  if (originalScroll) Object.defineProperty(Element.prototype, "scrollIntoView", originalScroll);
  else Reflect.deleteProperty(Element.prototype, "scrollIntoView");
});

it("keeps transcript row work bounded through the real meeting page during playback and progress events", async () => {
  const { container } = render(<MemoryRouter initialEntries={["/meeting/long"]}>
    <Routes><Route path="/meeting/:id" element={<MeetingPage />} /></Routes>
  </MemoryRouter>);
  await act(async () => {});
  expect(screen.getByRole("heading", { name: "Long synthetic meeting" })).toBeInTheDocument();
  expect(screen.getByText("Synthetic statement 1500")).toBeInTheDocument();
  // fmtStamp is called inside each real TranscriptRow, including its edit control.
  // Counting work here also catches unstable callbacks/arrays supplied by Meeting.
  expect(work.stamp.mock.calls.length).toBeGreaterThanOrEqual(1500);
  work.stamp.mockClear();
  const onTime = player.onTime;
  expect(onTime).toEqual(expect.any(Function));

  for (let tick = 1; tick <= 30; tick++) {
    act(() => onTime!(tick * 100));
  }
  expect(work.stamp.mock.calls.length).toBe(0);

  act(() => onTime!(5100));
  expect(work.stamp.mock.calls.length).toBeGreaterThan(0);
  expect(work.stamp.mock.calls.length).toBeLessThanOrEqual(4);
  const active = container.querySelectorAll("button[data-active='true']");
  expect(active).toHaveLength(1);
  expect(active[0]).toHaveTextContent("Synthetic statement 2");

  // Entering processing changes editability, so allow that semantic update first.
  const processing = listeners.get("processing");
  expect(processing).toEqual(expect.any(Function));
  act(() => processing!({ meeting_id: "long", stage: "cleanup", progress: 0.75, error: null }));
  const progressBar = screen.getByRole("progressbar", { name: "Cleaning up the transcript" });
  expect(progressBar).toHaveAttribute("aria-valuenow", "0");
  work.stamp.mockClear();
  for (let progress = 1; progress < 100; progress++) {
    act(() => processing!({ meeting_id: "long", stage: "cleanup", progress: 0.75 + 0.25 * progress / 100, error: null }));
  }
  expect(progressBar).toHaveAttribute("aria-valuenow", "99");
  expect(work.stamp.mock.calls.length).toBe(0);
  expect(api.getMeeting).toHaveBeenCalledTimes(1);
});

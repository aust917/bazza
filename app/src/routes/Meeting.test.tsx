import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Link, MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, expect, it, vi } from "vitest";
import { MeetingPage } from "./Meeting";

const api = vi.hoisted(() => ({
  getMeeting: vi.fn(), getSummary: vi.fn(), sttTranscript: vi.fn(), meetingTags: vi.fn(),
  listFolders: vi.fn(), listTemplates: vi.fn(), listModels: vi.fn(), listKnownSpeakers: vi.fn(),
  summaryJob: vi.fn(), summaryAgents: vi.fn(), generateSummary: vi.fn(), cancelSummary: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd: api, subscribe: () => () => {} }));
vi.mock("@/components/Player", async () => {
  const { forwardRef } = await import("react");
  return { Player: forwardRef(() => <div>Media player</div>) };
});
vi.mock("@/components/Transcript", () => ({ Transcript: () => <div>Saved transcript</div>, SPEAKER_COLORS: ["text-ink"] }));
vi.mock("@/components/LiveView", () => ({ LiveTranscript: () => null, LiveNotes: () => null, useLiveFeed: () => null }));

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
  api.summaryAgents.mockResolvedValue([{ agent: "codex", installed: true }, { agent: "claude", installed: false }]);
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

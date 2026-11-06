import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { ServiceFixture } from "./ServiceFixture";
import type { DesktopMeeting, MeetingContinuation, RepositoryIdentity, SummaryPage, TranscriptPage } from "@/lib/service-fixture-ipc";

const api = vi.hoisted(() => ({
  available: true, bootstrap: vi.fn(), preferences: vi.fn(), reconnect: vi.fn(), updatePreferences: vi.fn(),
  renameMeeting: vi.fn(), deleteMeeting: vi.fn(), meeting: vi.fn(), meetings: vi.fn(), changes: vi.fn(), transcript: vi.fn(), summary: vi.fn(),
  captureStatus: vi.fn(), captureStart: vi.fn(), capturePause: vi.fn(), captureResume: vi.fn(), captureStop: vi.fn(),
}));
vi.mock("@/lib/service-fixture-ipc", async (original) => ({
  ...await original<typeof import("@/lib/service-fixture-ipc")>(),
  fixtureAvailable: () => api.available, serviceFixture: api,
}));

const identity: RepositoryIdentity = { library_id: "fixture-library", library_epoch: "epoch-1", cursor_scope: "scope" };
const preferences = { revision: "prefs-1", ui_locale: "en" as const, close_to_tray: true, hotkey: "Ctrl+Shift+R" };
const meeting = (id = "first", title = "First fixture"): DesktopMeeting => ({
  id, title, app: null, started_at: "2026-11-02T00:00:00Z", ended_at: null, duration_ms: 0, has_video: false,
  status: "ready", folder_id: null, created_at: "2026-11-02T00:00:00Z", error: null, error_truncated: false,
  title_auto: false, participants: [], calendar_uid: null, content_kind: "text", metadata_revision: `metadata-${id}`,
  lifecycle_revision: "life-1", transcript_revision: `transcript-${id}`, transcript_state: "present",
  summary_revision: `summary-${id}`, summary_state: "present", media: { audio: false, video: false },
});
const continuation: MeetingContinuation = { library_id: identity.library_id, library_epoch: identity.library_epoch, cursor: { library_scope: "scope", snapshot_sequence: 1, query_hash: "hash", offset: 20 } };
function transcript(text: string, id = "first", representation: "raw" | "effective" = "raw"): TranscriptPage {
  return {
    identity, state: { meeting_id: id, revision: `transcript-${id}`, current_source_id: "source", document_state: "present", deletion_epoch: 0 },
    source_id: "source", representation, items: [{ segment_index: 0, segment_id: 1, start_ms: 0, end_ms: 1000, speaker: null, text, byte_offset: 0, total_bytes: 100, has_clean_text: true, clean_text_manual: false, effective_origin: "automatic" }], next: null,
  };
}
function summary(text: string, offset = 0): SummaryPage {
  return {
    identity, state: { meeting_id: "first", revision: "summary-first", document_state: "present", deletion_epoch: 0, automatic_generation_suppressed: false, origin: "manual", auto_replace: false, source_label: null, source_revision: null, source_missing: false },
    field: "structured_json", text, byte_offset: offset, total_bytes: 100, next: null,
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
async function connected() {
  render(<ServiceFixture />);
  await screen.findByText(/Library fixture-library/);
}
async function selectFirst() {
  fireEvent.click(screen.getByRole("button", { name: "Read meetings" }));
  fireEvent.click(await screen.findByRole("button", { name: "First fixture" }));
  await screen.findByLabelText("Title");
}
beforeEach(() => {
  vi.resetAllMocks(); api.available = true;
  api.bootstrap.mockResolvedValue({ identity, data: { preferences, change_sequence: 1, cursor_scope: "scope" } });
  api.preferences.mockResolvedValue({ identity, data: preferences });
  api.reconnect.mockResolvedValue(identity);
  api.meetings.mockResolvedValue({ identity, items: [meeting()], next: null, snapshot_sequence: 1 });
  api.meeting.mockImplementation(async (id: string) => ({ identity, data: meeting(id, id === "first" ? "First fixture" : "Second fixture") }));
  api.transcript.mockResolvedValue(transcript("Raw fixture text"));
  api.summary.mockResolvedValue(summary('{"part":'));
  api.changes.mockResolvedValue({ identity, items: [], next: { library_id: identity.library_id, library_epoch: identity.library_epoch, cursor: { library_scope: "scope", after: 1 } }, has_more: false, current_sequence: 1 });
  api.captureStatus.mockResolvedValue({ state: "idle", meeting_id: null, elapsed_ms: 0, job_id: null });
  const outcome = { entity_id: "first", revision: "updated-2", change_sequence: 2, job_id: null, dedup_until: "2026-11-02T01:00:00Z" };
  api.renameMeeting.mockResolvedValue({ identity, data: outcome });
  api.updatePreferences.mockResolvedValue({ identity, data: outcome });
});

it("shows service unavailability outside Tauri without any fallback call", () => {
  api.available = false;
  render(<ServiceFixture />);
  expect(screen.getByRole("alert")).toHaveTextContent("Service unavailable outside");
  expect(api.bootstrap).not.toHaveBeenCalled(); expect(api.meetings).not.toHaveBeenCalled();
  expect(api.captureStatus).not.toHaveBeenCalled();
});

it("pauses and resumes capture, then stops from pause and retries finalization", async () => {
  const recorded = { meeting_id: "fixture-capture-id", elapsed_ms: 1400, job_id: null };
  api.captureStart.mockResolvedValue({ ...recorded, state: "recording" });
  api.capturePause.mockResolvedValueOnce({ ...recorded, state: "paused" });
  api.capturePause.mockRejectedValueOnce({ code: "VIDEO_TIMELINE_UNAVAILABLE", message: "Timing update failed.", retryable: true });
  api.captureResume.mockResolvedValue({ ...recorded, state: "recording" });
  api.captureStop.mockRejectedValueOnce({ code: "SERVICE_UNAVAILABLE", message: "Retry after reconnecting.", retryable: true });
  api.captureStop.mockResolvedValueOnce({ ...recorded, state: "idle", job_id: "fixture-job-id" });
  api.captureStatus.mockResolvedValueOnce({ state: "idle", meeting_id: null, elapsed_ms: 0, job_id: null });
  api.captureStatus.mockResolvedValueOnce({ ...recorded, state: "paused" });
  api.captureStatus.mockResolvedValueOnce({ ...recorded, state: "pending" });
  await connected();
  expect(api.captureStart).not.toHaveBeenCalled();
  const start = screen.getByRole("button", { name: "Start microphone capture" });
  await waitFor(() => expect(start).toBeEnabled());
  fireEvent.click(screen.getByRole("checkbox", { name: "Include primary screen" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Use saved audio devices" }));
  fireEvent.click(start);
  await screen.findByText(/Capture recording · meeting fixture-capture-id/);
  expect(start).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Pause capture" }));
  await screen.findByText(/Capture paused · meeting fixture-capture-id/);
  fireEvent.click(screen.getByRole("button", { name: "Resume capture" }));
  await screen.findByText(/Capture recording · meeting fixture-capture-id/);
  expect(api.captureResume).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Pause capture" }));
  await screen.findByText(/Timing update failed/);
  await screen.findByText(/Capture paused · meeting fixture-capture-id/);
  expect(screen.getByText(/Timing update failed/)).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Stop microphone capture" }));
  await screen.findByText(/Retry after reconnecting/);
  expect(start).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Refresh capture status" }));
  fireEvent.click(await screen.findByRole("button", { name: "Retry finalization" }));
  await screen.findByText(/Capture idle · meeting fixture-capture-id · job fixture-job-id/);
  expect(api.captureStart).toHaveBeenCalledTimes(1);
  expect(api.captureStart).toHaveBeenCalledWith(true, true);
  expect(api.captureStop).toHaveBeenCalledTimes(2);
});
it("reads only explicit bounded pages and replaces the current page", async () => {
  api.meetings.mockResolvedValueOnce({ identity, items: [meeting()], next: continuation, snapshot_sequence: 1 });
  await connected();
  expect(api.meetings).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Read meetings" }));
  await screen.findByRole("button", { name: "First fixture" });
  expect(api.meetings).toHaveBeenCalledExactlyOnceWith({ filter: { folder: { kind: "all" }, tag_id: null, content_kind: null }, cursor: null, limit: 20, max_bytes: 16384 });
  api.meetings.mockResolvedValueOnce({ identity, items: [meeting("second", "Second fixture")], next: null, snapshot_sequence: 1 });
  fireEvent.click(screen.getByRole("button", { name: "Next meeting page" }));
  await screen.findByRole("button", { name: "Second fixture" });
  expect(screen.queryByRole("button", { name: "First fixture" })).not.toBeInTheDocument();
  expect(api.meetings.mock.calls[1][0].cursor).toEqual(continuation);
  expect(api.meetings).toHaveBeenCalledTimes(2);
});

it("ignores late headers and transcript replies after the selection changes", async () => {
  const oldHeader = deferred<{ identity: RepositoryIdentity; data: DesktopMeeting }>();
  const oldTranscript = deferred<TranscriptPage>();
  api.meetings.mockResolvedValue({ identity, items: [meeting(), meeting("second", "Second fixture")], next: null, snapshot_sequence: 1 });
  api.meeting.mockReturnValueOnce(oldHeader.promise);
  await connected();
  fireEvent.click(screen.getByRole("button", { name: "Read meetings" }));
  fireEvent.click(await screen.findByRole("button", { name: "First fixture" }));
  fireEvent.click(screen.getByRole("button", { name: "Second fixture" }));
  await screen.findByDisplayValue("Second fixture");
  await act(async () => oldHeader.resolve({ identity, data: meeting() }));
  expect(screen.getByLabelText("Title")).toHaveValue("Second fixture");
  api.transcript.mockReturnValueOnce(oldTranscript.promise);
  fireEvent.click(screen.getByRole("button", { name: "Read transcript" }));
  fireEvent.click(screen.getByRole("button", { name: "First fixture" }));
  await screen.findByDisplayValue("First fixture");
  await act(async () => oldTranscript.resolve(transcript("Obsolete second transcript", "second")));
  expect(screen.queryByText("Obsolete second transcript")).not.toBeInTheDocument();
});

it("clears content and ignores old replies when raw/effective or query changes", async () => {
  const oldTranscript = deferred<TranscriptPage>();
  await connected(); await selectFirst();
  api.transcript.mockReturnValueOnce(oldTranscript.promise);
  fireEvent.click(screen.getByRole("button", { name: "Read transcript" }));
  fireEvent.change(screen.getByLabelText("Representation"), { target: { value: "effective" } });
  api.transcript.mockResolvedValueOnce(transcript("Effective text", "first", "effective"));
  fireEvent.click(screen.getByRole("button", { name: "Read transcript" }));
  await screen.findByText("Effective text");
  await act(async () => oldTranscript.resolve(transcript("Stale raw text")));
  expect(screen.queryByText("Stale raw text")).not.toBeInTheDocument();
  expect(api.transcript.mock.calls[1][0]).toMatchObject({ expected_revision: "transcript-first", representation: "effective", cursor: null });
  fireEvent.change(screen.getByLabelText("Content kind"), { target: { value: "text" } });
  expect(screen.queryByTestId("transcript-page")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "First fixture" })).not.toBeInTheDocument();
  expect(api.meetings).toHaveBeenCalledTimes(1);
});

it("replaces transcript fragment pages while retaining the header revision", async () => {
  const first = transcript("First fragment");
  first.next = { library_id: identity.library_id, library_epoch: identity.library_epoch, cursor: { library_scope: "scope", meeting_id: "first", revision: "transcript-first", current_source_id: "source", source_id: "source", representation: "raw", segment_index: 0, byte_offset: 14, query_hash: "hash" } };
  const second = transcript("Second fragment");
  second.items[0].byte_offset = 14;
  api.transcript.mockResolvedValueOnce(first).mockResolvedValueOnce(second);
  await connected(); await selectFirst();
  fireEvent.click(screen.getByRole("button", { name: "Read transcript" }));
  await screen.findByText("First fragment");
  expect(api.transcript).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Next transcript page" }));
  await screen.findByText("Second fragment");
  expect(screen.queryByText("First fragment")).not.toBeInTheDocument();
  expect(api.transcript.mock.calls[1][0]).toMatchObject({ expected_revision: "transcript-first", cursor: first.next, limit: 20, max_bytes: 16384 });
});

it("writes using the displayed revision and a unique key without a hidden refresh or retry", async () => {
  await connected(); await selectFirst();
  fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Renamed fixture" } });
  fireEvent.click(screen.getByRole("button", { name: "Rename meeting" }));
  await waitFor(() => expect(api.renameMeeting).toHaveBeenCalledTimes(1));
  const rename = api.renameMeeting.mock.calls[0][0];
  expect(rename).toMatchObject({ meeting_id: "first", expected_revision: "metadata-first", title: "Renamed fixture" });
  expect(rename.idempotency_key).toMatch(/^[\w-]{36}$/);
  expect(api.meeting).toHaveBeenCalledTimes(1);
  await screen.findByText(/metadata updated-2/);
  fireEvent.change(screen.getByLabelText("Fixture locale"), { target: { value: "ru" } });
  api.updatePreferences.mockRejectedValueOnce({ code: "CONFLICT", message: "Preferences changed.", retryable: false });
  fireEvent.click(screen.getByRole("button", { name: "Save preferences" }));
  await screen.findByRole("alert");
  const update = api.updatePreferences.mock.calls[0][0];
  expect(update).toMatchObject({ expected_revision: "prefs-1", patch: { ui_locale: "ru", close_to_tray: true, hotkey: "Ctrl+Shift+R" } });
  expect(update.idempotency_key).not.toBe(rename.idempotency_key);
  expect(api.preferences).not.toHaveBeenCalled();
  expect(api.updatePreferences).toHaveBeenCalledTimes(1);
  expect(screen.queryByLabelText("Fixture locale")).not.toBeInTheDocument();
});

it("confirms deletion and reuses the same lifecycle revision and key after a lost reply", async () => {
  api.deleteMeeting.mockRejectedValueOnce({ code: "DEADLINE_EXCEEDED", message: "Reply timed out.", retryable: true });
  api.deleteMeeting.mockResolvedValueOnce({
    identity,
    data: { entity_id: "first", revision: "deleted", change_sequence: 2, job_id: "purge", dedup_until: "2026-11-02T01:00:00Z" },
  });
  await connected();
  await selectFirst();
  fireEvent.click(screen.getByRole("button", { name: "Delete meeting" }));
  expect(api.deleteMeeting).not.toHaveBeenCalled();
  const confirm = screen.getByRole("button", { name: "Confirm delete" });
  fireEvent.click(confirm);
  await screen.findByText(/Reply timed out/);
  const first = api.deleteMeeting.mock.calls[0][0];
  expect(first).toMatchObject({ meeting_id: "first", expected_lifecycle_revision: "life-1" });
  expect(first.idempotency_key).toMatch(/^[\w-]{36}$/);
  fireEvent.click(confirm);
  await waitFor(() => expect(screen.queryByLabelText("Title")).not.toBeInTheDocument());
  expect(api.deleteMeeting.mock.calls[1][0]).toEqual(first);
});

it("drops old epoch responses immediately on reconnect", async () => {
  const oldPage = deferred<TranscriptPage>();
  const reconnect = deferred<RepositoryIdentity>();
  await connected(); await selectFirst();
  api.transcript.mockReturnValueOnce(oldPage.promise);
  fireEvent.click(screen.getByRole("button", { name: "Read transcript" }));
  api.reconnect.mockReturnValueOnce(reconnect.promise);
  fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
  expect(screen.queryByLabelText("Title")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "First fixture" })).not.toBeInTheDocument();
  await act(async () => oldPage.resolve(transcript("Old epoch text")));
  const nextIdentity = { ...identity, library_epoch: "epoch-2" };
  api.bootstrap.mockResolvedValueOnce({ identity: nextIdentity, data: { preferences, change_sequence: 2, cursor_scope: "scope" } });
  await act(async () => reconnect.resolve(nextIdentity));
  await screen.findByText(/epoch epoch-2/);
  expect(screen.queryByText("Old epoch text")).not.toBeInTheDocument();
  expect(api.meetings).toHaveBeenCalledTimes(1);
});

it("does not apply a late preference mutation after reconnect", async () => {
  const save = deferred<{ identity: RepositoryIdentity; data: { entity_id: string; revision: string; change_sequence: number; job_id: null; dedup_until: string } }>();
  await connected();
  api.updatePreferences.mockReturnValueOnce(save.promise);
  fireEvent.change(screen.getByLabelText("Fixture hotkey"), { target: { value: "Ctrl+Shift+T" } });
  fireEvent.click(screen.getByRole("button", { name: "Save preferences" }));
  const nextIdentity = { ...identity, library_epoch: "epoch-2" };
  api.reconnect.mockResolvedValueOnce(nextIdentity);
  api.bootstrap.mockResolvedValueOnce({ identity: nextIdentity, data: { preferences: { ...preferences, revision: "prefs-current", hotkey: "Ctrl+Shift+P" }, change_sequence: 2, cursor_scope: "scope" } });
  fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
  await screen.findByDisplayValue("Ctrl+Shift+P");
  await act(async () => save.resolve({ identity, data: { entity_id: "preferences", revision: "prefs-stale", change_sequence: 2, job_id: null, dedup_until: "2026-11-02T01:00:00Z" } }));
  expect(screen.getByLabelText("Fixture hotkey")).toHaveValue("Ctrl+Shift+P");
  expect(screen.queryByText(/prefs-stale/)).not.toBeInTheDocument();
  expect(api.updatePreferences).toHaveBeenCalledTimes(1);
});

it("invalidates displayed snapshots on changes and advances the change cursor only explicitly", async () => {
  const oldPage = deferred<TranscriptPage>();
  const cursor = { library_id: identity.library_id, library_epoch: identity.library_epoch, cursor: { library_scope: "scope", after: 3 } };
  api.changes.mockResolvedValueOnce({ identity, items: [{ sequence: 3, entity_kind: "meeting", entity_id: "first", revision: "metadata-new", actor: "mcp" }], next: cursor, has_more: true, current_sequence: 4 });
  await connected(); await selectFirst();
  api.transcript.mockReturnValueOnce(oldPage.promise);
  fireEvent.click(screen.getByRole("button", { name: "Read transcript" }));
  fireEvent.click(screen.getByRole("button", { name: "Read changes" }));
  await screen.findByText(/metadata-new/);
  expect(screen.queryByLabelText("Title")).not.toBeInTheDocument();
  expect(screen.queryByLabelText("Fixture locale")).not.toBeInTheDocument();
  expect(api.changes).toHaveBeenCalledTimes(1);
  await act(async () => oldPage.resolve(transcript("Content from before changes")));
  expect(screen.queryByText("Content from before changes")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Next change page" }));
  await waitFor(() => expect(api.changes).toHaveBeenCalledTimes(2));
  expect(api.changes.mock.calls[1][0]).toEqual({ cursor, limit: 20, max_bytes: 16384 });
});

it("keeps structured JSON as bounded fragments and invalidates rejected cursors", async () => {
  const first = summary('{"partial":');
  first.next = { library_id: identity.library_id, library_epoch: identity.library_epoch, cursor: { library_scope: "scope", meeting_id: "first", revision: "summary-first", field: "structured_json", byte_offset: 11, query_hash: "hash" } };
  api.summary.mockResolvedValueOnce(first).mockResolvedValueOnce(summary('"next"}', 11));
  await connected(); await selectFirst();
  fireEvent.change(screen.getByLabelText("Field"), { target: { value: "structured_json" } });
  fireEvent.click(screen.getByRole("button", { name: "Read summary" }));
  await screen.findByText('{"partial":');
  expect(api.summary).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Next summary page" }));
  await screen.findByText('"next"}');
  expect(within(screen.getByTestId("summary-page")).queryByText('{"partial":')).not.toBeInTheDocument();
  expect(api.summary.mock.calls[1][0]).toMatchObject({ expected_revision: "summary-first", cursor: first.next });
  api.meetings.mockRejectedValueOnce({ code: "CURSOR_STALE", message: "The snapshot changed.", retryable: false });
  fireEvent.click(screen.getByRole("button", { name: "Read meetings" }));
  await screen.findByRole("alert");
  expect(screen.queryByTestId("summary-page")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Next meeting page" })).toBeDisabled();
});

it("rejects a reply from a different library instead of rendering its data", async () => {
  await connected();
  api.meetings.mockResolvedValueOnce({ identity: { ...identity, library_id: "another-library" }, items: [meeting("foreign", "Foreign meeting")], next: null, snapshot_sequence: 1 });
  fireEvent.click(screen.getByRole("button", { name: "Read meetings" }));
  await screen.findByRole("alert");
  expect(screen.queryByRole("button", { name: "Foreign meeting" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Read meetings" })).toBeDisabled();
});

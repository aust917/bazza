import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Library } from "./Library";

const backend = vi.hoisted(() => ({ listMeetingsFiltered: vi.fn(), listFolders: vi.fn(), listTags: vi.fn(), listUpcoming: vi.fn(), search: vi.fn() }));
const listeners = vi.hoisted(() => new Map<string, Set<(payload: unknown) => void>>());
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: (name: string, handler: (payload: unknown) => void) => {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler); listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));

function emit(stage: string, progress = 0) {
  listeners.get("processing")?.forEach((handler) => handler({ meeting_id: "first", stage, progress, error: null }));
}
function renderLibrary() { return render(<MemoryRouter><Library /></MemoryRouter>); }

beforeEach(() => {
  vi.useFakeTimers(); vi.clearAllMocks(); listeners.clear();
  backend.listMeetingsFiltered.mockResolvedValue([]);
  backend.listFolders.mockResolvedValue([]);
  backend.listTags.mockResolvedValue([]);
  backend.listUpcoming.mockResolvedValue([]);
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

it("coalesces stage changes and skips percent-only events and repeated metadata queries", async () => {
  renderLibrary();
  await act(async () => {});
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(1);
  act(() => { for (let i = 0; i < 20; i++) emit("cleanup", i / 20); });
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(2);
  act(() => { emit("cleanup", 1); emit("title"); emit("done", 1); });
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(3);
  expect(backend.listFolders).toHaveBeenCalledTimes(1);
  expect(backend.listTags).toHaveBeenCalledTimes(1);
  act(() => listeners.get("recording_state")?.forEach((handler) => handler({ state: "recording", meeting_id: "second", elapsed_ms: 0 })));
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(4);
  expect(backend.listFolders).toHaveBeenCalledTimes(2);
});

it("waits for an outstanding refresh, then fetches the final state once", async () => {
  let resolve!: (rows: []) => void;
  backend.listMeetingsFiltered.mockReturnValueOnce(new Promise((done) => { resolve = done; }));
  const { unmount } = renderLibrary();
  act(() => { emit("cleanup"); emit("done", 1); });
  await act(() => vi.advanceTimersByTimeAsync(1000));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(1);
  await act(async () => { resolve([]); });
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(2);
  act(() => emit("transcribe"));
  unmount();
  await act(() => vi.advanceTimersByTimeAsync(1000));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(2);
});

it("does not apply a stale search response after the query is cleared", async () => {
  let resolve!: (rows: unknown[]) => void;
  backend.search.mockReturnValue(new Promise((done) => { resolve = done; }));
  renderLibrary();
  await act(async () => {});
  const input = screen.getByPlaceholderText("Search transcripts and titles");
  fireEvent.change(input, { target: { value: "old query" } });
  await act(() => vi.advanceTimersByTimeAsync(120));
  fireEvent.change(input, { target: { value: "" } });
  await act(async () => { resolve([{ meeting_id: "old", segment_id: 1, start_ms: 0, meeting_title: "Stale result", snippet: "old" }]); });
  expect(screen.queryByText("Stale result")).not.toBeInTheDocument();
});

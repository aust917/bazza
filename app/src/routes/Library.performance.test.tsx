import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Meeting, SearchHit } from "@/lib/ipc";
import { Library } from "./Library";

const backend = vi.hoisted(() => ({
  listMeetingsFiltered: vi.fn(), listFolders: vi.fn(), listTags: vi.fn(), listUpcoming: vi.fn(), search: vi.fn(),
}));
const listeners = vi.hoisted(() => new Map<string, Set<(payload: unknown) => void>>());
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: (name: string, handler: (payload: unknown) => void) => {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler);
  listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function emit(name: string, payload: unknown) {
  listeners.get(name)?.forEach((listener) => listener(payload));
}

function processingBurst(stage: string) {
  for (let meeting = 0; meeting < 40; meeting++) {
    for (let percent = 0; percent <= 100; percent++) {
      emit("processing", { meeting_id: `meeting-${meeting}`, stage, progress: percent / 100, error: null });
    }
  }
}

function row(title: string): Meeting {
  return {
    id: "meeting-0", title, app: "import", started_at: "2026-11-02T12:00:00Z", ended_at: null,
    duration_ms: 60_000, has_video: false, status: "ready", folder_id: null,
    created_at: "2026-11-02T12:00:00Z", error: null, title_auto: false, participants: [], calendar_uid: null,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  listeners.clear();
  backend.listMeetingsFiltered.mockResolvedValue([row("Initial recording")]);
  backend.listFolders.mockResolvedValue([]);
  backend.listTags.mockResolvedValue([]);
  backend.listUpcoming.mockResolvedValue([]);
  backend.search.mockResolvedValue([]);
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

it("bounds backend work during thousands of multi-meeting events and eventually displays the final state", async () => {
  render(<MemoryRouter><Library /></MemoryRouter>);
  await act(async () => {});
  expect(screen.getByText("Initial recording")).toBeInTheDocument();

  const first = deferred<Meeting[]>();
  const final = deferred<Meeting[]>();
  let inFlight = 0;
  let peakInFlight = 0;
  const request = (pending: ReturnType<typeof deferred<Meeting[]>>) => {
    inFlight++;
    peakInFlight = Math.max(peakInFlight, inFlight);
    return pending.promise.finally(() => { inFlight--; });
  };
  backend.listMeetingsFiltered
    .mockImplementationOnce(() => request(first))
    .mockImplementationOnce(() => request(final));

  act(() => processingBurst("transcribe"));
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(2);
  act(() => {
    processingBurst("cleanup");
    processingBurst("done");
    for (let i = 0; i < 500; i++) emit("recording_state", { state: "idle" });
  });
  await act(() => vi.advanceTimersByTimeAsync(5_000));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(2);
  expect(backend.listFolders).toHaveBeenCalledTimes(1);
  expect(backend.listTags).toHaveBeenCalledTimes(1);
  expect(screen.getByText("Initial recording")).toBeInTheDocument();

  await act(async () => { first.resolve([row("Intermediate recording")]); });
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(3);
  expect(peakInFlight).toBe(1);
  expect(backend.listFolders).toHaveBeenCalledTimes(2);
  expect(backend.listTags).toHaveBeenCalledTimes(2);
  await act(async () => { final.resolve([row("Final recording")]); });
  expect(screen.getByText("Final recording")).toBeInTheDocument();
  expect(screen.queryByText("Intermediate recording")).not.toBeInTheDocument();

  // Last-seen stages are tracked per meeting; percent updates should stay silent indefinitely.
  act(() => processingBurst("done"));
  await act(() => vi.advanceTimersByTimeAsync(5_000));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(3);
  expect(inFlight).toBe(0);
});

it("recovers from a slow failed refresh once and releases pending event work on unmount", async () => {
  const { unmount } = render(<MemoryRouter><Library /></MemoryRouter>);
  await act(async () => {});
  const failed = deferred<Meeting[]>();
  backend.listMeetingsFiltered.mockReturnValueOnce(failed.promise);
  act(() => processingBurst("transcribe"));
  await act(() => vi.advanceTimersByTimeAsync(100));
  act(() => processingBurst("done"));
  await act(() => vi.advanceTimersByTimeAsync(2_000));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(2);
  expect(screen.getByText("Initial recording")).toBeInTheDocument();
  await act(async () => { failed.reject(new Error("Temporary database contention")); });
  expect(screen.getByText("Initial recording")).toBeInTheDocument();
  backend.listMeetingsFiltered.mockResolvedValueOnce([row("Recovered recording")]);
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(3);
  expect(screen.getByText("Recovered recording")).toBeInTheDocument();

  act(() => processingBurst("cleanup"));
  unmount();
  await act(() => vi.advanceTimersByTimeAsync(120_000));
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(3);
  expect(backend.listUpcoming).toHaveBeenCalledTimes(1);
  expect(listeners.get("processing")?.size).toBe(0);
  expect(listeners.get("recording_state")?.size).toBe(0);
  expect(vi.getTimerCount()).toBe(0);
});

it("debounces a typing burst into one search and ignores an older slow result", async () => {
  render(<MemoryRouter><Library /></MemoryRouter>);
  await act(async () => {});
  const stale = deferred<SearchHit[]>();
  const latest = deferred<SearchHit[]>();
  backend.search.mockReturnValueOnce(stale.promise).mockReturnValueOnce(latest.promise);
  const input = screen.getByPlaceholderText("Search transcripts and titles");
  const query = "decisions from the product planning meeting";
  for (let length = 1; length <= query.length; length++) {
    fireEvent.change(input, { target: { value: query.slice(0, length) } });
    await act(() => vi.advanceTimersByTimeAsync(20));
  }
  expect(backend.search).not.toHaveBeenCalled();
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(backend.search).toHaveBeenCalledExactlyOnceWith(query);

  fireEvent.change(input, { target: { value: `${query} final` } });
  await act(() => vi.advanceTimersByTimeAsync(120));
  await act(async () => { latest.resolve([{
    meeting_id: "latest", meeting_title: "Latest search result", segment_id: 1, start_ms: 0, snippet: "final",
  }]); });
  await act(async () => { stale.resolve([{
    meeting_id: "stale", meeting_title: "Stale search result", segment_id: 2, start_ms: 0, snippet: "older",
  }]); });
  expect(screen.getByText("Latest search result")).toBeInTheDocument();
  expect(screen.queryByText("Stale search result")).not.toBeInTheDocument();
  expect(backend.search).toHaveBeenCalledTimes(2);
  expect(backend.listMeetingsFiltered).toHaveBeenCalledTimes(1);
  expect(backend.listFolders).toHaveBeenCalledTimes(1);
  expect(backend.listTags).toHaveBeenCalledTimes(1);
});

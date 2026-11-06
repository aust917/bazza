import { act, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { expect, it, vi } from "vitest";
import type { Events, RecordingState } from "@/lib/ipc";
import { Shell } from "./Shell";

const api = vi.hoisted(() => ({
  recordingStatus: vi.fn(),
  stopRecording: vi.fn(),
  getSettings: vi.fn(),
  listeners: {} as { [K in keyof Events]?: (payload: Events[K]) => void },
}));

vi.mock("@/lib/ipc", () => ({
  cmd: api,
  subscribe: <K extends keyof Events>(event: K, listener: (typeof api.listeners)[K]) => {
    api.listeners[event] = listener;
    return () => { delete api.listeners[event]; };
  },
}));
vi.mock("./UpdateBanner", () => ({ UpdateBanner: () => null }));

it("reconciles the sidebar after missed events and ignores an older recording read", async () => {
  vi.useFakeTimers();
  try {
    const idle: RecordingState = { state: "idle", meeting_id: null, elapsed_ms: 0 };
    const recording: RecordingState = { state: "recording", meeting_id: "test-meeting", elapsed_ms: 454_000 };
    let finishOlderRead!: (status: RecordingState) => void;
    api.getSettings.mockResolvedValue({ hotkey: "CmdOrCtrl+Shift+R" });
    api.recordingStatus.mockResolvedValueOnce(recording)
      .mockImplementationOnce(() => new Promise<RecordingState>((resolve) => { finishOlderRead = resolve; }));

    await act(async () => {
      render(<MemoryRouter><Routes><Route path="/" element={<Shell />} /></Routes></MemoryRouter>);
    });
    expect(screen.getByRole("button", { name: "Stop" })).toBeVisible();

    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    act(() => api.listeners.recording_state?.(idle));
    await act(async () => { finishOlderRead(recording); });
    expect(screen.getByText("Ready when you are.")).toBeVisible();

    // The next recording starts without a native event.
    api.recordingStatus.mockResolvedValue(recording);
    await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
    expect(screen.getByRole("button", { name: "Stop" })).toBeVisible();

    // The record bar stops it while the main window is out of focus.
    api.recordingStatus.mockResolvedValue(idle);
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByText("Ready when you are.")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
  } finally {
    vi.useRealTimers();
  }
});

it("keeps real progress separate for each active model and removes terminal downloads", async () => {
  api.getSettings.mockResolvedValue({ hotkey: "CmdOrCtrl+Shift+R" });
  api.recordingStatus.mockResolvedValue({ state: "idle", meeting_id: null, elapsed_ms: 0 });
  await act(async () => {
    render(<MemoryRouter><Routes><Route path="/" element={<Shell />} /></Routes></MemoryRouter>);
  });
  act(() => {
    api.listeners.model_progress?.({ id: "whisper-small", done: 512, total: 1024, status: "downloading", error: null });
    api.listeners.model_progress?.({ id: "titanet-small", done: 256, total: 1024, status: "downloading", error: null });
  });
  expect(screen.getByRole("progressbar", { name: /whisper-small/ })).toHaveAttribute("aria-valuenow", "50");
  expect(screen.getByRole("progressbar", { name: /titanet-small/ })).toHaveAttribute("aria-valuenow", "25");
  expect(screen.getByText(/512 B.*1\.0 KB/)).toBeVisible();
  expect(screen.getByRole("link", { name: "Downloading 2 models" })).toHaveAttribute("href", "/settings");
  expect(screen.getByRole("button", { name: "Record now" })).toBeEnabled();

  act(() => api.listeners.model_progress?.({ id: "whisper-small", done: 1024, total: 1024, status: "done", error: null }));
  expect(screen.queryByRole("progressbar", { name: /whisper-small/ })).not.toBeInTheDocument();
  expect(screen.getByRole("progressbar", { name: /titanet-small/ })).toHaveAttribute("aria-valuenow", "25");
  act(() => api.listeners.model_progress?.({ id: "titanet-small", done: 0, total: 1024, status: "failed", error: "offline" }));
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
});

it("shows indeterminate progress while saving from the main window", async () => {
  let finishStop!: () => void;
  api.stopRecording.mockReturnValueOnce(new Promise<void>((resolve) => { finishStop = resolve; }));
  api.getSettings.mockResolvedValue({ hotkey: "CmdOrCtrl+Shift+R" });
  api.recordingStatus.mockResolvedValue({ state: "recording", meeting_id: "test-meeting", elapsed_ms: 1000 });
  await act(async () => {
    render(<MemoryRouter><Routes><Route path="/" element={<Shell />} /></Routes></MemoryRouter>);
  });
  const stop = screen.getByRole("button", { name: "Stop" });
  fireEvent.click(stop);
  expect(stop).toBeDisabled();
  expect(screen.getByRole("progressbar", { name: "Saving recording" })).not.toHaveAttribute("aria-valuenow");
  await act(async () => {
    api.listeners.recording_state?.({ state: "idle", meeting_id: null, elapsed_ms: 0 });
    finishStop();
  });
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  expect(screen.getByText("Ready when you are.")).toBeVisible();
});

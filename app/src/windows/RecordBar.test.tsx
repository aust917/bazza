import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RecordBar } from "./RecordBar";

const listeners = vi.hoisted(() => new Map<string, (payload: unknown) => void>());
vi.mock("@/lib/ipc", () => ({
  cmd: { recordingStatus: async () => ({ state: "recording", meeting_id: "m1", elapsed_ms: 1000 }), openMain: vi.fn() },
  subscribe: (name: string, callback: (payload: unknown) => void) => {
    listeners.set(name, callback);
    return () => { listeners.delete(name); };
  },
}));
beforeEach(() => listeners.clear());
afterEach(() => cleanup());

describe("recording bar source feedback", () => {
  it("labels mic/system levels and keeps recorder warnings visible", async () => {
    await act(async () => { render(<RecordBar />); });
    expect(screen.getByText("Mic")).toBeInTheDocument();
    expect(screen.getByText("Sys")).toBeInTheDocument();
    act(() => listeners.get("notice")?.({ level: "warning", message: "Audio device lost. Stop to keep captured audio." }));
    expect(screen.getByRole("alert")).toHaveTextContent("Audio device lost");
    act(() => listeners.get("notice")?.({ level: "info", message: "Other background activity" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Audio device lost");
    act(() => listeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 0 }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows the paused state even when a caption was already received", async () => {
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("caption")?.({ start_ms: 0, end_ms: 1000, text: "Previous caption", is_final: true }));
    act(() => listeners.get("recording_state")?.({ state: "paused", meeting_id: "m1", elapsed_ms: 1000 }));
    expect(screen.getByText("Paused")).toBeInTheDocument();
    expect(screen.queryByText("Previous caption")).not.toBeInTheDocument();
  });
});

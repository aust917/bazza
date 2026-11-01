import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Events, Settings, VideoSource } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import { Popup } from "./Popup";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), setSettings: vi.fn(), listAudioDevices: vi.fn(),
  listVideoSources: vi.fn(), startRecording: vi.fn(), dismissDetection: vi.fn(),
}));
const listeners = vi.hoisted(() => new Map<string, (payload: unknown) => void>());
vi.mock("@/lib/ipc", () => ({
  cmd: backend,
  subscribe: (name: string, callback: (payload: unknown) => void) => {
    listeners.set(name, callback);
    return () => { listeners.delete(name); };
  },
}));

const sources: VideoSource[] = [
  { target: { kind: "window", title: "Call" }, label: "Call", is_meeting: true, pid: 42 },
  { target: { kind: "monitor", index: 0 }, label: "Display 1", is_meeting: false, pid: null },
];
let settings: Settings;

async function openPicker() {
  act(() => listeners.get("detection")?.({ app: "manual", label: "Record now", title: "Choose sources", event: null, confidence: 1 } satisfies Events["detection"]));
  await waitFor(() => expect(screen.getByRole("button", { name: "Record" })).toBeEnabled());
}

beforeEach(async () => {
  vi.clearAllMocks();
  listeners.clear();
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Windows");
  settings = { ...await mockInvoke<Settings>("get_settings"), audio_scope: "app", capture_system: true };
  backend.getSettings.mockImplementation(async () => ({ ...settings }));
  backend.setSettings.mockImplementation(async (next: Settings) => { settings = next; return { ...next }; });
  backend.listVideoSources.mockResolvedValue(sources);
  backend.listAudioDevices.mockResolvedValue({ inputs: [], outputs: [{ id: "speaker", name: "USB speakers", is_default: true }] });
  backend.startRecording.mockResolvedValue({ id: "meeting" });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("recording picker audio scope", () => {
  it("remembers all-app audio and Enter uses the latest choice", async () => {
    render(<Popup />);
    await openPicker();
    expect(screen.getByRole("radio", { name: "This app only" })).toBeChecked();
    fireEvent.click(screen.getByRole("radio", { name: "All apps on output" }));
    await waitFor(() => expect(backend.setSettings).toHaveBeenCalledWith(expect.objectContaining({ audio_scope: "all" })));
    await waitFor(() => expect(screen.getByRole("button", { name: "Record" })).toBeEnabled());
    expect(screen.getByText(/Output: USB speakers/)).toHaveTextContent("Audio on other outputs is not included");
    fireEvent.keyDown(window, { key: "Enter" });
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ audio_pid: undefined })));
    await openPicker();
    expect(screen.getByRole("radio", { name: "All apps on output" })).toBeChecked();
  });

  it("keeps per-app audio in audio-only mode", async () => {
    render(<Popup />);
    await openPicker();
    fireEvent.click(screen.getByRole("button", { name: "Record screen video" }));
    fireEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ audio_pid: 42, video: false, target: undefined })));
  });

  it("falls back to the selected output for a monitor without changing the saved app preference", async () => {
    settings.loopback_device = "speaker";
    render(<Popup />);
    await openPicker();
    fireEvent.change(screen.getByLabelText("Screen to record"), { target: { value: "m:0" } });
    expect(screen.getByRole("radio", { name: "This app only" })).toBeDisabled();
    expect(screen.getByRole("radio", { name: "All apps on output" })).toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ audio_pid: undefined })));
    expect(backend.setSettings).not.toHaveBeenCalled();
  });

  it("makes microphone-only settings explicit and never passes an app PID", async () => {
    settings.capture_system = false;
    render(<Popup />);
    await openPicker();
    expect(screen.getByText(/Microphone only/)).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "This app only" })).toBeDisabled();
    expect(screen.getByRole("radio", { name: "All apps on output" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ audio_pid: undefined })));
  });

  it("rolls back a choice that could not be saved", async () => {
    backend.setSettings.mockRejectedValueOnce(new Error("Storage unavailable"));
    render(<Popup />);
    await openPicker();
    fireEvent.click(screen.getByRole("radio", { name: "All apps on output" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not save audio choice");
    expect(screen.getByRole("radio", { name: "This app only" })).toBeChecked();
  });

  it("uses all-app capture on platforms without process audio", async () => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Macintosh");
    render(<Popup />);
    await openPicker();
    expect(screen.getByRole("radio", { name: "This app only" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ audio_pid: undefined })));
  });
});

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RecordBar } from "./RecordBar";
import i18n from "@/lib/i18n";

const listeners = vi.hoisted(() => new Map<string, (payload: unknown) => void>());
const stopRecording = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
const pauseRecording = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
const openMain = vi.hoisted(() => vi.fn());
const liveSnapshot = vi.hoisted(() => vi.fn());
vi.mock("@/lib/ipc", () => ({
  cmd: { recordingStatus: async () => ({ state: "recording", meeting_id: "m1", elapsed_ms: 1000 }), discordRecordingStatus: async () => null, attendeeRecordingStatus: async () => null, liveSnapshot, openMain, pauseRecording, stopRecording },
  on: async (name: string, callback: (payload: unknown) => void) => {
    listeners.set(name, callback);
    return () => { listeners.delete(name); };
  },
  subscribe: (name: string, callback: (payload: unknown) => void) => {
    listeners.set(name, callback);
    return () => { listeners.delete(name); };
  },
}));
beforeEach(async () => {
  listeners.clear();
  vi.clearAllMocks();
  liveSnapshot.mockReset().mockResolvedValue(null);
  await i18n.changeLanguage("en");
});
afterEach(() => cleanup());

describe("recording bar source feedback", () => {
  it("shows overlapping named captions and clears only the matching speaker", async () => {
    await act(async () => { render(<RecordBar />); });
    const alice = { meeting_id: "m1", speaker_id: "a", speaker_name: "Alice", start_ms: 10, end_ms: 30, is_final: false };
    const bob = { meeting_id: "m1", speaker_id: "b", speaker_name: "Bob", start_ms: 20, end_ms: 40, is_final: false };
    act(() => listeners.get("caption")?.({ ...alice, text: "First draft" }));
    act(() => listeners.get("caption")?.({ ...bob, text: "Second draft" }));
    expect(screen.getByText("Alice").parentElement).toHaveTextContent("First draft");
    expect(screen.getByText("Bob").parentElement).toHaveTextContent("Second draft");

    act(() => listeners.get("caption")?.({ ...alice, text: "First final", is_final: true }));
    act(() => listeners.get("caption")?.({ ...alice, meeting_id: "other", text: "Other meeting" }));
    expect(screen.queryByText("First draft")).not.toBeInTheDocument();
    expect(screen.getByText("First final")).toBeVisible();
    expect(screen.getByText("Second draft")).toBeVisible();
    expect(screen.queryByText("Other meeting")).not.toBeInTheDocument();

    act(() => listeners.get("caption")?.({ ...bob, text: "", is_final: true }));
    expect(screen.queryByText("Bob")).not.toBeInTheDocument();
    expect(screen.getByText("First final")).toBeVisible();
  });

  it("still replaces one unnamed draft and keeps the last two ordinary lines", async () => {
    await act(async () => { render(<RecordBar />); });
    const previous = { start_ms: 0, end_ms: 10, text: "Previous line", is_final: true };
    const draft = { start_ms: 10, end_ms: 30, text: "Plain draft", is_final: false };
    act(() => listeners.get("caption")?.(previous));
    act(() => listeners.get("caption")?.(draft));
    act(() => listeners.get("caption")?.({ ...draft, end_ms: 40, text: "Plain revised" }));
    expect(screen.queryByText("Plain draft")).not.toBeInTheDocument();
    expect(screen.getByText("Previous line")).toBeVisible();
    expect(screen.getByText("Plain revised")).toBeVisible();
    act(() => listeners.get("caption")?.({ ...draft, end_ms: 50, text: "Plain final", is_final: true }));
    expect(screen.queryByText("Plain revised")).not.toBeInTheDocument();
    expect(screen.getByText("Previous line")).toBeVisible();
    expect(screen.getByText("Plain final")).toBeVisible();
  });

  it("shows a localized recording audio label alongside a named speaker", async () => {
    await i18n.changeLanguage("ru");
    await act(async () => { render(<RecordBar />); });
    const partial = { meeting_id: "m1", start_ms: 10, end_ms: 30, is_final: false };
    act(() => listeners.get("caption")?.({ ...partial, speaker_id: "discord:a", speaker_name: "Alice", text: "Голос участника" }));
    act(() => listeners.get("caption")?.({ ...partial, speaker_id: "local:mix", text: "Речь из демонстрации" }));
    expect(screen.getByText("Alice").parentElement).toHaveTextContent("Голос участника");
    expect(screen.getByText("Звук записи").parentElement).toHaveTextContent("Речь из демонстрации");
  });

  it.each([
    ["model_missing", "Live captions need a downloaded speech model."],
    ["model_load_failed", "Could not load the live captions model."],
  ])("shows scoped caption status %s alongside a capture warning", async (reason, label) => {
    liveSnapshot.mockResolvedValue({ meeting_id: "m1", finals: [], partial: null, notes: "", notes_updated_ms: null, caption_unavailable: reason });
    await act(async () => { render(<RecordBar />); });
    expect(screen.getByRole("status")).toHaveTextContent(label);

    const warning = { level: "warning", code: "native.captureWarning", message: "Capture timing is estimated.", detail: "Capture timing is estimated." };
    act(() => listeners.get("notice")?.(warning));
    expect(screen.getByRole("alert")).toHaveTextContent(warning.detail);
    expect(screen.getByRole("status")).toHaveTextContent(label);
    act(() => listeners.get("caption")?.({ start_ms: 0, end_ms: 1000, text: "Live caption", is_final: false }));
    expect(screen.getByText("Live caption")).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent(warning.detail);

    act(() => listeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 0 }));
    await act(async () => listeners.get("recording_state")?.({ state: "recording", meeting_id: "m2", elapsed_ms: 1000 }));
    expect(screen.queryByText(label)).not.toBeInTheDocument();
    expect(screen.getByText("Listening...")).toBeInTheDocument();
  });

  it("switches the mounted bar and an existing warning without changing recording state", async () => {
    await act(async () => { render(<RecordBar />); });
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    act(() => listeners.get("notice")?.({ level: "warning", code: "native.ffmpegMissing", message: "ffmpeg is missing from this install", args: {}, detail: null }));
    await act(async () => { await i18n.changeLanguage("ru"); });
    expect(screen.getByRole("button", { name: "Стоп" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Пауза" })).toBeInTheDocument();
    expect(screen.getByText("Микр.")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(/ffmpeg/);
    expect(screen.getByRole("alert")).not.toHaveTextContent("missing from this install");
    act(() => listeners.get("recording_state")?.({ state: "paused", meeting_id: "m1", elapsed_ms: 1000 }));
    await act(async () => { await i18n.changeLanguage("en"); });
    expect(screen.getByRole("button", { name: "Resume" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("ffmpeg is missing from this install");
  });
  it("labels mic/system levels and keeps recorder warnings visible", async () => {
    await act(async () => { render(<RecordBar />); });
    expect(screen.getByText("Mic")).toBeInTheDocument();
    expect(screen.getByText("Sys")).toBeInTheDocument();
    const detail = "Audio device lost. Stop to keep captured audio.";
    act(() => listeners.get("notice")?.({ level: "warning", code: "native.captureWarning", message: detail, detail }));
    expect(screen.getByRole("alert")).toHaveTextContent("Audio device lost");
    act(() => listeners.get("notice")?.({ level: "info", message: "Other background activity" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Audio device lost");
    act(() => listeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 0 }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps system audio loss visible until the output reconnects", async () => {
    await act(async () => { render(<RecordBar serviceDesktop />); });
    const notice = (code: string, message: string) => act(() => listeners.get("notice")?.({ level: "warning", code, message }));
    notice("native.systemAudioUnavailable", "Output unavailable");
    notice("native.captureWarning", "Capture timing is estimated.");
    expect(screen.getByRole("alert")).toHaveTextContent("System audio is unavailable");

    notice("native.systemAudioReconnected", "Output reconnected");
    expect(screen.getByRole("alert")).toHaveTextContent("Some system audio may be missing");

    act(() => listeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 0 }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it.each([
    ["native.screenStartFailed", "Screen capture could not start"],
    ["native.screenCaptureInterrupted", "The selected window closed. (exit code: 3) Audio recording continues."],
    ["native.recordingAudioOnly", null],
    ["native.captureWarning", "Screen capture stopped unexpectedly (error). Audio recording continues."],
    ["native.captureWarning", "Screen capture is unavailable: secure desktop. Audio recording continues."],
  ])("shows video loss from %s without interrupting captions", async (code, detail) => {
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("caption")?.({ start_ms: 0, end_ms: 1000, text: "First caption", is_final: true }));
    act(() => listeners.get("notice")?.({ level: "warning", code, message: detail ?? "Recording audio only.", detail }));
    act(() => listeners.get("caption")?.({ start_ms: 1000, end_ms: 2000, text: "Next caption", is_final: false }));

    const warning = screen.getByRole("button", { name: "Video unavailable. Audio recording continues." });
    if (detail) expect(warning).toHaveAttribute("title", expect.stringContaining(detail));
    fireEvent.click(warning);
    expect(openMain).toHaveBeenCalledTimes(1);
    expect(screen.getByText("Next caption")).toBeVisible();

    await act(async () => { await i18n.changeLanguage("ru"); });
    expect(screen.getByRole("alert")).toHaveTextContent("Видео недоступно. Запись звука продолжается.");
    for (const state of ["paused", "finishing", "pending"]) {
      act(() => listeners.get("recording_state")?.({ state, meeting_id: "m1", elapsed_ms: 2000 }));
      expect(screen.getByRole("alert")).toHaveTextContent(/^Видео недоступно\.$/);
      expect(screen.getByRole("button", { name: "Видео недоступно." })).toHaveAttribute("title", "Видео недоступно.");
    }
  });

  it("keeps captions visible when a recording action fails", async () => {
    pauseRecording.mockRejectedValueOnce(new Error("Unable to pause"));
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("caption")?.({ start_ms: 0, end_ms: 1000, text: "Live caption", is_final: false }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Pause" })));

    expect(screen.getByRole("alert")).toHaveTextContent("Unable to pause");
    expect(screen.getByText("Live caption")).toBeVisible();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
  });

  it("shows the paused state even when a caption was already received", async () => {
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("caption")?.({ start_ms: 0, end_ms: 1000, text: "Previous caption", is_final: true }));
    act(() => listeners.get("recording_state")?.({ state: "paused", meeting_id: "m1", elapsed_ms: 1000 }));
    expect(screen.getByText("Paused")).toBeInTheDocument();
    expect(screen.queryByText("Previous caption")).not.toBeInTheDocument();
  });

  it("retries a pending service commit through the normal Stop command", async () => {
    await act(async () => { render(<RecordBar serviceDesktop />); });
    act(() => listeners.get("recording_state")?.({ state: "pending", meeting_id: "m1", elapsed_ms: 1000 }));
    expect(screen.getByRole("button", { name: "Pause" })).toBeDisabled();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Retry saving" })));
    expect(stopRecording).toHaveBeenCalledTimes(1);
  });

  it("shows indeterminate saving progress while Stop is pending and permits retry on failure", async () => {
    let rejectStop!: (error: Error) => void;
    stopRecording.mockReturnValueOnce(new Promise((_resolve, reject) => { rejectStop = reject; }));
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("caption")?.({ start_ms: 0, end_ms: 1000, text: "Last caption", is_final: true }));
    const stop = screen.getByRole("button", { name: "Stop" });
    fireEvent.click(stop);
    expect(screen.getByRole("progressbar", { name: "Saving the recording" })).not.toHaveAttribute("aria-valuenow");
    expect(screen.getByText("Last caption")).toBeVisible();
    expect(stop).toBeDisabled();
    fireEvent.click(stop);
    expect(stopRecording).toHaveBeenCalledTimes(1);
    await act(async () => { rejectStop(new Error("Unable to save")); });
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Unable to save");
    expect(stop).toBeEnabled();
  });

  it("shows the Discord session state for the current recording only", async () => {
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("discord_recording_status")?.({ meeting_id: "m1", state: "connected" }));
    expect(screen.getByRole("status")).toHaveTextContent("waiting for sound");
    act(() => listeners.get("discord_recording_status")?.({ meeting_id: "old-meeting", state: "unavailable" }));
    expect(screen.getByRole("status")).toHaveTextContent("waiting for sound");
    act(() => listeners.get("discord_recording_status")?.({ meeting_id: "m1", state: "receiving" }));
    expect(screen.getByRole("status")).toHaveTextContent("Receiving Discord audio");
    act(() => listeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 0 }));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    expect(listeners.has("discord_recording_status")).toBe(false);
  });

  it("shows Google Meet recording status for the active meeting", async () => {
    await act(async () => { render(<RecordBar />); });
    act(() => listeners.get("attendee_recording_status")?.({ meetingId: "m1", phase: "waiting", message: null }));
    expect(screen.getByRole("status")).toHaveTextContent("check bot admission");
    act(() => listeners.get("attendee_recording_status")?.({ meetingId: "old", phase: "failed", message: null }));
    expect(screen.getByRole("status")).toHaveTextContent("check bot admission");
    act(() => listeners.get("attendee_recording_status")?.({ meetingId: "m1", phase: "receiving", message: null }));
    expect(screen.getByRole("status")).toHaveTextContent("Recording separate Google Meet tracks");
    act(() => listeners.get("recording_state")?.({ state: "idle", meeting_id: null, elapsed_ms: 0 }));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    expect(listeners.has("attendee_recording_status")).toBe(false);
  });
});

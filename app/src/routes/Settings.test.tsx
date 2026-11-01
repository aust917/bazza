import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Settings } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import { SettingsPage } from "./Settings";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), setSettings: vi.fn(), listAudioDevices: vi.fn(), probeHardware: vi.fn(),
  dataInfo: vi.fn(), listShares: vi.fn(), listModels: vi.fn(), recordingStatus: vi.fn(),
  sonioxKeyStatus: vi.fn(), saveSonioxKey: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: () => () => {} }));
vi.mock("@/components/TemplatesEditor", () => ({ TemplatesEditor: () => null }));
let settings: Settings;

beforeEach(async () => {
  vi.clearAllMocks();
  settings = { ...await mockInvoke<Settings>("get_settings"), video_fps: 15, video_max_height: 1080 };
  backend.getSettings.mockResolvedValue(settings);
  backend.setSettings.mockImplementation(async (next: Settings) => next);
  backend.listAudioDevices.mockResolvedValue({ inputs: [], outputs: [] });
  backend.probeHardware.mockResolvedValue(await mockInvoke("probe_hardware"));
  backend.dataInfo.mockResolvedValue(await mockInvoke("data_info"));
  backend.listShares.mockResolvedValue([]);
  backend.listModels.mockResolvedValue([]);
  backend.sonioxKeyStatus.mockResolvedValue(false);
  backend.saveSonioxKey.mockResolvedValue(true);
  backend.recordingStatus.mockResolvedValue({ state: "recording", meeting_id: "m1", elapsed_ms: 0 });
});
afterEach(() => cleanup());

describe("capture settings", () => {
  it("requires explicit cloud consent and keeps the API key out of ordinary settings", async () => {
    render(<SettingsPage />);
    const provider = await screen.findByLabelText("Transcription provider");
    expect(provider).toHaveValue("local");
    expect(screen.getByRole("option", { name: /Soniox cloud/ })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Soniox API key"), { target: { value: "private-key" } });
    fireEvent.click(screen.getByRole("button", { name: "Save key" }));
    await waitFor(() => expect(backend.saveSonioxKey).toHaveBeenCalledWith("private-key"));
    expect(backend.setSettings).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByLabelText("Soniox API key")).toHaveValue(""));
  });
  it("preserves defaults and saves the selected quality for the next recording", async () => {
    render(<SettingsPage />);
    const fps = await screen.findByLabelText("Frame rate");
    expect(fps).toHaveValue("15");
    expect(screen.getByLabelText("Maximum height")).toHaveValue("1080");
    fireEvent.change(fps, { target: { value: "30" } });
    await waitFor(() => expect(backend.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({ video_fps: 30, video_max_height: 1080 })));
    await screen.findByText("Saved");
    fireEvent.change(screen.getByLabelText("Maximum height"), { target: { value: "720" } });
    await waitFor(() => expect(backend.setSettings).toHaveBeenLastCalledWith(expect.objectContaining({ video_fps: 30, video_max_height: 720 })));
  });

  it("retains valid custom values and rolls back rejected changes", async () => {
    settings.video_fps = 12;
    settings.video_max_height = 900;
    backend.setSettings.mockRejectedValueOnce(new Error("Could not save settings"));
    render(<SettingsPage />);
    const fps = await screen.findByLabelText("Frame rate");
    expect(fps).toHaveValue("12");
    expect(screen.getByLabelText("Maximum height")).toHaveValue("900");
    fireEvent.change(fps, { target: { value: "30" } });
    await screen.findByText("Error: Could not save settings");
    expect(fps).toHaveValue("12");
    expect(screen.getByLabelText("Maximum height")).toHaveValue("900");
  });
});

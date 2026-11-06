import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Settings } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import { SettingsPage } from "./Settings";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), setSettings: vi.fn(), listAudioDevices: vi.fn(), probeHardware: vi.fn(),
  dataInfo: vi.fn(), listShares: vi.fn(), listModels: vi.fn(), recordingStatus: vi.fn(),
  sonioxKeyStatus: vi.fn(), saveSonioxKey: vi.fn(), setUiLocale: vi.fn(), sampleLevels: vi.fn(),
  discordStatus: vi.fn(), attendeeStatus: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: () => () => {} }));
vi.mock("@/components/TemplatesEditor", () => ({ TemplatesEditor: () => null }));
let settings: Settings;

beforeEach(async () => {
  vi.resetAllMocks();
  settings = { ...await mockInvoke<Settings>("get_settings"), stt_provider: "local", soniox_consent: false };
  backend.getSettings.mockImplementation(async () => settings);
  backend.setSettings.mockImplementation(async (next: Settings) => next);
  backend.listAudioDevices.mockResolvedValue({ inputs: [], outputs: [] });
  backend.probeHardware.mockResolvedValue(await mockInvoke("probe_hardware"));
  backend.dataInfo.mockResolvedValue(await mockInvoke("data_info"));
  backend.listShares.mockResolvedValue([]);
  backend.listModels.mockResolvedValue([]);
  backend.sonioxKeyStatus.mockResolvedValue(false);
  backend.discordStatus.mockResolvedValue({ bot: null, channel: null });
  backend.attendeeStatus.mockResolvedValue(await mockInvoke("attendee_status"));
  backend.saveSonioxKey.mockResolvedValue(true);
  backend.recordingStatus.mockResolvedValue({ state: "recording", meeting_id: "meeting", elapsed_ms: 0 });
  backend.setUiLocale.mockImplementation(async (locale: "ru" | "en") => locale);
});
afterEach(() => cleanup());

it("revokes cloud permission and returns to Local while the setting is being persisted", async () => {
  settings = { ...settings, stt_provider: "soniox", soniox_consent: true };
  backend.sonioxKeyStatus.mockResolvedValue(true);
  let finishSave!: (next: Settings) => void;
  backend.setSettings.mockReturnValueOnce(new Promise<Settings>((resolve) => { finishSave = resolve; }));
  render(<SettingsPage />);
  const provider = await screen.findByLabelText("Transcription provider");
  expect(provider).toHaveValue("soniox");
  const consent = screen.getByRole("switch", { name: "Allow Soniox to process recorded audio" });
  expect(consent).toHaveAttribute("aria-checked", "true");
  fireEvent.click(consent);

  expect(consent).toHaveAttribute("aria-checked", "false");
  expect(provider).toHaveValue("local");
  expect(screen.getByRole("option", { name: /Soniox cloud/ })).toBeDisabled();
  expect(backend.setSettings).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
    stt_provider: "local", soniox_consent: false,
  }));
  await act(async () => { finishSave(backend.setSettings.mock.calls[0][0] as Settings); });
  expect(await screen.findByText("Saved")).toBeInTheDocument();
  expect(provider).toHaveValue("local");
  expect(backend.saveSonioxKey).not.toHaveBeenCalled();
});

it("keeps a rejected API key available for correction and retry, then clears it on success", async () => {
  backend.saveSonioxKey.mockRejectedValueOnce(new Error("Secure storage temporarily unavailable"));
  render(<SettingsPage />);
  const key = await screen.findByLabelText("Soniox API key");
  const save = screen.getByRole("button", { name: "Save key" });
  expect(save).toBeDisabled();
  expect(key).toHaveAttribute("type", "password");
  fireEvent.change(key, { target: { value: "synthetic-test-key" } });
  fireEvent.click(save);
  expect(await screen.findByText("Error: Secure storage temporarily unavailable")).toBeInTheDocument();
  expect(key).toHaveValue("synthetic-test-key");
  expect(save).toBeEnabled();
  expect(backend.setSettings).not.toHaveBeenCalled();

  fireEvent.change(key, { target: { value: "corrected-synthetic-test-key" } });
  fireEvent.click(save);
  expect(await screen.findByText("Soniox key saved")).toBeInTheDocument();
  await waitFor(() => expect(key).toHaveValue(""));
  expect(save).toBeDisabled();
  expect(screen.queryByText("Error: Secure storage temporarily unavailable")).not.toBeInTheDocument();
  expect(backend.saveSonioxKey.mock.calls).toEqual([["synthetic-test-key"], ["corrected-synthetic-test-key"]]);
  expect(backend.setSettings).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Transcription provider")).toHaveValue("local");
  expect(screen.getByRole("option", { name: /Soniox cloud/ })).toBeDisabled();
});

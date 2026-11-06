import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import type { cmd, ModelRow, Settings } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import { Onboarding } from "./Onboarding";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), probeHardware: vi.fn(), listModels: vi.fn(), sampleLevels: vi.fn(),
  setSettings: vi.fn(), downloadModel: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: () => () => {} }));

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  backend.getSettings.mockResolvedValue(null);
  backend.probeHardware.mockResolvedValue(null);
  backend.listModels.mockResolvedValue([]);
  backend.sampleLevels.mockResolvedValue([-30, -40]);
});
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("keeps onboarding responsive between meter probes and stops them after the audio step", async () => {
  render(<MemoryRouter><Onboarding /></MemoryRouter>);
  await act(async () => {});
  expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
  await act(() => vi.advanceTimersByTimeAsync(599));
  expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
  await act(() => vi.advanceTimersByTimeAsync(5000));
  expect(backend.sampleLevels).toHaveBeenCalledTimes(1);
});

it.each([
  { state: "missing", present: false, downloading: false, action: "Start with essentials" },
  { state: "downloading", present: false, downloading: true, action: null },
  { state: "available", present: true, downloading: false, action: "Open Bazza, finish the rest in the background" },
])("completes onboarding without starting downloads when the essential model is $state", async ({ present, downloading, action }) => {
  const settings = { ...await mockInvoke<Settings>("get_settings"), onboarded: false };
  const essential: ModelRow = {
    spec: { id: "whisper-base", name: "Whisper Base", kind: "asr", file: "base.bin", url: "", sha256: "", size_bytes: 1024, note: "" },
    roles: ["live"], present, downloading,
  };
  const optional: ModelRow = {
    ...essential,
    spec: { ...essential.spec, id: "whisper-small", name: "Whisper Small", file: "small.bin" },
    roles: ["quality"], present: false, downloading: false,
  };
  backend.getSettings.mockResolvedValue(settings);
  backend.probeHardware.mockResolvedValue(await mockInvoke<Awaited<ReturnType<typeof cmd.probeHardware>>>("probe_hardware"));
  backend.listModels.mockResolvedValue([essential, optional]);
  backend.setSettings.mockImplementation(async (value: Settings) => value);
  const onDone = vi.fn();
  render(<MemoryRouter initialEntries={["/setup"]}><Routes>
    <Route path="/setup" element={<Onboarding onDone={onDone} />} />
    <Route path="/" element={<h1>Library</h1>} />
  </Routes></MemoryRouter>);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Continue" })));
  if (action) expect(screen.getByRole("button", { name: action })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Skip for now" })).toBeEnabled();

  await act(async () => { await i18n.changeLanguage("ru"); });
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Пропустить" })));
  expect(backend.downloadModel).not.toHaveBeenCalled();
  expect(backend.setSettings.mock.calls).toEqual([[settings], [{ ...settings, onboarded: true }]]);
  expect(onDone).toHaveBeenCalledOnce();
  expect(screen.getByRole("heading", { name: "Library" })).toBeVisible();
});

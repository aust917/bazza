import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { cmd, ModelRow, Settings } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import { Onboarding } from "./Onboarding";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), probeHardware: vi.fn(), listModels: vi.fn(), sampleLevels: vi.fn(),
  setSettings: vi.fn(), downloadModel: vi.fn(), setUiLocale: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: () => () => {} }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

type HardwareResult = Awaited<ReturnType<typeof cmd.probeHardware>>;
let hardware: HardwareResult;

beforeEach(async () => {
  vi.resetAllMocks();
  hardware = await mockInvoke<HardwareResult>("probe_hardware");
  backend.getSettings.mockResolvedValue(await mockInvoke<Settings>("get_settings"));
  backend.listModels.mockResolvedValue([]);
  backend.sampleLevels.mockResolvedValue([-30, -40]);
});
afterEach(() => cleanup());

it("shows the hardware step while its probe is pending and allows going Back without starting another probe", async () => {
  const probe = deferred<HardwareResult>();
  backend.probeHardware.mockReturnValueOnce(probe.promise);
  render(<MemoryRouter><Onboarding /></MemoryRouter>);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(screen.getByRole("heading", { name: "Sized for this machine." })).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent("Loading...");
  expect(screen.getByRole("button", { name: "Back" })).toBeEnabled();
  expect(screen.queryByRole("button", { name: /^CPU, roomy/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Continue" })).not.toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  expect(screen.getByRole("heading", { name: "Bazza is listening for you, not to you." })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(screen.getByRole("status")).toHaveTextContent("Loading...");
  expect(backend.probeHardware).toHaveBeenCalledTimes(1);
  await act(async () => { probe.resolve(hardware); });
  expect(screen.getByRole("button", { name: /^CPU, roomy/ })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /^CPU, light/ })).toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(backend.setSettings).not.toHaveBeenCalled();
  expect(backend.downloadModel).not.toHaveBeenCalled();
});

it("offers a retry after a failed hardware probe and restores choices without duplicate work while retrying", async () => {
  const firstProbe = deferred<HardwareResult>();
  const retryProbe = deferred<HardwareResult>();
  backend.probeHardware.mockReturnValueOnce(firstProbe.promise).mockReturnValueOnce(retryProbe.promise);
  render(<MemoryRouter><Onboarding /></MemoryRouter>);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await act(async () => { firstProbe.reject(new Error("Hardware information temporarily unavailable")); });
  expect(screen.getByRole("heading", { name: "Sized for this machine." })).toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent("Error: Hardware information temporarily unavailable");
  expect(screen.getByRole("button", { name: "Back" })).toBeEnabled();
  const retry = screen.getByRole("button", { name: "Retry" });
  act(() => { fireEvent.click(retry); fireEvent.click(retry); });
  expect(screen.getByRole("status")).toHaveTextContent("Loading...");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(backend.probeHardware).toHaveBeenCalledTimes(2);
  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(backend.probeHardware).toHaveBeenCalledTimes(2);
  await act(async () => { retryProbe.resolve(hardware); });
  expect(screen.getByRole("button", { name: /^CPU, roomy/ })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /^CPU, light/ })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
  expect(backend.setSettings).not.toHaveBeenCalled();
  expect(backend.downloadModel).not.toHaveBeenCalled();
});

it("uses the new settings revision to complete managed onboarding without legacy audio or locale controls", async () => {
  const settings = { ...await mockInvoke<Settings>("get_settings"), onboarded: false, service_revision: "initial" };
  const model: ModelRow = {
    spec: {
      id: "whisper-base", kind: "asr", name: "Whisper Base",
      file: "model.bin", url: "", sha256: "", size_bytes: 1, note: "",
    },
    present: true, downloading: false, roles: ["live"],
  };
  backend.getSettings.mockResolvedValue(settings);
  backend.probeHardware.mockResolvedValue(hardware);
  backend.listModels.mockResolvedValue([model]);
  backend.setSettings.mockResolvedValueOnce({ ...settings, service_revision: "updated" })
    .mockResolvedValueOnce({ ...settings, onboarded: true, service_revision: "finished" });
  const onDone = vi.fn();
  render(<MemoryRouter><Onboarding serviceDesktop onDone={onDone} /></MemoryRouter>);

  const next = await screen.findByRole("button", { name: "Continue" });
  expect(screen.getByRole("heading", { name: "Sized for this machine." })).toBeInTheDocument();
  expect(screen.queryByLabelText("Interface language")).not.toBeInTheDocument();
  expect(backend.sampleLevels).not.toHaveBeenCalled();
  fireEvent.click(next);
  fireEvent.click(await screen.findByRole("button", { name: "Open Bazza" }));
  await act(async () => {});

  expect(backend.setSettings.mock.calls).toEqual([
    [settings],
    [{ ...settings, service_revision: "updated", onboarded: true }],
  ]);
  expect(onDone).toHaveBeenCalledOnce();
});

it("offers a per-model retry when the download cannot start", async () => {
  const settings = { ...await mockInvoke<Settings>("get_settings"), onboarded: false };
  let model: ModelRow = {
    spec: { id: "whisper-base", name: "Whisper Base", kind: "asr", file: "", url: "", sha256: "", size_bytes: 2048, note: "" },
    present: false, downloading: false, roles: ["live"],
  };
  backend.getSettings.mockResolvedValue(settings);
  backend.setSettings.mockResolvedValue(settings);
  backend.probeHardware.mockResolvedValue(hardware);
  backend.listModels.mockImplementation(async () => [model]);
  const first = deferred<void>();
  const retry = deferred<void>();
  backend.downloadModel.mockReturnValueOnce(first.promise).mockReturnValueOnce(retry.promise);
  render(<MemoryRouter><Onboarding serviceDesktop /></MemoryRouter>);

  fireEvent.click(await screen.findByRole("button", { name: "Continue" }));
  fireEvent.click(await screen.findByRole("button", { name: "Download" }));
  expect(screen.getByRole("progressbar", { name: "Download progress for Whisper Base" })).not.toHaveAttribute("aria-valuenow");
  expect(screen.queryByRole("button", { name: "Download all" })).not.toBeInTheDocument();
  await act(async () => first.reject(new Error("Network unavailable")));
  expect(screen.getByRole("alert")).toHaveTextContent("Download failed. Error: Network unavailable");

  fireEvent.click(screen.getByRole("button", { name: "Retry download" }));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.getByText("Preparing download…")).toBeVisible();
  model = { ...model, present: true };
  await act(async () => retry.resolve(undefined));
  expect(screen.getByRole("button", { name: "Open Bazza" })).toBeEnabled();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
});

import { act, cleanup, render, screen } from "@testing-library/react";
import { Outlet } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => {
  history.replaceState(null, "", "/?mode=service-desktop&managed=1&installed=1");
  return { getSettings: vi.fn() };
});
vi.mock("@/lib/theme", () => ({ installTheme: vi.fn() }));
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: () => () => {} }));
vi.mock("@/components/Shell", () => ({ Shell: () => <><p>Managed library shell</p><Outlet /></> }));
vi.mock("@/routes/Library", () => ({ Library: () => <p>Library</p> }));
vi.mock("@/routes/RecordingSetup", () => ({ default: () => <p>Recording setup</p> }));
vi.mock("@/routes/Onboarding", () => ({
  Onboarding: ({ serviceDesktop }: { serviceDesktop: boolean }) => <p>{serviceDesktop ? "Managed onboarding" : "Legacy onboarding"}</p>,
}));
import App from "./App";

beforeEach(() => {
  history.replaceState(null, "", "/?mode=service-desktop&managed=1&installed=1");
  vi.clearAllMocks();
});
afterEach(() => {
  cleanup();
  history.replaceState(null, "", "/");
});

it("opens managed onboarding for a fresh installed library", async () => {
  backend.getSettings.mockResolvedValue({ onboarded: false });
  render(<App />);
  expect(await screen.findByText("Managed onboarding")).toBeInTheDocument();
  expect(backend.getSettings).toHaveBeenCalledOnce();
});

it("opens the managed library directly after onboarding has been completed", async () => {
  backend.getSettings.mockResolvedValue({ onboarded: true });
  render(<App />);
  expect(await screen.findByText("Managed library shell")).toBeInTheDocument();
  expect(screen.queryByText("Managed onboarding")).not.toBeInTheDocument();
});

it("opens recording setup when the native window changes its hash during startup", async () => {
  let loadSettings!: (settings: { onboarded: boolean }) => void;
  backend.getSettings.mockImplementation(() => new Promise((resolve) => { loadSettings = resolve; }));
  render(<App />);

  await act(async () => {
    window.location.hash = "/recording";
    loadSettings({ onboarded: true });
  });

  expect(await screen.findByText("Recording setup")).toBeInTheDocument();
  expect(screen.queryByText("Managed onboarding")).not.toBeInTheDocument();
});

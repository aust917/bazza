import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Events, Settings, VideoSource } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import { Popup } from "./Popup";
import i18n from "@/lib/i18n";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), setSettings: vi.fn(), listAudioDevices: vi.fn(),
  listVideoSources: vi.fn(), startRecording: vi.fn(), dismissDetection: vi.fn(),
  discordStatus: vi.fn(), discordGuilds: vi.fn(), discordChannels: vi.fn(),
  attendeeStatus: vi.fn(),
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
  { target: { kind: "monitor", index: 0 }, screen_id: "display-a", label: "Display 1" },
];
let settings: Settings;

async function openPicker() {
  act(() => listeners.get("detection")?.({ app: "unknown", label: "Possible call", title: "Call", event: null, confidence: 1 } satisfies Events["detection"]));
  await waitFor(() => expect(screen.getByRole("button", { name: "Record" })).toBeEnabled());
}

beforeEach(async () => {
  vi.clearAllMocks();
  listeners.clear();
  settings = { ...await mockInvoke<Settings>("get_settings"), capture_system: true };
  backend.getSettings.mockImplementation(async () => ({ ...settings }));
  backend.setSettings.mockImplementation(async (next: Settings) => { settings = next; return { ...next }; });
  backend.listVideoSources.mockResolvedValue(sources);
  backend.listAudioDevices.mockResolvedValue({ inputs: [], outputs: [{ id: "speaker", name: "USB speakers", is_default: true }] });
  backend.startRecording.mockResolvedValue({ id: "meeting" });
  backend.discordStatus.mockResolvedValue({ bot: null, channel: null });
  backend.attendeeStatus.mockResolvedValue({ configured: false });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("detected call picker audio", () => {
  it("shows the saved speech language before recording", async () => {
    settings.language = "en";
    render(<Popup />);
    await openPicker();
    expect(screen.getByText("Speech language · English")).toBeVisible();
    expect(backend.setSettings).not.toHaveBeenCalled();
    expect(backend.startRecording).not.toHaveBeenCalled();
  });

  it("translates the output description without changing settings or starting capture", async () => {
    render(<Popup />);
    await openPicker();
    await act(async () => { await i18n.changeLanguage("ru"); });
    expect(screen.getByRole("button", { name: "Записать" })).toBeEnabled();
    expect(screen.getByText(/весь звук на выходе «USB speakers»/)).toBeInTheDocument();
    expect(backend.setSettings).not.toHaveBeenCalled();
    expect(backend.startRecording).not.toHaveBeenCalled();
    await act(async () => { await i18n.changeLanguage("en"); });
    expect(screen.getByText(/Records all sound on USB speakers/)).toBeInTheDocument();
  });
  it("uses the whole saved output when started with Enter, even with an old app-only preference", async () => {
    const legacySettings = { ...settings, loopback_device: "speaker", audio_scope: "app" };
    settings = legacySettings;
    render(<Popup />);
    await openPicker();
    expect(screen.queryByRole("radio", { name: "This app only" })).not.toBeInTheDocument();
    expect(screen.getByText(/Records all sound on USB speakers/)).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Enter" });
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith({
      app: undefined, video: true, target: { kind: "monitor", index: 0 }, screen_id: "display-a",
    }));
    expect(backend.setSettings).not.toHaveBeenCalled();
  });

  it("records the whole output in audio-only mode", async () => {
    render(<Popup />);
    await openPicker();
    fireEvent.click(screen.getByRole("button", { name: "Record screen video" }));
    fireEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith({ app: undefined, video: false, target: undefined }));
  });

  it("requires an explicit monitor with multiple displays", async () => {
    backend.listVideoSources.mockResolvedValue([
      ...sources,
      { target: { kind: "monitor", index: 1 }, screen_id: "display-b", label: "Display 2" },
    ]);
    render(<Popup />);
    act(() => listeners.get("detection")?.({ app: "unknown", label: "Possible call", title: "Call", event: null, confidence: 1 } satisfies Events["detection"]));
    const picker = await screen.findByLabelText("Screen to record");
    await screen.findByRole("option", { name: "Display 2" });
    expect(screen.getByRole("button", { name: "Record" })).toBeDisabled();
    fireEvent.keyDown(window, { key: "Enter" });
    expect(backend.startRecording).not.toHaveBeenCalled();
    fireEvent.change(picker, { target: { value: "display-b" } });
    fireEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith({
      app: undefined, video: true, target: { kind: "monitor", index: 1 }, screen_id: "display-b",
    }));
    expect(backend.setSettings).not.toHaveBeenCalled();
  });

  it("makes microphone-only settings explicit", async () => {
    settings.capture_system = false;
    render(<Popup />);
    await openPicker();
    expect(screen.getByText(/Microphone only/)).toBeInTheDocument();
    expect(screen.queryByText(/Records all sound on/)).not.toBeInTheDocument();
  });
});

it("selects a Discord channel without joining until recording, and resets the opt-in for the next recording", async () => {
  const original = { guild_id: "guild", guild_name: "Team", channel_id: "original", channel_name: "Daily" };
  const other = { ...original, channel_id: "other", channel_name: "Workshop" };
  backend.discordStatus.mockResolvedValue({ bot: { id: "bot", name: "Demo bot" }, channel: original });
  backend.discordGuilds.mockResolvedValue([{ id: "guild", name: "Team" }]);
  backend.discordChannels.mockResolvedValue([original, other]);
  render(<Popup />);
  await openPicker();

  const names = await screen.findByRole("switch", { name: "Names from Discord" });
  expect(names).not.toBeChecked();
  expect(backend.discordGuilds).not.toHaveBeenCalled();
  fireEvent.click(names);
  expect(screen.getByText("Team / Daily")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Choose channel" }));
  await screen.findByRole("option", { name: "Workshop" });
  fireEvent.change(screen.getByLabelText("Discord voice channel"), { target: { value: "other" } });
  expect(backend.startRecording).not.toHaveBeenCalled();
  fireEvent.keyDown(window, { key: "Enter" });
  await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ discord: { guild_id: "guild", channel_id: "other" } })));

  await openPicker();
  expect(await screen.findByRole("switch", { name: "Names from Discord" })).not.toBeChecked();
  fireEvent.click(screen.getByRole("button", { name: "Record" }));
  await waitFor(() => expect(backend.startRecording).toHaveBeenCalledTimes(2));
  expect(backend.startRecording.mock.calls[1][0]).not.toHaveProperty("discord");
});

it("joins Google Meet only after an explicit opt-in and a valid meeting link", async () => {
  backend.attendeeStatus.mockResolvedValue({ configured: true });
  render(<Popup />);
  await openPicker();
  const tracks = await screen.findByRole("switch", { name: "Separate Google Meet tracks" });
  expect(tracks).not.toBeChecked();

  fireEvent.click(tracks);
  const link = screen.getByLabelText("Google Meet link");
  const record = screen.getByRole("button", { name: "Record" });
  expect(record).toBeDisabled();
  fireEvent.change(link, { target: { value: "https://meet.google.com.example.org/abc-defg-hij" } });
  expect(record).toBeDisabled();
  expect(backend.startRecording).not.toHaveBeenCalled();

  const meetingUrl = "https://meet.google.com/meet/abc-defg-hij?pli=1";
  fireEvent.change(link, { target: { value: meetingUrl } });
  fireEvent.click(record);
  await waitFor(() => expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ app: "meet", attendee: { meetingUrl } })));

  await openPicker();
  expect(await screen.findByRole("switch", { name: "Separate Google Meet tracks" })).not.toBeChecked();
  expect(screen.queryByLabelText("Google Meet link")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Record" }));
  expect(backend.startRecording.mock.calls[1][0]).not.toHaveProperty("attendee");
});

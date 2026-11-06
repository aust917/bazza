import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Events, Settings, VideoSource } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import RecordingSetup from "./RecordingSetup";

const backend = vi.hoisted(() => ({
  attendeeStatus: vi.fn(),
  connectRecordingBot: vi.fn(),
  discordChannels: vi.fn(),
  discordGuilds: vi.fn(),
  discordSetChannel: vi.fn(),
  discordStatus: vi.fn(),
  disconnectRecordingBot: vi.fn(),
  getSettings: vi.fn(),
  listAudioDevices: vi.fn(),
  listVideoSources: vi.fn(),
  recordingStatus: vi.fn(),
  setSettings: vi.fn(),
  startRecording: vi.fn(),
}));
const listeners = vi.hoisted(() => new Map<string, (payload: unknown) => void>());
vi.mock("@/lib/ipc", () => ({
  cmd: backend,
  on: async (name: string, callback: (payload: unknown) => void) => {
    listeners.set(name, callback);
    return () => { listeners.delete(name); };
  },
}));

const sources: VideoSource[] = [
  { target: { kind: "monitor", index: 0 }, screen_id: "display-a", label: "Display 1", is_primary: true },
];
const extraScreen: VideoSource = { target: { kind: "monitor", index: 1 }, screen_id: "display-b", label: "Display 2" };
let settings: Settings;

beforeEach(async () => {
  vi.resetAllMocks();
  listeners.clear();
  settings = { ...await mockInvoke<Settings>("get_settings"), capture_system: true };
  backend.getSettings.mockImplementation(async () => ({ ...settings }));
  backend.setSettings.mockImplementation(async (next: Settings) => { settings = next; return { ...next }; });
  backend.listVideoSources.mockResolvedValue(sources);
  backend.listAudioDevices.mockResolvedValue({
    inputs: [{ id: "mic", name: "USB microphone", is_default: true }],
    outputs: [{ id: "speaker", name: "USB speakers", is_default: true }],
  });
  backend.recordingStatus.mockResolvedValue({ state: "idle", meeting_id: null, elapsed_ms: 0 });
  backend.startRecording.mockResolvedValue({ id: "new-meeting" });
  backend.discordGuilds.mockResolvedValue([]);
  backend.discordChannels.mockResolvedValue([]);
  backend.discordStatus.mockResolvedValue({ bot: null, channel: null });
  backend.attendeeStatus.mockResolvedValue({ configured: false });
  backend.connectRecordingBot.mockImplementation(async (requestId: string) => ({
    requestId, revision: 0, phase: "connecting",
  }));
  backend.disconnectRecordingBot.mockResolvedValue(undefined);
});
afterEach(() => vi.restoreAllMocks());

function SettingsView() {
  const { hash } = useLocation();
  return <><h1>Settings view</h1><output aria-label="Settings section">{hash}</output></>;
}

function renderSetup(returnTo?: string) {
  return render(
    <MemoryRouter initialEntries={[{ pathname: "/recording", state: { returnTo } }]}>
      <Routes>
        <Route path="/" element={<h1>Library view</h1>} />
        <Route path="/recording" element={<RecordingSetup />} />
        <Route path="/meeting/:id" element={<h1>Meeting view</h1>} />
        <Route path="/settings" element={<SettingsView />} />
      </Routes>
    </MemoryRouter>,
  );
}

const start = () => screen.getByRole("button", { name: "Start recording" });
const startWithBot = () => screen.getByRole("button", { name: "Start recording with bot" });
const connect = () => screen.getByRole("button", { name: "Connect bot" });
type BotStatus = Events["recording_bot_connection_status"];

it("shows the default speech language before recording", async () => {
  renderSetup();
  expect(await screen.findByText("Auto-detect")).toBeVisible();
  expect(backend.setSettings).not.toHaveBeenCalled();
  expect(backend.startRecording).not.toHaveBeenCalled();
});

it.each([["Discord", "#discord"], ["Google Meet", "#google-meet"]])(
  "opens the matching settings section when the %s bot is unconfigured",
  async (mode, section) => {
    renderSetup();
    await waitFor(() => expect(start()).toBeEnabled());
    fireEvent.click(screen.getByRole("radio", { name: mode }));
    fireEvent.click(await screen.findByRole("button", { name: "Open settings" }));
    expect(await screen.findByRole("heading", { name: "Settings view" })).toBeInTheDocument();
    expect(screen.getByLabelText("Settings section")).toHaveTextContent(section);
  },
);

function emitBot(status: BotStatus) {
  act(() => listeners.get("recording_bot_connection_status")?.(status));
}

async function chooseMeet(meetingUrl: string) {
  fireEvent.click(screen.getByRole("radio", { name: "Google Meet" }));
  const input = await screen.findByLabelText("Google Meet link");
  fireEvent.change(input, { target: { value: meetingUrl } });
  return input;
}

function screenMenu() {
  return screen.getByRole("button", { name: /^Screen to record/ });
}

async function openScreenMenu() {
  await waitFor(() => expect(screenMenu()).toBeEnabled());
  fireEvent.click(screenMenu());
}

it("keeps audio-only format and the whole output across modes", async () => {
  renderSetup();
  expect(screen.getByRole("heading", { name: "New recording" })).toBeInTheDocument();
  await waitFor(() => expect(start()).toBeEnabled());
  expect(screen.getByText("Live captions or recording audio")).toBeVisible();
  fireEvent.click(screen.getByRole("radio", { name: /^Audio only/ }));
  fireEvent.click(screen.getByRole("radio", { name: "Google Meet" }));
  expect(screen.getByRole("radio", { name: /^Audio only/ })).toBeChecked();
  expect(screen.queryByRole("button", { name: /^Screen to record/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("radio", { name: "This app only" })).not.toBeInTheDocument();
  expect(screen.getByText("Microphone + all sound on USB speakers")).toBeVisible();
  expect(start()).toBeEnabled();
  expect(connect()).toBeDisabled();
  fireEvent.click(screen.getByRole("radio", { name: "Standard" }));
  expect(backend.startRecording).not.toHaveBeenCalled();

  fireEvent.click(start());
  expect(await screen.findByRole("heading", { name: "Meeting view" })).toBeInTheDocument();
  expect(backend.startRecording).toHaveBeenCalledOnce();
  expect(backend.startRecording).toHaveBeenCalledWith({ app: undefined, video: false, target: undefined });
  expect(backend.setSettings).not.toHaveBeenCalled();
});

it("defaults to Standard, the sole display and all apps without changing saved preferences", async () => {
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  expect(screen.getByRole("radio", { name: "Standard" })).toBeChecked();
  expect(screen.getByRole("radio", { name: "Zoom" })).toBeDisabled();
  expect(screenMenu()).toHaveTextContent("Display 1");
  expect(screen.getByText("Records all sound on USB speakers, including media and notifications.")).toBeVisible();

  fireEvent.click(start());
  await screen.findByRole("heading", { name: "Meeting view" });
  expect(backend.startRecording).toHaveBeenCalledWith({
    video: true, target: { kind: "monitor", index: 0 }, screen_id: "display-a", app: undefined,
  });
  expect(backend.setSettings).not.toHaveBeenCalled();
});

it("uses the system primary display on open and refresh until a display is chosen", async () => {
  backend.listVideoSources.mockResolvedValueOnce([extraScreen, ...sources]);
  const { unmount } = renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  expect(screenMenu()).toHaveTextContent("Display 1");

  backend.listVideoSources.mockResolvedValue([
    { ...extraScreen, target: { kind: "monitor", index: 0 }, is_primary: true },
    { ...sources[0], target: { kind: "monitor", index: 1 }, is_primary: false },
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Refresh sources" }));
  await waitFor(() => expect(screenMenu()).toHaveTextContent("Display 2"));
  unmount();

  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  expect(screenMenu()).toHaveTextContent("Display 2");
  fireEvent.click(start());
  await screen.findByRole("heading", { name: "Meeting view" });
  expect(backend.startRecording).toHaveBeenCalledWith({
    app: undefined, video: true, target: { kind: "monitor", index: 0 }, screen_id: "display-b",
  });
});

it("requires a new choice when the selected monitor disappears on refresh", async () => {
  backend.listVideoSources.mockResolvedValue([...sources, extraScreen]);
  renderSetup();
  await openScreenMenu();
  expect(start()).toBeEnabled();
  fireEvent.click(screen.getByRole("menuitemradio", { name: /Display 2/ }));
  expect(start()).toBeEnabled();
  backend.listVideoSources.mockResolvedValueOnce(sources);
  fireEvent.click(screen.getByRole("button", { name: "Refresh sources" }));
  await openScreenMenu();
  expect(screen.queryByRole("menuitemradio", { name: /Display 2/ })).not.toBeInTheDocument();
  expect(start()).toBeDisabled();
  expect(backend.startRecording).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("menuitemradio", { name: /Display 1/ }));
  expect(start()).toBeEnabled();
});

it("sends the chosen monitor after display indices change", async () => {
  backend.listVideoSources.mockResolvedValue([...sources, extraScreen]);
  renderSetup();
  await openScreenMenu();
  expect(start()).toBeEnabled();
  fireEvent.click(screen.getByRole("menuitemradio", { name: /Display 2/ }));
  expect(screenMenu()).toHaveFocus();
  fireEvent.keyDown(screenMenu(), { key: "ArrowDown" });
  expect(screen.getByRole("menuitemradio", { name: /Display 2/, checked: true })).toBeInTheDocument();
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screenMenu()).toHaveFocus();
  backend.listVideoSources.mockResolvedValueOnce([
    { ...extraScreen, target: { kind: "monitor", index: 0 } },
    ...sources.map((source) => ({ ...source, target: { kind: "monitor", index: 1 } })),
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Refresh sources" }));
  await waitFor(() => expect(start()).toBeEnabled());
  fireEvent.click(start());
  await screen.findByRole("heading", { name: "Meeting view" });
  expect(backend.startRecording).toHaveBeenCalledWith({
    app: undefined, video: true, target: { kind: "monitor", index: 0 }, screen_id: "display-b",
  });
});

it("returns to the previous screen on Cancel without starting capture", async () => {
  renderSetup("/settings");
  await waitFor(() => expect(start()).toBeEnabled());
  fireEvent.click(screen.getByRole("link", { name: "Cancel" }));
  expect(await screen.findByRole("heading", { name: "Settings view" })).toBeInTheDocument();
  expect(backend.startRecording).not.toHaveBeenCalled();
});

it("updates when another window starts a recording", async () => {
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  act(() => listeners.get("recording_state")?.({
    state: "recording", meeting_id: "current-meeting", elapsed_ms: 3000,
  } satisfies Events["recording_state"]));
  expect(screen.queryByRole("button", { name: "Start recording" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Open recording" })).toBeEnabled();
  expect(backend.startRecording).not.toHaveBeenCalled();
});

it("starts with a Meet bot only after the current request confirms admission", async () => {
  backend.attendeeStatus.mockResolvedValue({ configured: true });
  let finishConnect!: (status: BotStatus) => void;
  backend.connectRecordingBot.mockReturnValueOnce(new Promise<BotStatus>((resolve) => { finishConnect = resolve; }));
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  const link = await chooseMeet("https://meet.google.com.example.org/abc-defg-hij");
  expect(connect()).toBeDisabled();
  expect(start()).toBeEnabled();
  expect(backend.connectRecordingBot).not.toHaveBeenCalled();
  expect(backend.startRecording).not.toHaveBeenCalled();

  const meetingUrl = "https://meet.google.com/abc-defg-hij?pli=1";
  fireEvent.change(link, { target: { value: meetingUrl } });
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledOnce());
  const [[requestId]] = backend.connectRecordingBot.mock.calls;
  expect(backend.connectRecordingBot).toHaveBeenCalledWith(requestId, { provider: "meet", meetingUrl });

  emitBot({ requestId, revision: 1, phase: "waiting_admission" });
  emitBot({ requestId: "another-request", revision: 9, phase: "connected" });
  expect(screen.getByText("Waiting for admission. Admit the bot in Google Meet.")).toHaveAttribute("role", "status");
  expect(screen.getByRole("progressbar", { name: "Waiting for admission. Admit the bot in Google Meet." })).not.toHaveAttribute("aria-valuenow");
  expect(screen.getByRole("button", { name: "Cancel connection" })).toBeEnabled();
  expect(start()).toBeEnabled();
  expect(screen.queryByRole("button", { name: "Start recording with bot" })).not.toBeInTheDocument();
  expect(backend.startRecording).not.toHaveBeenCalled();

  emitBot({ requestId, revision: 2, phase: "connected" });
  await act(async () => { finishConnect({ requestId, revision: 0, phase: "connecting" }); });
  emitBot({ requestId, revision: 1, phase: "waiting_admission" });
  expect(screen.getByText("Bot connected.")).toHaveAttribute("role", "status");
  expect(startWithBot()).toBeEnabled();
  fireEvent.click(startWithBot());
  await screen.findByRole("heading", { name: "Meeting view" });
  expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({
    app: "meet", bot_connection_id: requestId,
  }));
  expect(backend.startRecording.mock.calls[0][0]).not.toHaveProperty("attendee");
  expect(backend.disconnectRecordingBot).not.toHaveBeenCalled();
});

it("shows the Discord channel prompt in the picker alone", async () => {
  backend.discordStatus.mockResolvedValue({ bot: { id: "1", name: "Bazza" }, channel: null });
  backend.discordGuilds.mockResolvedValue([{ id: "12", name: "Project" }]);
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  fireEvent.click(screen.getByRole("radio", { name: "Discord" }));

  await waitFor(() => expect(screen.getByRole("combobox", { name: "Discord voice channel" })).toBeEnabled());
  expect(screen.getByRole("option", { name: "Choose voice channel" })).toBeInTheDocument();
  expect(screen.queryByText("Choose a voice channel")).not.toBeInTheDocument();
  expect(connect()).toBeDisabled();
  expect(start()).toBeEnabled();
});

it("uses a Discord channel from a validated link only for the current recording", async () => {
  const saved = { guild_id: "3", channel_id: "4", guild_name: "Studio", channel_name: "Chat" };
  const linked = { guild_id: "12", channel_id: "34", guild_name: "Project", channel_name: "Workshop" };
  backend.discordStatus.mockResolvedValue({ bot: { id: "1", name: "Bazza" }, channel: saved });
  backend.discordChannels.mockResolvedValue([linked]);
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  fireEvent.click(screen.getByRole("radio", { name: "Discord" }));
  expect(await screen.findByText("Studio / Chat")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Paste a voice channel link" }));
  fireEvent.change(screen.getByLabelText("Discord channel link"), { target: { value: "https://discord.com/channels/12/34" } });
  fireEvent.click(screen.getByRole("button", { name: "Choose by link" }));
  expect(await screen.findByText("Project / Workshop")).toBeInTheDocument();
  expect(backend.discordChannels).toHaveBeenCalledWith("12");
  expect(backend.discordSetChannel).not.toHaveBeenCalled();

  expect(start()).toBeEnabled();
  expect(backend.connectRecordingBot).not.toHaveBeenCalled();
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledOnce());
  const [[requestId]] = backend.connectRecordingBot.mock.calls;
  expect(backend.connectRecordingBot).toHaveBeenCalledWith(requestId, {
    provider: "discord", guild_id: "12", channel_id: "34",
  });
  emitBot({ requestId, revision: 1, phase: "connected" });
  fireEvent.click(startWithBot());
  await screen.findByRole("heading", { name: "Meeting view" });
  expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({
    app: "discord", bot_connection_id: requestId,
  }));
  expect(backend.startRecording.mock.calls[0][0]).not.toHaveProperty("discord");
});

it("cancels a pending Meet connection when the user chooses local recording", async () => {
  backend.attendeeStatus.mockResolvedValue({ configured: true });
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  await chooseMeet("https://meet.google.com/abc-defg-hij");
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledOnce());
  const [[requestId]] = backend.connectRecordingBot.mock.calls;
  emitBot({ requestId, revision: 1, phase: "waiting_admission" });
  fireEvent.click(start());
  await screen.findByRole("heading", { name: "Meeting view" });
  expect(backend.disconnectRecordingBot).toHaveBeenCalledWith(requestId);
  expect(backend.startRecording).toHaveBeenCalledOnce();
  const [[options]] = backend.startRecording.mock.calls;
  expect(options.bot_connection_id).toBeUndefined();
  expect(options.attendee).toBeUndefined();
  expect(options.discord).toBeUndefined();
});

it("invalidates the previous request when the Meet address changes and releases the replacement on Cancel", async () => {
  backend.attendeeStatus.mockResolvedValue({ configured: true });
  renderSetup("/settings");
  await waitFor(() => expect(start()).toBeEnabled());
  const link = await chooseMeet("https://meet.google.com/abc-defg-hij");
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledOnce());
  const [[previousId]] = backend.connectRecordingBot.mock.calls;
  emitBot({ requestId: previousId, revision: 1, phase: "connected" });
  expect(startWithBot()).toBeEnabled();

  const replacement = "https://meet.google.com/klm-nopq-rst";
  fireEvent.change(link, { target: { value: replacement } });
  await waitFor(() => expect(backend.disconnectRecordingBot).toHaveBeenCalledWith(previousId));
  emitBot({ requestId: previousId, revision: 2, phase: "connected" });
  expect(start()).toBeEnabled();
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledTimes(2));
  const [, [nextId]] = backend.connectRecordingBot.mock.calls;
  expect(nextId).not.toEqual(previousId);
  expect(backend.connectRecordingBot).toHaveBeenLastCalledWith(nextId, { provider: "meet", meetingUrl: replacement });
  fireEvent.click(screen.getByRole("link", { name: "Cancel" }));
  await screen.findByRole("heading", { name: "Settings view" });
  expect(backend.disconnectRecordingBot).toHaveBeenCalledWith(nextId);
  expect(backend.startRecording).not.toHaveBeenCalled();
});

it("keeps local recording available after a connection failure and can retry with a fresh request", async () => {
  backend.attendeeStatus.mockResolvedValue({ configured: true });
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  await chooseMeet("https://meet.google.com/abc-defg-hij");
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledOnce());
  const [[failedId]] = backend.connectRecordingBot.mock.calls;
  emitBot({ requestId: failedId, revision: 1, phase: "failed", message: "Bot could not join" });
  expect(screen.getByRole("alert")).toHaveTextContent("Could not connect the bot. Bot could not join");
  expect(start()).toBeEnabled();
  expect(backend.startRecording).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "Try connecting again" }));
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledTimes(2));
  const [, [retryId]] = backend.connectRecordingBot.mock.calls;
  expect(retryId).not.toEqual(failedId);
  emitBot({ requestId: failedId, revision: 2, phase: "connected" });
  expect(start()).toBeEnabled();
  emitBot({ requestId: retryId, revision: 1, phase: "connected" });
  expect(startWithBot()).toBeEnabled();
});

it("does not silently start local recording if native rejects adoption of the bot connection", async () => {
  backend.attendeeStatus.mockResolvedValue({ configured: true });
  backend.startRecording.mockRejectedValueOnce(new Error("Bot connection expired"));
  renderSetup();
  await waitFor(() => expect(start()).toBeEnabled());
  await chooseMeet("https://meet.google.com/abc-defg-hij");
  fireEvent.click(connect());
  await waitFor(() => expect(backend.connectRecordingBot).toHaveBeenCalledOnce());
  const [[requestId]] = backend.connectRecordingBot.mock.calls;
  emitBot({ requestId, revision: 1, phase: "connected" });
  fireEvent.click(startWithBot());
  expect(await screen.findByRole("alert")).toHaveTextContent("Bot connection expired");
  expect(backend.startRecording).toHaveBeenCalledOnce();
  expect(backend.startRecording).toHaveBeenCalledWith(expect.objectContaining({ bot_connection_id: requestId }));
  expect(screen.getByRole("heading", { name: "New recording" })).toBeInTheDocument();
});

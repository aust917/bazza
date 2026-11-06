import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Link, MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttendeeSettingsStatus, AttendeeSetupProgress, CodexMcpStatus, Events, ModelRow, Settings } from "@/lib/ipc";
import { mockInvoke } from "@/lib/mock";
import i18n from "@/lib/i18n";
import { SettingsPage } from "./Settings";

const backend = vi.hoisted(() => ({
  getSettings: vi.fn(), setSettings: vi.fn(), listAudioDevices: vi.fn(), probeHardware: vi.fn(),
  dataInfo: vi.fn(), listShares: vi.fn(), listModels: vi.fn(), recordingStatus: vi.fn(),
  downloadModel: vi.fn(), cancelDownload: vi.fn(),
  sonioxKeyStatus: vi.fn(), saveSonioxKey: vi.fn(),
  discordStatus: vi.fn(), discordSaveToken: vi.fn(), discordSetChannel: vi.fn(), discordGuilds: vi.fn(),
  attendeeStatus: vi.fn(), attendeeConfigure: vi.fn(), attendeeSetup: vi.fn(), subscribe: vi.fn(),
  setUiLocale: vi.fn(), sampleLevels: vi.fn(),
  getCodexMcpStatus: vi.fn(), setCodexMcpEnabled: vi.fn(),
  getClaudeMcpStatus: vi.fn(), setClaudeMcpEnabled: vi.fn(),
  subscriptionAgents: vi.fn(),
  exportMeetings: vi.fn(),
}));
const fileDialog = vi.hoisted(() => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: backend.subscribe }));
vi.mock("@tauri-apps/plugin-dialog", () => fileDialog);
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
  backend.discordStatus.mockResolvedValue({ bot: null, channel: null });
  backend.attendeeStatus.mockResolvedValue(await mockInvoke("attendee_status"));
  backend.subscribe.mockReturnValue(() => {});
  backend.discordGuilds.mockResolvedValue([]);
  backend.recordingStatus.mockResolvedValue({ state: "recording", meeting_id: "m1", elapsed_ms: 0 });
  backend.sampleLevels.mockResolvedValue([-30, -40]);
  backend.setUiLocale.mockImplementation(async (locale: "ru" | "en") => locale);
  backend.getCodexMcpStatus.mockResolvedValue({ enabled: true });
  backend.setCodexMcpEnabled.mockImplementation(async (enabled: boolean) => ({ enabled }));
  backend.getClaudeMcpStatus.mockResolvedValue(null);
});
afterEach(() => cleanup());

it("recovers a library export after reopening settings and shows 100% when the file is saved", async () => {
  let finish!: (count: number) => void;
  fileDialog.save.mockResolvedValue("synthetic.bazza");
  backend.exportMeetings.mockReturnValue(new Promise<number>((resolve) => { finish = resolve; }));
  const first = render(<SettingsPage />);
  fireEvent.click(await screen.findByRole("button", { name: "Export everything" }));
  expect(await screen.findByRole("progressbar", { name: /Exporting/ })).not.toHaveAttribute("aria-valuenow");
  first.unmount();
  render(<SettingsPage />);
  expect(await screen.findByRole("progressbar", { name: /Exporting/ })).not.toHaveAttribute("aria-valuenow");
  await act(async () => finish(2));
  expect(await screen.findByRole("progressbar", { name: /synthetic.bazza/ })).toHaveAttribute("aria-valuenow", "100");
  expect(backend.exportMeetings).toHaveBeenCalledExactlyOnceWith([], "synthetic.bazza", true);
});

it("explains manual subscription summaries separately from the local model and checks CLI sign-in on request", async () => {
  backend.subscriptionAgents.mockResolvedValue([
    { agent: "codex", status: "missing" }, { agent: "claude", status: "sign_in_required" },
  ]);
  render(<SettingsPage serviceDesktop />);

  expect(await screen.findByLabelText("Local language model")).toBeVisible();
  expect(screen.getByText(/Open a finished meeting, select Summary/)).toBeVisible();
  expect(screen.getByText(/Review the excerpt and summary instructions before sending them to OpenAI/)).toBeVisible();
  expect(backend.subscriptionAgents).not.toHaveBeenCalled();

  fireEvent.click(screen.getByText("Check CLI sign-in"));
  expect(await screen.findByText("Codex · CLI missing from PATH")).toBeVisible();
  expect(screen.getByText("Claude Code · sign in to the CLI")).toBeVisible();
  expect(backend.setSettings).not.toHaveBeenCalled();
});

it("focuses the requested integration after loading settings and follows section navigation", async () => {
  let finish!: (value: Settings) => void;
  backend.getSettings.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
  render(<MemoryRouter initialEntries={["/settings#discord"]}>
    <Link to="/settings#google-meet">Open Meet settings</Link>
    <SettingsPage serviceDesktop />
  </MemoryRouter>);
  expect(screen.queryByRole("region", { name: /Discord/ })).not.toBeInTheDocument();
  await act(async () => finish(settings));
  expect(screen.getByRole("region", { name: /Discord/ })).toHaveFocus();

  fireEvent.click(screen.getByRole("link", { name: "Open Meet settings" }));
  expect(screen.getByRole("region", { name: /Google Meet/ })).toHaveFocus();
});

it("shows independent model progress and lets a failed model retry before the old command returns", async () => {
  const id = "whisper-base";
  let firstModel: ModelRow = {
    spec: { id, name: "Whisper Base", kind: "asr", file: "", url: "", sha256: "", size_bytes: 2048, note: "" },
    present: false, downloading: false, roles: ["live"],
  };
  const background: ModelRow = {
    ...firstModel,
    spec: { ...firstModel.spec, id: "whisper-small", name: "Whisper Small" },
    downloading: true,
  };
  backend.listModels.mockImplementation(async () => [firstModel, background]);
  let rejectFirst!: (reason: Error) => void;
  let resolveSecond!: () => void;
  backend.downloadModel
    .mockReturnValueOnce(new Promise<void>((_, reject) => { rejectFirst = reject; }))
    .mockReturnValueOnce(new Promise<void>((resolve) => { resolveSecond = resolve; }));
  render(<SettingsPage serviceDesktop />);

  const start = await screen.findByRole("button", { name: "Download" });
  const backgroundBar = screen.getByRole("progressbar", { name: "Download progress for Whisper Small" });
  expect(backgroundBar).not.toHaveAttribute("aria-valuenow");
  fireEvent.click(start);
  const currentBar = () => screen.getByRole("progressbar", { name: "Download progress for Whisper Base" });
  expect(currentBar()).not.toHaveAttribute("aria-valuenow");
  const emit = backend.subscribe.mock.calls.find(([event]) => event === "model_progress")?.[1] as (value: Events["model_progress"]) => void;
  act(() => emit({ id, status: "downloading", done: 1024, total: 2048, error: null }));
  expect(currentBar()).toHaveAttribute("aria-valuenow", "50");
  expect(screen.getByText("1.0 KB of 2.0 KB")).toBeVisible();
  expect(backgroundBar).not.toHaveAttribute("aria-valuenow");

  act(() => emit({ id, status: "downloading", done: 2048, total: 2048, error: null }));
  expect(currentBar()).not.toHaveAttribute("aria-valuenow");
  expect(screen.getByText("Finishing download…")).toBeVisible();
  expect(screen.getByText("2.0 KB of 2.0 KB")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Remove" })).not.toBeInTheDocument();

  await act(async () => emit({ id, status: "failed", done: 0, total: 2048, error: "checksum mismatch" }));
  expect(screen.getByRole("alert")).toHaveTextContent("Download failed. checksum mismatch");
  fireEvent.click(screen.getByRole("button", { name: "Retry download" }));
  expect(currentBar()).not.toHaveAttribute("aria-valuenow");
  expect(backend.downloadModel.mock.calls).toEqual([[id], [id]]);
  await act(async () => rejectFirst(new Error("checksum mismatch")));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();

  firstModel = { ...firstModel, present: true };
  await act(async () => resolveSecond());
  expect(screen.queryByRole("progressbar", { name: "Download progress for Whisper Base" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Remove" })).toBeEnabled();
  expect(backgroundBar).toBeVisible();
});

it("sets up Google Meet from one button and shows progress without exposing manual fields", async () => {
  const blank = await mockInvoke<AttendeeSettingsStatus>("attendee_status");
  let finish!: (status: AttendeeSettingsStatus) => void;
  backend.attendeeSetup.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
  render(<SettingsPage serviceDesktop />);

  const setup = await screen.findByRole("button", { name: "Set up Google Meet" });
  expect(backend.attendeeSetup).not.toHaveBeenCalled();
  expect(screen.queryByLabelText("Attendee API key")).not.toBeInTheDocument();
  expect(screen.queryByRole("progressbar", { name: "Google Meet setup progress" })).not.toBeInTheDocument();
  vi.useFakeTimers();
  try {
    fireEvent.click(setup);
    expect(backend.attendeeSetup).toHaveBeenCalledOnce();
    expect(setup).toBeDisabled();
    const bar = screen.getByRole("progressbar", { name: "Google Meet setup progress" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(screen.getByLabelText("Elapsed 0:00")).toBeVisible();
    const progress = backend.subscribe.mock.calls.find(([event]) => event === "attendee_setup_progress")?.[1] as (value: AttendeeSetupProgress) => void;
    act(() => vi.advanceTimersByTime(2000));
    act(() => progress({ phase: "installing", message: null }));
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(screen.getByText("Installing Attendee…")).toBeInTheDocument();
    act(() => progress({ phase: "enablingWsl", message: null }));
    expect(screen.getByText("Enabling WSL 2 in Windows…")).toBeInTheDocument();
    act(() => progress({ phase: "preparing", message: null }));
    expect(screen.getByText("Preparing Google Meet setup…")).toBeInTheDocument();
    expect(bar).not.toHaveAttribute("aria-valuenow");
    act(() => progress({ phase: "checking", message: null }));
    expect(screen.getByText("Checking the connection…")).toBeInTheDocument();
    expect(bar).not.toHaveAttribute("aria-valuenow");
    act(() => vi.advanceTimersByTime(1000));
    expect(screen.getByLabelText("Elapsed 0:03")).toBeVisible();
    await act(async () => finish({ ...blank, configured: true, managed: true }));
    expect(bar).toHaveAttribute("aria-valuenow", "100");
    expect(screen.getByText("100%")).toBeVisible();
    act(() => vi.advanceTimersByTime(1000));
    expect(screen.getByLabelText("Elapsed 0:03")).toBeVisible();
  } finally {
    vi.useRealTimers();
  }
  expect(screen.getByText("Google Meet is ready")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Repair Google Meet setup" })).toBeEnabled();
  expect(backend.attendeeConfigure).not.toHaveBeenCalled();
});

it("keeps the Attendee API key masked and uses the dedicated configuration command", async () => {
  const settings = {
    apiUrl: "http://127.0.0.1:18760", callbackUrl: "wss://bazza-attendee.local:18761/audio", bindAddress: "127.0.0.1:18761",
    certificatePath: "C:/attendee/certificate.pem", privateKeyPath: "C:/attendee/private-key.pem",
  };
  backend.attendeeConfigure.mockResolvedValue({ ...settings, configured: true });
  render(<SettingsPage serviceDesktop />);
  fireEvent.click(await screen.findByRole("button", { name: "Advanced settings · use your own server" }));

  const key = await screen.findByLabelText("Attendee API key");
  expect(key).toHaveAttribute("type", "password");
  fireEvent.change(key, { target: { value: "test-key" } });
  fireEvent.change(screen.getByLabelText("Attendee server URL"), { target: { value: settings.apiUrl } });
  fireEvent.change(screen.getByLabelText("Secure audio callback URL"), { target: { value: settings.callbackUrl } });
  fireEvent.change(screen.getByLabelText("Local address and port"), { target: { value: settings.bindAddress } });
  fireEvent.change(screen.getByLabelText("TLS certificate path"), { target: { value: settings.certificatePath } });
  fireEvent.change(screen.getByLabelText("TLS private key path"), { target: { value: settings.privateKeyPath } });
  const save = screen.getByRole("button", { name: "Save Attendee settings" });
  fireEvent.click(save);
  expect(await screen.findByText("Attendee settings saved")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Set up Google Meet" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Repair Google Meet setup" })).not.toBeInTheDocument();
  expect(backend.attendeeConfigure).toHaveBeenCalledWith(settings, "test-key");
  expect(key).toHaveValue("");
  fireEvent.click(save);
  expect(backend.attendeeConfigure).toHaveBeenLastCalledWith(settings, null);
});

it("configures a bot with a masked token and saves a Discord channel link through dedicated commands", async () => {
  const bot = { id: "demo", name: "Demo bot" };
  const channel = { guild_id: "1234", guild_name: "Demo server", channel_id: "5678", channel_name: "Meetings" };
  const url = "https://discord.com/channels/1234/5678";
  backend.discordSaveToken.mockResolvedValue({ bot, channel: null });
  backend.discordSetChannel.mockResolvedValue(channel);
  render(<SettingsPage serviceDesktop />);
  fireEvent.click(await screen.findByRole("button", { name: "Continue to token" }));
  const token = screen.getByLabelText("Discord bot token");
  expect(token).toHaveAttribute("type", "password");
  fireEvent.change(token, { target: { value: "synthetic-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Verify and save" }));
  expect(await screen.findByText("Bot configured · Demo bot")).toBeInTheDocument();
  expect(screen.queryByLabelText("Discord bot token")).not.toBeInTheDocument();
  expect(backend.discordSaveToken).toHaveBeenCalledWith("synthetic-token");
  fireEvent.click(screen.getByRole("button", { name: "Bot added · choose channel" }));
  fireEvent.change(screen.getByLabelText("Discord voice channel link"), { target: { value: url } });
  fireEvent.click(screen.getByRole("button", { name: "Use link" }));
  expect(await screen.findByText("Default voice channel · Demo server / Meetings")).toBeInTheDocument();
  expect(backend.discordSetChannel).toHaveBeenCalledWith(url);
  expect(backend.setSettings).not.toHaveBeenCalled();
});

describe("capture settings", () => {
  it("does not open audio devices after leaving settings during a pending status check", async () => {
    let resolveStatus!: (value: { state: "idle" }) => void;
    backend.recordingStatus.mockReturnValue(new Promise((resolve) => { resolveStatus = resolve; }));
    const { unmount } = render(<SettingsPage />);
    await screen.findByLabelText("Frame rate");
    await waitFor(() => expect(backend.recordingStatus).toHaveBeenCalledTimes(1));
    unmount();
    await act(async () => { resolveStatus({ state: "idle" }); });
    expect(backend.sampleLevels).not.toHaveBeenCalled();
  });

  it("switches the displayed language while leaving speech and capture settings intact", async () => {
    settings.language = "en";
    await i18n.changeLanguage("ru");
    render(<SettingsPage />);
    expect(await screen.findByLabelText("Сервис распознавания")).toHaveValue("local");
    expect(screen.getByLabelText("Частота кадров")).toHaveValue("15");
    expect(screen.getByText("Язык распознавания речи")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Язык интерфейса"), { target: { value: "en" } });
    expect(await screen.findByLabelText("Transcription provider")).toHaveValue("local");
    expect(screen.getByLabelText("Maximum height")).toHaveValue("1080");

    fireEvent.change(screen.getByLabelText("Interface language"), { target: { value: "ru" } });
    expect(await screen.findByLabelText("Сервис распознавания")).toHaveValue("local");
    expect(backend.setUiLocale.mock.calls).toEqual([["en"], ["ru"]]);
    expect(backend.setSettings).not.toHaveBeenCalled();
    expect(settings.language).toBe("en");
    expect(settings.video_fps).toBe(15);
  });

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
    expect(backend.getCodexMcpStatus).not.toHaveBeenCalled();
    expect(backend.getClaudeMcpStatus).not.toHaveBeenCalled();
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

describe("Codex MCP access", () => {
  it("disables and enables access through the managed integration, independently of recording preferences", async () => {
    let finishDisable!: (status: CodexMcpStatus) => void;
    backend.setCodexMcpEnabled.mockReturnValueOnce(new Promise((resolve) => { finishDisable = resolve; }));
    render(<SettingsPage serviceDesktop />);
    const disable = await screen.findByRole("button", { name: "Disable access" });

    fireEvent.click(disable);
    expect(backend.setCodexMcpEnabled).toHaveBeenCalledWith(false);
    expect(disable).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent("Access enabled");
    await act(async () => { finishDisable({ enabled: false }); });
    expect(screen.getByRole("status")).toHaveTextContent("Access disabled");

    fireEvent.click(screen.getByRole("button", { name: "Enable access" }));
    await screen.findByRole("button", { name: "Disable access" });
    expect(backend.setCodexMcpEnabled).toHaveBeenLastCalledWith(true);
    expect(backend.setSettings).not.toHaveBeenCalled();
  });

  it("shows the actual revoked state when Codex configuration cleanup fails", async () => {
    backend.getCodexMcpStatus.mockResolvedValueOnce({ enabled: true }).mockResolvedValueOnce({ enabled: false });
    backend.setCodexMcpEnabled.mockRejectedValueOnce(new Error("Could not update the Codex configuration"));
    render(<SettingsPage serviceDesktop />);

    fireEvent.click(await screen.findByRole("button", { name: "Disable access" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not update the Codex configuration");
    expect(await screen.findByRole("button", { name: "Enable access" })).toBeEnabled();
    expect(screen.getByRole("status")).toHaveTextContent("Access disabled");
  });

  it("hides the integration when the managed setup is unavailable", async () => {
    backend.getCodexMcpStatus.mockResolvedValueOnce(null);
    render(<SettingsPage serviceDesktop />);
    await screen.findByLabelText("Frame rate");
    await waitFor(() => expect(screen.queryByRole("region", { name: "Codex access via MCP" })).not.toBeInTheDocument());
  });
});

describe("Claude MCP access", () => {
  it("controls both Claude clients separately from each other and Codex", async () => {
    backend.getClaudeMcpStatus.mockResolvedValue({ code_enabled: true, desktop_enabled: true });
    backend.setClaudeMcpEnabled
      .mockResolvedValueOnce({ code_enabled: false, desktop_enabled: true })
      .mockResolvedValueOnce({ code_enabled: false, desktop_enabled: false });
    render(<SettingsPage serviceDesktop />);

    const codex = within(await screen.findByRole("region", { name: "Codex access via MCP" }));
    const claude = within(await screen.findByRole("region", { name: "Claude access via MCP" }));
    const code = within(await claude.findByRole("group", { name: "Claude Code" }));
    const desktop = within(claude.getByRole("group", { name: "Claude Desktop" }));

    fireEvent.click(code.getByRole("button", { name: "Disable access" }));
    expect(await code.findByRole("button", { name: "Enable access" })).toBeEnabled();
    expect(desktop.getByRole("status")).toHaveTextContent("Access enabled");
    expect(codex.getByRole("status")).toHaveTextContent("Access enabled");

    fireEvent.click(desktop.getByRole("button", { name: "Disable access" }));
    expect(await desktop.findByRole("button", { name: "Enable access" })).toBeEnabled();
    expect(backend.setClaudeMcpEnabled.mock.calls).toEqual([["code", false], ["desktop", false]]);
    expect(backend.setCodexMcpEnabled).not.toHaveBeenCalled();
    expect(backend.setSettings).not.toHaveBeenCalled();
  });

  it("refreshes the grant state if removing Claude configuration fails", async () => {
    backend.getClaudeMcpStatus
      .mockResolvedValueOnce({ code_enabled: true, desktop_enabled: true })
      .mockResolvedValueOnce({ code_enabled: true, desktop_enabled: false });
    backend.setClaudeMcpEnabled.mockRejectedValueOnce(new Error("Could not update Claude Desktop configuration"));
    render(<SettingsPage serviceDesktop />);

    const claude = within(await screen.findByRole("region", { name: "Claude access via MCP" }));
    const desktop = within(await claude.findByRole("group", { name: "Claude Desktop" }));
    fireEvent.click(desktop.getByRole("button", { name: "Disable access" }));
    expect(await claude.findByRole("alert")).toHaveTextContent("Could not update Claude Desktop configuration");
    expect(await desktop.findByRole("button", { name: "Enable access" })).toBeEnabled();
  });
});

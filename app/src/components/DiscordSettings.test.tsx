import { openUrl } from "@tauri-apps/plugin-opener";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DiscordSettings } from "./DiscordSettings";
import { cmd, type DiscordChannel, type DiscordSettingsStatus } from "@/lib/ipc";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/lib/mock", () => ({ isMock: false }));
vi.mock("@/lib/ipc", () => ({
  cmd: {
    discordStatus: vi.fn(),
    discordOpenDeveloperPortal: vi.fn(),
    discordSaveToken: vi.fn(),
    discordDisconnect: vi.fn(),
    discordInviteUrl: vi.fn(),
    discordGuilds: vi.fn(),
    discordChannels: vi.fn(),
    discordSetChannel: vi.fn(),
  },
}));

const bot = { id: "123", name: "Test Recorder" };
const channel: DiscordChannel = { guild_id: "456", guild_name: "Test Server", channel_id: "789", channel_name: "Voice Room" };
const inviteUrl = "https://discord.com/oauth2/authorize?client_id=123&scope=bot&permissions=1049600";
const writeText = vi.fn();
const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(cmd.discordStatus).mockResolvedValue({ bot: null, channel: null });
  vi.mocked(cmd.discordOpenDeveloperPortal).mockResolvedValue(undefined);
  vi.mocked(cmd.discordSaveToken).mockResolvedValue({ bot, channel: null });
  vi.mocked(cmd.discordInviteUrl).mockResolvedValue(inviteUrl);
  vi.mocked(cmd.discordGuilds).mockResolvedValue([{ id: channel.guild_id, name: channel.guild_name }]);
  vi.mocked(cmd.discordChannels).mockResolvedValue([channel]);
  vi.mocked(cmd.discordSetChannel).mockResolvedValue(channel);
  writeText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
});

afterEach(() => {
  if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
  else Reflect.deleteProperty(navigator, "clipboard");
});

it("guides a new installation and finishes only after native channel validation", async () => {
  render(<DiscordSettings />);
  fireEvent.click(await screen.findByRole("button", { name: "Open Developer Portal" }));
  await waitFor(() => expect(cmd.discordOpenDeveloperPortal).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "Continue to token" }));
  const token = screen.getByLabelText("Discord bot token");
  expect(token).toHaveAttribute("type", "password");
  fireEvent.change(token, { target: { value: "  synthetic-token  " } });
  let resolveToken!: (result: DiscordSettingsStatus) => void;
  vi.mocked(cmd.discordSaveToken).mockReturnValueOnce(new Promise((resolve) => { resolveToken = resolve; }));
  vi.useFakeTimers();
  try {
    const verify = screen.getByRole("button", { name: "Verify and save" });
    fireEvent.click(verify);
    expect(verify).toBeDisabled();
    expect(screen.getByRole("progressbar", { name: "Discord validation progress" })).not.toHaveAttribute("aria-valuenow");
    expect(screen.getByText("Verifying the Discord bot token…")).toBeVisible();
    act(() => vi.advanceTimersByTime(2000));
    expect(screen.getByLabelText("Elapsed 0:02")).toBeVisible();
    await act(async () => resolveToken({ bot, channel: null }));
  } finally {
    vi.useRealTimers();
  }
  expect(screen.queryByRole("progressbar", { name: "Discord validation progress" })).not.toBeInTheDocument();
  await screen.findByText("Bot configured · Test Recorder");
  expect(cmd.discordSaveToken).toHaveBeenCalledWith("synthetic-token");
  expect(screen.queryByLabelText("Discord bot token")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Copy invite link" }));
  await screen.findByText("Link copied");
  expect(writeText).toHaveBeenCalledWith(inviteUrl);

  fireEvent.click(screen.getByRole("button", { name: "Bot added · choose channel" }));
  await screen.findByRole("option", { name: "Voice Room" });
  fireEvent.change(screen.getByLabelText("Discord voice channel", { selector: "select" }), { target: { value: channel.channel_id } });
  vi.mocked(cmd.discordSetChannel).mockRejectedValueOnce({ code: "native.discordChannelUnavailable" });
  fireEvent.click(screen.getByRole("button", { name: "Save channel" }));
  expect(screen.getByText("Checking the bot’s voice channel permissions…")).toBeVisible();
  expect(screen.getByRole("progressbar", { name: "Discord validation progress" })).not.toHaveAttribute("aria-valuenow");
  expect(await screen.findByRole("alert")).toHaveTextContent("Check View Channel and Connect permissions");
  expect(screen.queryByRole("progressbar", { name: "Discord validation progress" })).not.toBeInTheDocument();
  expect(screen.queryByText(/Setup complete/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Save channel" }));
  await screen.findByText(/Setup complete/);
  expect(cmd.discordSetChannel).toHaveBeenLastCalledWith("https://discord.com/channels/456/789");
});

it("shows the operating system error when launching the portal fails", async () => {
  vi.mocked(cmd.discordOpenDeveloperPortal).mockRejectedValueOnce({
    code: "native.discordPortalOpenFailed",
    detail: "No browser handler",
  });
  render(<DiscordSettings />);
  fireEvent.click(await screen.findByRole("button", { name: "Open Developer Portal" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not open Discord Developer Portal in the browser. No browser handler");
  expect(openUrl).not.toHaveBeenCalled();
  expect(writeText).not.toHaveBeenCalled();
});

it("copies the AI guide without the token or clipboard contents", async () => {
  render(<DiscordSettings />);
  expect(await screen.findByRole("button", { name: "Copy AI guide" })).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Continue to token" }));
  fireEvent.change(screen.getByLabelText("Discord bot token"), { target: { value: "private-test-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Copy AI guide" }));
  await screen.findByText("AI guide copied");
  const [guide] = writeText.mock.calls[0];
  expect(guide).toMatch(/^Step 1\. Create a Discord application for Bazza\./);
  expect(guide.split("\n\n")[0]).toContain("enter Bazza Recorder");
  expect(guide).toContain("https://discord.com/developers/applications");
  expect(guide).toContain("Never ask for my password, Discord bot token");
  expect(guide).toContain("Do not read the clipboard");
  expect(guide).not.toContain("private-test-token");
  expect(cmd.discordSaveToken).not.toHaveBeenCalled();
});

it("resumes from the saved channel and keeps it when replacing the bot fails", async () => {
  vi.mocked(cmd.discordStatus).mockResolvedValue({ bot, channel });
  vi.mocked(cmd.discordSaveToken).mockRejectedValueOnce({ code: "native.discordTokenInvalid" });
  render(<DiscordSettings />);
  expect(await screen.findByText("Default voice channel · Test Server / Voice Room")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Set up a different bot" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue to token" }));
  fireEvent.change(screen.getByLabelText("Discord bot token"), { target: { value: "invalid-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Verify and save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Discord rejected the bot token");
  expect(screen.getByText("Bot configured · Test Recorder")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.getByText("Default voice channel · Test Server / Voice Room")).toBeVisible();
  expect(cmd.discordDisconnect).not.toHaveBeenCalled();
});

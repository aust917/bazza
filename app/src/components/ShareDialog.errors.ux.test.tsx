import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ShareInfo } from "@/lib/ipc";
import { ShareDialog } from "./ShareDialog";

const api = vi.hoisted(() => ({ listShares: vi.fn(), setShareEnabled: vi.fn(), deleteShare: vi.fn() }));
const writeText = vi.hoisted(() => vi.fn());
const clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, "clipboard");
vi.mock("@/lib/ipc", () => ({ cmd: api }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn(), open: vi.fn() }));

const share: ShareInfo = {
  url: "https://synthetic.example.invalid/share/synthetic-token",
  share: {
    token: "synthetic-token", meeting_id: "synthetic-meeting", meeting_title: "Synthetic meeting",
    kind: "meeting", start_ms: null, end_ms: null, enabled: true, created_at: "2026-11-02T00:00:00Z",
  },
};
function dialog(meetingId = "synthetic-meeting") {
  return <ShareDialog meetingId={meetingId} title="Synthetic meeting" durationMs={60_000}
    hasVideo={false} currentMs={0} onClose={vi.fn()} />;
}
function renderDialog() {
  return render(dialog());
}
async function loadedDialog() {
  const rendered = renderDialog();
  await screen.findByText(share.url);
  return rendered;
}
beforeEach(() => {
  vi.resetAllMocks();
  api.listShares.mockResolvedValue([share]);
  api.setShareEnabled.mockResolvedValue(undefined);
  api.deleteShare.mockResolvedValue(undefined);
  writeText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
});
afterEach(() => {
  cleanup();
  if (clipboardDescriptor) Object.defineProperty(navigator, "clipboard", clipboardDescriptor);
  else Reflect.deleteProperty(navigator, "clipboard");
});

it("explains an initial link-list failure and retries loading without creating a share", async () => {
  api.listShares.mockRejectedValueOnce(new Error("Synthetic share list unavailable"));
  renderDialog();
  expect(await screen.findByRole("alert")).toHaveTextContent("Synthetic share list unavailable");
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByText(share.url);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(api.listShares).toHaveBeenCalledTimes(2);
});

it("keeps a failed link disable visible as enabled and lets the user retry", async () => {
  api.setShareEnabled.mockRejectedValueOnce(new Error("Synthetic disable failure"));
  await loadedDialog();
  fireEvent.click(screen.getByRole("switch", { name: "Link enabled" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Synthetic disable failure");
  expect(screen.getByRole("switch", { name: "Link enabled" })).toBeChecked();
  expect(screen.getByText(share.url)).toBeInTheDocument();
  api.listShares.mockResolvedValueOnce([{ ...share, share: { ...share.share, enabled: false } }]);
  await act(async () => { fireEvent.click(screen.getByRole("switch", { name: "Link enabled" })); });
  expect(screen.getByRole("switch", { name: "Link enabled" })).not.toBeChecked();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(api.setShareEnabled).toHaveBeenCalledTimes(2);
  expect(api.setShareEnabled).toHaveBeenLastCalledWith("synthetic-token", false);
});

it("keeps a failed deletion visible and removes the link only after a successful retry", async () => {
  api.deleteShare.mockRejectedValueOnce(new Error("Synthetic deletion failure"));
  await loadedDialog();
  fireEvent.click(screen.getByTitle("Delete link"));
  expect(await screen.findByRole("alert")).toHaveTextContent("Synthetic deletion failure");
  expect(screen.getByText(share.url)).toBeInTheDocument();
  api.listShares.mockResolvedValueOnce([]);
  await act(async () => { fireEvent.click(screen.getByTitle("Delete link")); });
  expect(screen.queryByText(share.url)).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(api.deleteShare).toHaveBeenCalledTimes(2);
});

it("announces clipboard denial and lets the same copy action retry", async () => {
  writeText.mockRejectedValueOnce(new Error("Synthetic clipboard denied"));
  await loadedDialog();
  fireEvent.click(screen.getByTitle("Copy link"));
  expect(await screen.findByRole("alert")).toHaveTextContent("Synthetic clipboard denied");
  expect(screen.getByText(share.url)).toBeInTheDocument();
  await act(async () => { fireEvent.click(screen.getByTitle("Copy link")); });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(writeText).toHaveBeenCalledTimes(2);
  expect(writeText).toHaveBeenLastCalledWith(share.url);
  expect(api.listShares).toHaveBeenCalledTimes(1);
});

it("retries a failed refresh without repeating the completed share mutation", async () => {
  api.listShares.mockResolvedValueOnce([share]).mockRejectedValueOnce(new Error("Synthetic refresh failure"))
    .mockResolvedValueOnce([{ ...share, share: { ...share.share, enabled: false } }]);
  await loadedDialog();
  fireEvent.click(screen.getByRole("switch", { name: "Link enabled" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Synthetic refresh failure");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Retry" })); });
  expect(screen.getByRole("switch", { name: "Link enabled" })).not.toBeChecked();
  expect(api.setShareEnabled).toHaveBeenCalledExactlyOnceWith("synthetic-token", false);
  expect(api.listShares).toHaveBeenCalledTimes(3);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("does not show a previous meeting's late load failure in the current dialog", async () => {
  let fail!: (reason: Error) => void;
  api.listShares.mockReturnValueOnce(new Promise<ShareInfo[]>((_resolve, reject) => { fail = reject; }));
  const other = { ...share, url: "https://synthetic.example.invalid/share/other", share: { ...share.share, meeting_id: "other-meeting" } };
  api.listShares.mockResolvedValueOnce([other]);
  const { rerender } = renderDialog();
  rerender(dialog("other-meeting"));
  await screen.findByText(other.url);
  await act(async () => { fail(new Error("Previous meeting's late failure")); });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.getByText(other.url)).toBeInTheDocument();
  expect(screen.queryByText(share.url)).not.toBeInTheDocument();
});

it("does not reload a closed dialog when an already requested action completes", async () => {
  let finish!: () => void;
  api.setShareEnabled.mockReturnValueOnce(new Promise<void>((resolve) => { finish = resolve; }));
  const { unmount } = await loadedDialog();
  fireEvent.click(screen.getByRole("switch", { name: "Link enabled" }));
  unmount();
  await act(async () => { finish(); });
  expect(api.listShares).toHaveBeenCalledTimes(1);
});

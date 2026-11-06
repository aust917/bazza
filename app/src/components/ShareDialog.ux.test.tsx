import { useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ShareDialog } from "./ShareDialog";

const backend = vi.hoisted(() => ({ listShares: vi.fn(), exportClip: vi.fn(), createShare: vi.fn(), exportMeetings: vi.fn(), exportHtml: vi.fn() }));
const dialogs = vi.hoisted(() => ({ save: vi.fn(), open: vi.fn() }));
vi.mock("@/lib/ipc", () => ({ cmd: backend }));
vi.mock("@tauri-apps/plugin-dialog", () => dialogs);

function Fixture() {
  const [open, setOpen] = useState(false);
  return <>
    <button onClick={() => setOpen(true)}>Open export</button>
    {open && <ShareDialog meetingId="synthetic" title="Synthetic meeting" durationMs={60_000} hasVideo={false} currentMs={0} onClose={() => setOpen(false)} />}
  </>;
}
async function openDialog() {
  render(<Fixture />);
  const trigger = screen.getByRole("button", { name: "Open export" });
  trigger.focus();
  fireEvent.click(trigger);
  await act(async () => {});
  return trigger;
}
beforeEach(() => {
  vi.resetAllMocks();
  backend.listShares.mockResolvedValue([]);
  dialogs.save.mockResolvedValue("synthetic-clip.mp3");
});
afterEach(cleanup);

it("keeps keyboard focus inside the named export dialog and returns it on Escape", async () => {
  const trigger = await openDialog();
  const dialog = screen.getByRole("dialog", { name: "Share or export" });
  expect(dialog).toHaveAttribute("aria-modal", "true");
  const close = screen.getByRole("button", { name: "Close" });
  const last = screen.getByRole("button", { name: "Web page (HTML)" });
  expect(close).toHaveFocus();
  fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
  expect(last).toHaveFocus();
  fireEvent.keyDown(last, { key: "Tab" });
  expect(close).toHaveFocus();
  fireEvent.keyDown(close, { key: "Escape" });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  expect(backend.createShare).not.toHaveBeenCalled();
});

it.each([
  ["nonsense", "0:30", "Enter a clip range"],
  ["-1:00", "0:30", "Enter a clip range"],
  ["0:40", "0:20", "The clip end must be after its start."],
  ["2:00", "3:00", "Choose a start before 1:00"],
])("explains invalid clip range %s–%s without opening a file picker", async (start, end, explanation) => {
  await openDialog();
  fireEvent.change(screen.getByRole("textbox", { name: "Clip start" }), { target: { value: start } });
  fireEvent.change(screen.getByRole("textbox", { name: "Clip end" }), { target: { value: end } });
  fireEvent.click(screen.getByRole("button", { name: "Save file" }));
  expect(screen.getByRole("alert")).toHaveTextContent(explanation);
  expect(screen.getByRole("textbox", { name: "Clip start" })).toHaveValue(start);
  expect(screen.getByRole("textbox", { name: "Clip end" })).toHaveValue(end);
  expect(dialogs.save).not.toHaveBeenCalled();
  expect(backend.exportClip).not.toHaveBeenCalled();
});

it("announces export progress and errors, preserves the chosen range and allows retry", async () => {
  let reject!: (error: Error) => void;
  backend.exportClip.mockReturnValueOnce(new Promise((_, fail) => { reject = fail; })).mockResolvedValueOnce(undefined);
  await openDialog();
  const start = screen.getByRole("textbox", { name: "Clip start" });
  fireEvent.change(start, { target: { value: "0:10" } });
  const save = screen.getByRole("button", { name: "Save file" });
  fireEvent.click(save);
  await act(async () => {});
  expect(screen.getByRole("progressbar", { name: "Saving the clip" })).not.toHaveAttribute("aria-valuenow");
  expect(screen.getByRole("dialog")).toHaveAttribute("aria-busy", "true");
  expect(save).toBeDisabled();
  await act(async () => { reject(new Error("Synthetic disk full")); });
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent("Synthetic disk full");
  expect(start).toHaveValue("0:10");
  expect(save).toBeEnabled();
  fireEvent.click(save);
  await act(async () => {});
  expect(screen.getByRole("status")).toHaveTextContent("synthetic-clip.mp3");
  expect(backend.exportClip).toHaveBeenCalledTimes(2);
  expect(backend.exportClip).toHaveBeenLastCalledWith("synthetic", 10_000, 30_000, "synthetic-clip.mp3");
});

it("does not report an export or invoke it while the destination picker is open or canceled", async () => {
  let choose!: (path: string | null) => void;
  dialogs.save.mockReturnValueOnce(new Promise<string | null>((resolve) => { choose = resolve; }));
  await openDialog();
  const save = screen.getByRole("button", { name: "Save file" });
  fireEvent.click(save);
  expect(save).toBeDisabled();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  expect(backend.exportClip).not.toHaveBeenCalled();

  await act(async () => { choose(null); });
  expect(save).toBeEnabled();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(backend.exportClip).not.toHaveBeenCalled();
  expect(backend.listShares).toHaveBeenCalledTimes(1);
});

it("keeps missing-media actions from creating a share or opening an export picker", async () => {
  render(<ShareDialog meetingId="text-only" title="Text-only meeting" durationMs={60_000} hasAudio={false} hasVideo={false} currentMs={0} onClose={() => {}} />);
  await act(async () => {});
  expect(screen.getByRole("textbox", { name: "Clip start" })).toBeDisabled();
  expect(screen.getByRole("textbox", { name: "Clip end" })).toBeDisabled();
  for (const name of ["Share whole meeting", "Link", "Save file", ".bazza bundle", "Web page (HTML)"]) {
    const action = screen.getByRole("button", { name });
    expect(action).toBeDisabled();
    fireEvent.click(action);
  }
  expect(backend.createShare).not.toHaveBeenCalled();
  expect(backend.exportClip).not.toHaveBeenCalled();
  expect(backend.exportMeetings).not.toHaveBeenCalled();
  expect(backend.exportHtml).not.toHaveBeenCalled();
  expect(dialogs.save).not.toHaveBeenCalled();
  expect(dialogs.open).not.toHaveBeenCalled();
});

it("allows a real video clip without a separate audio path while keeping the audio-required bundle disabled", async () => {
  dialogs.save.mockResolvedValue("synthetic-clip.mp4");
  backend.exportClip.mockResolvedValue(undefined);
  render(<ShareDialog meetingId="video-only" title="Video meeting" durationMs={60_000} hasAudio={false} hasVideo currentMs={0} onClose={() => {}} />);
  await act(async () => {});
  expect(screen.getByRole("button", { name: ".bazza bundle" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Share whole meeting" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "Save file" }));
  await act(async () => {});
  expect(dialogs.save).toHaveBeenCalledWith(expect.objectContaining({ defaultPath: "Video meeting-clip.mp4" }));
  expect(backend.exportClip).toHaveBeenCalledExactlyOnceWith("video-only", 0, 30_000, "synthetic-clip.mp4");
});

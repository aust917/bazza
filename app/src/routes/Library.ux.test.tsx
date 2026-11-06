import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Events, Folder, Meeting, MeetingFilter } from "@/lib/ipc";
import { Library } from "./Library";

const backend = vi.hoisted(() => ({
  listMeetingsFiltered: vi.fn(), listFolders: vi.fn(), listTags: vi.fn(), listUpcoming: vi.fn(),
  search: vi.fn(), importRecording: vi.fn(), moveMeeting: vi.fn(),
}));
const dialog = vi.hoisted(() => ({ open: vi.fn() }));
const events = vi.hoisted(() => new Map<string, (payload: Events["tauri://drag-drop"]) => void>());
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: (name: string, handler: (payload: Events["tauri://drag-drop"]) => void) => {
  events.set(name, handler);
  return () => events.delete(name);
} }));
vi.mock("@tauri-apps/plugin-dialog", () => dialog);

function recording(id: string, title: string): Meeting {
  return {
    id, title, app: "import", started_at: "2026-11-02T12:00:00Z", ended_at: null,
    duration_ms: 60_000, has_video: false, status: "ready", folder_id: null,
    created_at: "2026-11-02T12:00:00Z", error: null, title_auto: false, participants: [], calendar_uid: null,
  };
}

const project: Folder = { id: "project", name: "Project", parent_id: null, default_template_id: null, meeting_count: 0 };
const mouse = { pointerType: "mouse", isPrimary: true, pointerId: 1, button: 0, buttons: 1, clientX: 620, clientY: 225 };
const originalHitTest = Object.getOwnPropertyDescriptor(document, "elementFromPoint");

function mockHitTest(target: Element) {
  const hitTest = vi.fn().mockReturnValue(target);
  Object.defineProperty(document, "elementFromPoint", { configurable: true, value: hitTest });
  return hitTest;
}

function CurrentLocation() { return <output aria-label="Current page">{useLocation().pathname}</output>; }
function renderLibrary() { return render(<MemoryRouter><Library /><CurrentLocation /></MemoryRouter>); }

beforeEach(() => {
  vi.resetAllMocks();
  backend.listMeetingsFiltered.mockResolvedValue([recording("existing", "Existing recording")]);
  backend.listFolders.mockResolvedValue([]);
  backend.listTags.mockResolvedValue([]);
  backend.listUpcoming.mockResolvedValue([]);
  backend.search.mockResolvedValue([]);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  if (originalHitTest) Object.defineProperty(document, "elementFromPoint", originalHitTest);
  else Reflect.deleteProperty(document, "elementFromPoint");
});

it("lets the user cancel transcript selection without importing media or leaving the library", async () => {
  dialog.open.mockResolvedValueOnce("C:\\fixtures\\meeting.wav").mockResolvedValueOnce(null);
  renderLibrary();
  await screen.findByText("Existing recording");
  fireEvent.click(screen.getByRole("button", { name: "Media + transcript" }));
  await waitFor(() => expect(dialog.open).toHaveBeenCalledTimes(2));
  expect(dialog.open).toHaveBeenLastCalledWith(expect.objectContaining({
    title: "Choose its existing transcript", multiple: false,
  }));
  expect(backend.importRecording).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Current page")).toHaveTextContent("/");
  expect(screen.getByRole("button", { name: "Media + transcript" })).toBeEnabled();
  expect(screen.getByText("Existing recording")).toBeInTheDocument();
});

it("shows a pending import, prevents duplicate imports from an empty library, and permits retry after a partial failure", async () => {
  backend.listMeetingsFiltered.mockResolvedValue([]);
  let finishImport!: (meeting: Meeting) => void;
  let failImport!: (error: Error) => void;
  const pendingImport = new Promise<Meeting>((resolve) => { finishImport = resolve; });
  const pendingFailure = new Promise<Meeting>((_resolve, reject) => { failImport = reject; });
  backend.importRecording.mockReturnValueOnce(pendingImport)
    .mockReturnValueOnce(pendingFailure)
    .mockResolvedValueOnce(recording("retry", "Retried recording"));
  dialog.open.mockResolvedValueOnce(["C:\\fixtures\\good.wav", "C:\\fixtures\\bad.wav"])
    .mockResolvedValueOnce(["C:\\fixtures\\bad.wav"]);
  const alert = vi.spyOn(window, "alert").mockImplementation(() => {});
  renderLibrary();
  await screen.findByText("Nothing recorded yet.");
  fireEvent.click(screen.getByRole("button", { name: "Import" }));
  const pendingButton = await screen.findByRole("button", { name: "Importing..." });
  const progress = screen.getByRole("progressbar", { name: "Importing media" });
  expect(progress).toHaveAttribute("aria-valuenow", "0");
  expect(screen.getByText("good.wav")).toBeVisible();
  expect(pendingButton).toBeDisabled();
  expect(screen.getByRole("button", { name: "Media + transcript" })).toBeDisabled();
  const emptyImport = screen.getByRole("button", { name: "Import a file" });
  expect(emptyImport).toBeDisabled();
  fireEvent.click(pendingButton);
  fireEvent.click(emptyImport);
  expect(dialog.open).toHaveBeenCalledTimes(1);
  expect(backend.importRecording).toHaveBeenCalledTimes(1);
  expect(alert).not.toHaveBeenCalled();

  backend.listMeetingsFiltered.mockResolvedValue([recording("good", "Imported recording")]);
  await act(async () => { finishImport(recording("good", "Imported recording")); });
  expect(progress).toHaveAttribute("aria-valuenow", "50");
  expect(screen.getByText("1 of 2 files processed", { exact: false })).toBeVisible();
  expect(screen.getByText("bad.wav")).toBeVisible();
  await act(async () => { failImport(new Error("Audio decoder could not read this file")); });
  expect(await screen.findByText("Imported recording")).toBeInTheDocument();
  expect(progress).not.toBeInTheDocument();
  expect(alert).toHaveBeenCalledExactlyOnceWith(expect.stringContaining(
    "bad.wav: Error: Audio decoder could not read this file",
  ));
  expect(screen.getByLabelText("Current page")).toHaveTextContent("/");
  const retryButton = screen.getByRole("button", { name: "Import" });
  expect(retryButton).toBeEnabled();
  fireEvent.click(retryButton);
  await waitFor(() => expect(screen.getByLabelText("Current page")).toHaveTextContent("/meeting/retry"));
  expect(backend.importRecording).toHaveBeenLastCalledWith("C:\\fixtures\\bad.wav");
  expect(alert).toHaveBeenCalledTimes(1);
});

it("opens recording setup from the empty library", async () => {
  backend.listMeetingsFiltered.mockResolvedValue([]);
  renderLibrary();
  fireEvent.click(await screen.findByRole("button", { name: "Record now" }));
  expect(screen.getByLabelText("Current page")).toHaveTextContent("/recording");
});

it("imports dropped media immediately, refreshes All meetings, and reports skipped formats", async () => {
  const path = "C:\\fixtures\\meeting.WAV";
  const result = recording("imported", "Dropped recording");
  backend.listFolders.mockResolvedValue([project]);
  backend.importRecording.mockImplementation(async () => {
    backend.listMeetingsFiltered.mockResolvedValue([result]);
    return result;
  });
  const { unmount } = renderLibrary();
  fireEvent.click(await screen.findByRole("button", { name: "Project" }));
  const hitTest = mockHitTest(screen.getByRole("heading", { name: "Project" }));
  const position = { x: 620 * window.devicePixelRatio, y: 225 * window.devicePixelRatio };
  const drop = events.get("tauri://drag-drop")!;

  await act(async () => drop({ paths: [path, path, "C:\\fixtures\\notes.srt"], position }));

  expect(hitTest).toHaveBeenCalledWith(620, 225);
  expect(dialog.open).not.toHaveBeenCalled();
  expect(backend.importRecording).toHaveBeenCalledExactlyOnceWith(path);
  expect(await screen.findByRole("heading", { name: "Library" })).toBeVisible();
  expect(await screen.findByText("Dropped recording")).toBeVisible();
  expect(screen.getByRole("alert")).toHaveTextContent("Unsupported file format: notes.srt");
  expect(screen.getByLabelText("Current page")).toHaveTextContent(/^\/$/);
  unmount();
  expect(events.has("tauri://drag-drop")).toBe(false);
});

it("ignores drops outside the library and skips batches without supported files", async () => {
  renderLibrary();
  await screen.findByText("Existing recording");
  const hitTest = mockHitTest(screen.getByLabelText("Current page"));
  const position = { x: 620, y: 225 };
  const drop = events.get("tauri://drag-drop")!;

  await act(async () => drop({ paths: ["C:\\fixtures\\meeting.wav"], position }));
  expect(backend.importRecording).not.toHaveBeenCalled();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();

  hitTest.mockReturnValue(screen.getByRole("heading", { name: "Library" }));
  await act(async () => drop({ paths: ["C:\\fixtures\\notes.srt"], position }));
  expect(backend.importRecording).not.toHaveBeenCalled();
  expect(screen.getByRole("alert")).toHaveTextContent("Unsupported file format: notes.srt");
});

it("moves a recording from its keyboard-accessible menu and refreshes folder counts and contents", async () => {
  let folderId: string | null = null;
  backend.listMeetingsFiltered.mockImplementation(async (filter: MeetingFilter) =>
    filter.folder === null || filter.folder.includes(folderId)
      ? [{ ...recording("existing", "Existing recording"), folder_id: folderId }]
      : []);
  backend.listFolders.mockImplementation(async () => [{ ...project, meeting_count: folderId ? 1 : 0 }]);
  backend.moveMeeting.mockImplementation(async (_id: string, destination: string | null) => { folderId = destination; });
  renderLibrary();

  const move = await screen.findByRole("button", { name: "Move to folder · Existing recording" });
  fireEvent.keyDown(move, { key: "ArrowDown" });
  const destination = screen.getByRole("menuitem", { name: "Project" });
  expect(destination).toHaveFocus();
  expect(screen.getByRole("menuitem", { name: "Unfiled" })).toBeDisabled();
  fireEvent.click(destination);
  expect(backend.moveMeeting).toHaveBeenLastCalledWith("existing", "project");
  expect(await screen.findByText("in Project")).toBeInTheDocument();
  expect(screen.getByLabelText("Current page")).toHaveTextContent("/");

  const projectButton = screen.getByRole("button", { name: /^Project/ });
  expect(await within(projectButton).findByText("1")).toBeInTheDocument();
  fireEvent.click(projectButton);
  await waitFor(() => expect(backend.listMeetingsFiltered).toHaveBeenLastCalledWith({ folder: ["project"], tag_id: null }));
  fireEvent.click(screen.getByRole("button", { name: "Move to folder · Existing recording" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Unfiled" }));
  expect(backend.moveMeeting).toHaveBeenLastCalledWith("existing", null);
  expect(await screen.findByText("Nothing here.")).toBeInTheDocument();
  expect(await screen.findByRole("button", { name: "Project" })).toBeInTheDocument();
});

it("moves a recording released over a highlighted folder and reports a failure for retry", async () => {
  backend.listFolders.mockResolvedValue([project]);
  backend.moveMeeting.mockRejectedValueOnce(new Error("Library is busy")).mockResolvedValueOnce(undefined);
  renderLibrary();

  const source = await screen.findByRole("link", { name: /Existing recording/ });
  const target = screen.getByRole("button", { name: "Project" });
  const destination = target.closest("[data-meeting-drop-target]");
  const hitTest = mockHitTest(target);
  fireEvent.pointerDown(source, mouse);
  fireEvent.pointerMove(source, { ...mouse, clientX: 623 });
  expect(hitTest).not.toHaveBeenCalled();
  expect(destination).not.toHaveClass("ring-2");
  fireEvent.pointerMove(source, { ...mouse, clientX: 328, clientY: 173 });
  expect(destination).toHaveClass("ring-2");
  fireEvent.pointerUp(source, { ...mouse, buttons: 0, clientX: 328, clientY: 173 });
  expect(destination).not.toHaveClass("ring-2");
  expect(backend.moveMeeting).toHaveBeenLastCalledWith("existing", "project");
  fireEvent.click(source, { detail: 1 });
  expect(screen.getByLabelText("Current page")).toHaveTextContent(/^\/$/);
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not move the meeting. Error: Library is busy");

  fireEvent.click(screen.getByRole("button", { name: "Move to folder · Existing recording" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Project" }));
  expect(backend.moveMeeting).toHaveBeenCalledTimes(2);
  await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
});

it("opens a recording on a normal click with small pointer movement", async () => {
  renderLibrary();
  const source = await screen.findByRole("link", { name: /Existing recording/ });
  fireEvent.pointerDown(source, mouse);
  fireEvent.pointerMove(source, { ...mouse, clientX: 623 });
  fireEvent.pointerUp(source, { ...mouse, buttons: 0, clientX: 623 });
  fireEvent.click(source, { detail: 1 });
  expect(screen.getByLabelText("Current page")).toHaveTextContent("/meeting/existing");
  expect(backend.moveMeeting).not.toHaveBeenCalled();
});

it.each(["pointercancel", "blur", "outside release", "scroll away from target"] as const)("clears a pending move on %s", async (reason) => {
  backend.listFolders.mockResolvedValue([project]);
  renderLibrary();
  const source = await screen.findByRole("link", { name: /Existing recording/ });
  const target = screen.getByRole("button", { name: "Project" });
  const destination = target.closest("[data-meeting-drop-target]");
  const hitTest = mockHitTest(target);
  fireEvent.pointerDown(source, mouse);
  fireEvent.pointerMove(source, { ...mouse, clientX: 328, clientY: 173 });
  expect(destination).toHaveClass("ring-2");

  if (reason === "pointercancel") fireEvent.pointerCancel(source, mouse);
  if (reason === "blur") fireEvent.blur(window);
  if (reason === "outside release") hitTest.mockReturnValue(source);
  if (reason === "scroll away from target") {
    hitTest.mockReturnValue(source);
    fireEvent.scroll(target);
    expect(destination).not.toHaveClass("ring-2");
  }
  fireEvent.pointerUp(source, { ...mouse, buttons: 0, clientX: 328, clientY: 173 });

  expect(destination).not.toHaveClass("ring-2");
  expect(backend.moveMeeting).not.toHaveBeenCalled();
  fireEvent.click(source, { detail: 0 });
  expect(screen.getByLabelText("Current page")).toHaveTextContent("/meeting/existing");
});

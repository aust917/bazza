import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, expect, it, vi } from "vitest";
import type { Folder, Meeting, MeetingFilter } from "@/lib/ipc";
import { Library } from "./Library";

const cmd = vi.hoisted(() => ({
  listMeetings: vi.fn(), listMeetingsFiltered: vi.fn(), listFolders: vi.fn(), listTags: vi.fn(),
  listUpcoming: vi.fn(), renameMeeting: vi.fn(),
  serviceLibraryOrganization: vi.fn(), mutateServiceLibraryOrganization: vi.fn(), moveMeeting: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd, subscribe: () => () => {} }));

const meeting: Meeting = {
  id: "meeting-1", title: "Service meeting", app: null, started_at: "2026-11-03T00:00:00Z", ended_at: null,
  duration_ms: 100, has_video: false, status: "ready", folder_id: null, created_at: "2026-11-03T00:00:00Z",
  error: null, title_auto: false, participants: [], calendar_uid: null, metadata_revision: "metadata-1", lifecycle_revision: "life-1",
};
const folder: Folder = { id: "project", name: "Project", parent_id: null, default_template_id: null, meeting_count: 0, revision: "folder-1" };

beforeEach(() => {
  vi.resetAllMocks();
  cmd.serviceLibraryOrganization.mockResolvedValue({ folders: [], tags: [] });
});

it("uses the ordinary list and sends the displayed revision when renaming in service mode", async () => {
  cmd.listMeetings.mockResolvedValueOnce([meeting]).mockResolvedValue([{ ...meeting, title: "Renamed", metadata_revision: "metadata-2" }]);
  cmd.renameMeeting.mockResolvedValue(meeting);
  render(<MemoryRouter><Library serviceDesktop /></MemoryRouter>);
  await screen.findByText("Service meeting");
  fireEvent.click(screen.getByRole("button", { name: "Rename" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Rename" }), { target: { value: "Renamed" } });
  fireEvent.click(screen.getByRole("button", { name: "Rename" }));
  await waitFor(() => expect(cmd.renameMeeting).toHaveBeenCalledExactlyOnceWith("meeting-1", "Renamed", "metadata-1", expect.any(String)));
  await screen.findByText("Renamed");
  for (const command of [cmd.listMeetingsFiltered, cmd.listFolders, cmd.listTags, cmd.listUpcoming]) {
    expect(command).not.toHaveBeenCalled();
  }
});

it("moves a meeting by pointer and by menu through the service with current revisions and counts", async () => {
  let current = { ...meeting };
  cmd.listMeetings.mockImplementation(async () => [current]);
  cmd.listMeetingsFiltered.mockImplementation(async (filter: MeetingFilter) => filter.folder?.includes(current.folder_id) ? [current] : []);
  cmd.serviceLibraryOrganization.mockImplementation(async () => ({ folders: [{ ...folder, meeting_count: current.folder_id ? 1 : 0 }], tags: [] }));
  cmd.mutateServiceLibraryOrganization.mockImplementation(async (request) => {
    current = { ...current, folder_id: request.folder_id, metadata_revision: "metadata-2" };
  });
  const { unmount } = render(<MemoryRouter><Library serviceDesktop /></MemoryRouter>);
  const link = await screen.findByRole("link", { name: /Service meeting/ });
  const project = await screen.findByRole("button", { name: "Project" });
  const descriptor = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
  Object.defineProperty(document, "elementFromPoint", { configurable: true, value: vi.fn(() => project) });
  try {
    const pointer = { pointerType: "mouse", isPrimary: true, pointerId: 1, button: 0, buttons: 1, clientX: 620, clientY: 225 };
    fireEvent.pointerDown(within(link).getByText("Service meeting"), pointer);
    fireEvent.pointerMove(project, { ...pointer, clientX: 300 });
    expect(project.closest("[data-meeting-drop-target]")).toHaveClass("ring-2");
    fireEvent.pointerUp(project, { ...pointer, buttons: 0, clientX: 300 });
  } finally {
    if (descriptor) Object.defineProperty(document, "elementFromPoint", descriptor);
    else Reflect.deleteProperty(document, "elementFromPoint");
  }
  expect(cmd.mutateServiceLibraryOrganization).toHaveBeenLastCalledWith({
    op: "meeting_move", meeting_id: meeting.id, folder_id: folder.id,
    expected_revision: "metadata-1", idempotency_key: expect.any(String),
  });
  expect(await within(project).findByText("1")).toBeVisible();
  unmount();

  render(<MemoryRouter><Library serviceDesktop /></MemoryRouter>);
  fireEvent.click(await screen.findByRole("button", { name: /^Project/ }));
  await waitFor(() => expect(cmd.listMeetingsFiltered).toHaveBeenLastCalledWith({ folder: [folder.id], tag_id: null }));
  fireEvent.click(await screen.findByRole("button", { name: "Move to folder · Service meeting" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Unfiled" }));
  expect(cmd.mutateServiceLibraryOrganization).toHaveBeenLastCalledWith({
    op: "meeting_move", meeting_id: meeting.id, folder_id: null,
    expected_revision: "metadata-2", idempotency_key: expect.any(String),
  });
  expect(await screen.findByText("Nothing here.")).toBeVisible();
  expect(await screen.findByRole("button", { name: "Project" })).toBeVisible();
  expect(cmd.moveMeeting).not.toHaveBeenCalled();
});

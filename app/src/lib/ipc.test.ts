import { beforeEach, expect, it, vi } from "vitest";

const bridge = vi.hoisted(() => ({ invoke: vi.fn().mockResolvedValue([]), webviewListen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: bridge.invoke }));
vi.mock("@tauri-apps/api/webview", () => ({ getCurrentWebview: () => ({ listen: bridge.webviewListen }) }));
vi.mock("./mock", () => ({ isMock: false, mockInvoke: vi.fn(), mockListen: vi.fn() }));

beforeEach(() => { vi.resetModules(); bridge.invoke.mockClear(); });

const folders: (null | (string | null)[])[] = [null, [null], ["folder-id"]];
it.each(folders.map((folder) => ({ folder })))("preserves the native folder wire shape $folder", async ({ folder }) => {
  const { cmd } = await import("./ipc");
  await expect(cmd.listMeetingsFiltered({ folder, tag_id: "tag-id" })).resolves.toEqual([]);
  expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("list_meetings_filtered", { filter: { folder, tag_id: "tag-id" } });
});

it("listens for native drops on the current webview and releases a late subscription", async () => {
  const { subscribe } = await import("./ipc");
  const unlisten = vi.fn();
  bridge.webviewListen.mockResolvedValueOnce(unlisten);
  const cleanup = subscribe("tauri://drag-drop", vi.fn());
  expect(bridge.webviewListen).toHaveBeenCalledExactlyOnceWith("tauri://drag-drop", expect.any(Function));
  cleanup();
  await Promise.resolve();
  expect(unlisten).toHaveBeenCalledOnce();
});

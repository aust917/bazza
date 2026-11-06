import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChatMessage, Events } from "@/lib/ipc";
import { ChatPanel } from "./ChatPanel";

const api = vi.hoisted(() => ({ listChats: vi.fn(), chatMessages: vi.fn(), createChat: vi.fn(), sendChat: vi.fn(), cancelChat: vi.fn() }));
const listeners = vi.hoisted(() => new Map<string, Set<(payload: unknown) => void>>());
const originalScroll = Object.getOwnPropertyDescriptor(Element.prototype, "scrollIntoView");
vi.mock("@/lib/ipc", () => ({ cmd: api, subscribe: (name: string, handler: (payload: unknown) => void) => {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler);
  listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));

function emit<K extends keyof Events>(name: K, payload: Events[K]) {
  listeners.get(name)?.forEach((handler) => handler(payload));
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
function answer(chat_id: string, content: string): ChatMessage {
  return { id: `${chat_id}-answer`, chat_id, role: "assistant", content, citations: [], created_at: "2026-11-02T00:00:00Z" };
}

beforeEach(() => {
  vi.resetAllMocks();
  listeners.clear();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  api.listChats.mockResolvedValue([{ id: "chat", title: "Discussion" }]);
  api.chatMessages.mockResolvedValue([]);
  api.sendChat.mockResolvedValue({
    id: "answer", chat_id: "chat", role: "assistant", content: "", citations: [], created_at: "2026-11-02T00:00:00Z",
  } satisfies ChatMessage);
});

it("shows elapsed time from send through the first reply token and clears progress on completion", async () => {
  vi.useFakeTimers();
  const pending = deferred<ChatMessage>();
  api.sendChat.mockReturnValueOnce(pending.promise);
  render(<ChatPanel scopeKind="all" />);
  await act(async () => {});
  const idleTimers = vi.getTimerCount();
  const composer = screen.getByRole("textbox");
  fireEvent.change(composer, { target: { value: "Question" } });
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(screen.getByRole("progressbar", { name: "Waiting for the model" })).not.toHaveAttribute("aria-valuenow");
  await act(() => vi.advanceTimersByTimeAsync(2000));
  expect(screen.getByText("0:02")).toBeInTheDocument();

  const placeholder = answer("chat", "");
  await act(async () => { pending.resolve(placeholder); });
  expect(screen.getByRole("progressbar", { name: "Waiting for the model" })).toBeInTheDocument();
  act(() => emit("chat_token", { chat_id: "chat", message_id: placeholder.id, delta: "First token" }));
  expect(screen.getByRole("progressbar", { name: "Receiving the reply" })).not.toHaveAttribute("aria-valuenow");
  expect(screen.getByText("0:02")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Stop generating" }));
  expect(api.cancelChat).toHaveBeenCalledExactlyOnceWith("chat");
  await act(async () => emit("chat_done", { chat_id: "chat", message: { ...placeholder, content: "Final reply" }, error: null }));
  expect(screen.getByText("Final reply")).toBeInTheDocument();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(1000));
  expect(vi.getTimerCount()).toBeLessThanOrEqual(idleTimers);
});

it.each(["resolve", "reject"] as const)("isolates a pending send that %s after the user selects another chat", async (outcome) => {
  const pending = deferred<ChatMessage>();
  api.listChats.mockResolvedValue([{ id: "chat", title: "First discussion" }, { id: "second", title: "Second discussion" }]);
  api.chatMessages.mockImplementation(async (id: string) => [answer(id, `${id} history`)]);
  api.sendChat.mockReturnValueOnce(pending.promise);
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  const composer = screen.getByRole("textbox");
  fireEvent.change(composer, { target: { value: "Question for the first chat" } });
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Second discussion" }));
  await act(async () => {});
  expect(screen.getByText("second history")).toBeInTheDocument();
  expect(composer).toBeEnabled();
  fireEvent.change(composer, { target: { value: "Draft for the second chat" } });

  await act(async () => {
    if (outcome === "resolve") pending.resolve(answer("chat", "First chat late answer"));
    else pending.reject(new Error("First chat late failure"));
  });
  expect(composer).toHaveValue("Draft for the second chat");
  expect(screen.queryByText("First chat late answer")).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Stop generating" })).not.toBeInTheDocument();
  expect(screen.getByText("second history")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "First discussion" }));
  await act(async () => {});
  if (outcome === "reject") {
    expect(composer).toHaveValue("Question for the first chat");
    expect(screen.getByRole("alert")).toHaveTextContent("First chat late failure");
  } else {
    fireEvent.click(screen.getByRole("button", { name: "Stop generating" }));
    expect(api.cancelChat).toHaveBeenCalledExactlyOnceWith("chat");
  }
});

it("keeps running-chat controls tied to their chat when navigating away and returning", async () => {
  api.listChats.mockResolvedValue([{ id: "chat", title: "First discussion" }, { id: "second", title: "Second discussion" }]);
  api.chatMessages.mockImplementation(async (id: string) => [answer(id, `${id} history`)]);
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  const composer = screen.getByRole("textbox");
  fireEvent.change(composer, { target: { value: "Question for the first chat" } });
  await act(async () => { fireEvent.keyDown(composer, { key: "Enter" }); });
  expect(screen.getByRole("button", { name: "Stop generating" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Second discussion" }));
  await act(async () => {});
  expect(screen.queryByRole("button", { name: "Stop generating" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Send message" })).toBeInTheDocument();
  expect(api.cancelChat).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "First discussion" }));
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Stop generating" }));
  expect(api.cancelChat).toHaveBeenCalledExactlyOnceWith("chat");

  fireEvent.click(screen.getByRole("button", { name: "Second discussion" }));
  await act(async () => {});
  act(() => emit("chat_done", { chat_id: "chat", message: answer("chat", "Completed while away"), error: null }));
  expect(screen.queryByText("Completed while away")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "First discussion" }));
  await act(async () => {});
  expect(screen.queryByRole("button", { name: "Stop generating" })).not.toBeInTheDocument();
});

it("ignores an older history response after selecting a different chat", async () => {
  const oldHistory = deferred<ChatMessage[]>();
  api.listChats.mockResolvedValue([{ id: "chat", title: "First discussion" }, { id: "second", title: "Second discussion" }]);
  api.chatMessages.mockImplementation((id: string) => id === "chat" ? oldHistory.promise : Promise.resolve([answer(id, "Second history")]));
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Second discussion" }));
  await act(async () => {});
  await act(async () => { oldHistory.resolve([answer("chat", "Old first history")]); });
  expect(screen.getByText("Second history")).toBeInTheDocument();
  expect(screen.queryByText("Old first history")).not.toBeInTheDocument();
});

it("isolates pending replies and drafts when the meeting scope changes", async () => {
  const pending = deferred<ChatMessage>();
  api.listChats.mockImplementation(async (_kind: string, id: string) => [{ id, title: `${id} discussion` }]);
  api.chatMessages.mockImplementation(async (id: string) => [answer(id, `${id} history`)]);
  api.sendChat.mockReturnValueOnce(pending.promise);
  const onSeek = vi.fn();
  const { rerender } = render(<ChatPanel scopeKind="meeting" scopeId="first-meeting" onSeek={onSeek} />);
  await act(async () => {});
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Old meeting question" } });
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "Enter" });
  rerender(<ChatPanel scopeKind="meeting" scopeId="second-meeting" onSeek={onSeek} />);
  await act(async () => {});
  expect(screen.getByText("second-meeting history")).toBeInTheDocument();
  const composer = screen.getByRole("textbox");
  expect(composer).toBeEnabled();
  fireEvent.change(composer, { target: { value: "New meeting draft" } });
  await act(async () => { pending.resolve(answer("first-meeting", "Old meeting reply")); });
  expect(composer).toHaveValue("New meeting draft");
  expect(screen.queryByText("Old meeting reply")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Stop generating" })).not.toBeInTheDocument();
});

it("preserves live text when an older history snapshot arrives after returning to a running chat", async () => {
  vi.useFakeTimers();
  const snapshot = deferred<ChatMessage[]>();
  api.listChats.mockResolvedValue([{ id: "chat", title: "First discussion" }, { id: "second", title: "Second discussion" }]);
  api.chatMessages.mockResolvedValueOnce([]).mockResolvedValueOnce([]).mockReturnValueOnce(snapshot.promise);
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Question" } });
  await act(async () => { fireEvent.keyDown(screen.getByRole("textbox"), { key: "Enter" }); });
  fireEvent.click(screen.getByRole("button", { name: "Second discussion" }));
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "First discussion" }));
  await act(async () => {});
  act(() => emit("chat_token", { chat_id: "chat", message_id: "answer", delta: "Retained streaming text" }));
  await act(() => vi.advanceTimersByTimeAsync(50));
  expect(screen.getByText("Retained streaming text")).toBeInTheDocument();
  await act(async () => { snapshot.resolve([{ ...answer("chat", ""), id: "answer" }]); });
  expect(screen.getByText("Retained streaming text")).toBeInTheDocument();
});

it("does not replace a just-created discussion with an older empty chat list", async () => {
  const initialList = deferred<{ id: string; title: string }[]>();
  const pending = deferred<ChatMessage>();
  api.listChats.mockReturnValueOnce(initialList.promise);
  api.createChat.mockResolvedValue({ id: "new", title: "New discussion" });
  api.sendChat.mockReturnValueOnce(pending.promise);
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "First question" } });
  await act(async () => { fireEvent.keyDown(screen.getByRole("textbox"), { key: "Enter" }); });
  expect(api.sendChat).toHaveBeenCalledExactlyOnceWith("new", "First question");
  await act(async () => { initialList.resolve([]); });
  expect(screen.getByRole("button", { name: "New discussion" })).toBeInTheDocument();
  expect(screen.getByText("First question")).toBeInTheDocument();
  expect(screen.getByRole("textbox")).toBeDisabled();
  await act(async () => { pending.resolve(answer("new", "New answer")); });
  expect(screen.getByText("New answer")).toBeInTheDocument();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  if (originalScroll) Object.defineProperty(Element.prototype, "scrollIntoView", originalScroll);
  else Reflect.deleteProperty(Element.prototype, "scrollIntoView");
});

it("leaves a composed or multiline message unsent until Enter is used to submit", async () => {
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  const composer = screen.getByRole("textbox");
  composer.focus();
  fireEvent.change(composer, { target: { value: "日本語の質問" } });
  await act(async () => { fireEvent.keyDown(composer, { key: "Enter", isComposing: true }); });
  expect(api.sendChat).not.toHaveBeenCalled();
  expect(composer).toHaveValue("日本語の質問");
  expect(composer).toHaveFocus();

  fireEvent.keyDown(composer, { key: "Enter", shiftKey: true });
  expect(api.sendChat).not.toHaveBeenCalled();
  fireEvent.change(composer, { target: { value: "日本語の質問\n補足の説明" } });
  await act(async () => { fireEvent.keyDown(composer, { key: "Enter" }); });
  expect(api.sendChat).toHaveBeenCalledExactlyOnceWith("chat", "日本語の質問\n補足の説明");
  expect(composer).toHaveValue("");
});

it("announces a failed message and preserves a named, focused composer for retry", async () => {
  let fail!: (reason: Error) => void;
  api.sendChat.mockReturnValueOnce(new Promise<ChatMessage>((_resolve, reject) => { fail = reject; }));
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  const composer = screen.getByRole("textbox");
  expect(composer).toHaveAccessibleName();
  composer.focus();
  fireEvent.change(composer, { target: { value: "Keep this question" } });
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(api.sendChat).toHaveBeenCalledExactlyOnceWith("chat", "Keep this question");
  expect(composer).toBeDisabled();
  fireEvent.submit(composer.closest("form")!);
  expect(api.sendChat).toHaveBeenCalledTimes(1);
  await act(async () => { fail(new Error("Provider is temporarily unavailable")); });
  expect(screen.getByRole("alert")).toHaveTextContent("Provider is temporarily unavailable");
  expect(composer).toHaveValue("Keep this question");
  expect(composer).toHaveFocus();
  expect(screen.getByRole("button", { name: "Send message" })).toBeEnabled();
  await act(async () => { fireEvent.keyDown(composer, { key: "Enter" }); });
  expect(api.sendChat).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(composer).toHaveValue("");
});

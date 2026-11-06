import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChatMessage, Events } from "@/lib/ipc";
import { ChatPanel } from "./ChatPanel";

const backend = vi.hoisted(() => ({ listChats: vi.fn(), chatMessages: vi.fn(), sendChat: vi.fn() }));
const listeners = vi.hoisted(() => new Map<string, Set<(payload: unknown) => void>>());
const markdown = vi.hoisted(() => vi.fn());
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: (name: string, handler: (payload: unknown) => void) => {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler); listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));
vi.mock("./Markdown", () => ({ Markdown: ({ text, onSeek }: { text: string; onSeek: (ms: number, label: string) => void }) => {
  markdown(text);
  return <button onClick={() => onSeek(65000, "1:05 @ Meeting")}>{text}</button>;
} }));

const message = (id: string, content: string): ChatMessage => ({ id, chat_id: "first", role: "assistant", content, citations: [], created_at: "2026-11-02T00:00:00Z" });
const originalScroll = Object.getOwnPropertyDescriptor(Element.prototype, "scrollIntoView");
function emit<K extends keyof Events>(name: K, payload: Events[K]) { listeners.get(name)?.forEach((handler) => handler(payload)); }
function token(delta: string, chat_id = "first") { emit("chat_token", { chat_id, message_id: "live", delta }); }

beforeEach(() => {
  vi.useFakeTimers(); vi.clearAllMocks(); listeners.clear();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  backend.listChats.mockResolvedValue([{ id: "first", title: "First chat" }, { id: "second", title: "Second chat" }]);
  backend.chatMessages.mockImplementation(async (id: string) => [message(id === "first" ? "old" : "other", id === "first" ? "Completed answer" : "Other chat")]);
});
afterEach(() => {
  cleanup(); vi.useRealTimers();
  if (originalScroll) Object.defineProperty(Element.prototype, "scrollIntoView", originalScroll);
  else Reflect.deleteProperty(Element.prototype, "scrollIntoView");
});

it("reuses completed Markdown while typing and batches token bursts in order", async () => {
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  markdown.mockClear();
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Draft" } });
  expect(markdown).not.toHaveBeenCalled();
  act(() => { for (const delta of ["Hello", ", ", "world", "!"]) token(delta); token("Unrelated", "second"); });
  await act(() => vi.advanceTimersByTimeAsync(49));
  expect(markdown).not.toHaveBeenCalled();
  await act(() => vi.advanceTimersByTimeAsync(1));
  expect(screen.getByText("Hello, world!")).toBeInTheDocument();
  expect(markdown.mock.calls).toEqual([["Hello, world!"]]);
  expect(screen.getByText("Completed answer")).toBeInTheDocument();
});

it("uses the authoritative final text and citations even before a pending token batch flushes", async () => {
  const onSeek = vi.fn();
  render(<ChatPanel scopeKind="all" onSeek={onSeek} />);
  await act(async () => {});
  const final = { ...message("live", "Final answer"), citations: [{ ms: 65000, meeting_id: "meeting-1", label: "1:05 @ Meeting" }] };
  act(() => { token("Final "); token("answer"); emit("chat_done", { chat_id: "first", message: final, error: null }); });
  expect(screen.getByText("Final answer")).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(screen.getAllByText("Final answer")).toHaveLength(1);
  fireEvent.click(screen.getByText("Final answer"));
  expect(onSeek).toHaveBeenCalledWith(65000, "meeting-1");
});

it("keeps fast-provider tokens and final text when send_chat returns its placeholder later", async () => {
  let finish!: (value: ChatMessage) => void;
  backend.sendChat.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
  render(<StrictMode><ChatPanel scopeKind="all" onSeek={vi.fn()} /></StrictMode>);
  await act(async () => {});
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Question" } });
  fireEvent.click(screen.getByRole("button", { name: "Send message" }));
  expect(screen.getByRole("progressbar", { name: "Waiting for the model" })).toBeInTheDocument();
  act(() => { token("First "); token("part"); });
  expect(screen.getByRole("progressbar", { name: "Receiving the reply" })).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(50));
  expect(screen.getByText("First part")).toBeInTheDocument();
  act(() => emit("chat_done", { chat_id: "first", message: message("live", "Complete answer"), error: null }));
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  await act(async () => { finish(message("live", "")); });
  expect(screen.getAllByText("Complete answer")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Stop generating" })).not.toBeInTheDocument();
});

it("cancels a queued batch when the selected chat changes", async () => {
  render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
  await act(async () => {});
  act(() => token("Old pending text"));
  fireEvent.click(screen.getByRole("button", { name: "Second chat" }));
  await act(async () => {});
  await act(() => vi.advanceTimersByTimeAsync(100));
  expect(screen.getByText("Other chat")).toBeInTheDocument();
  expect(screen.queryByText("Old pending text")).not.toBeInTheDocument();
});

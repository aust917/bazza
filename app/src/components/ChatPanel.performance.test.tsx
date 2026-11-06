import type { ComponentProps } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage, Events } from "@/lib/ipc";
import { ChatPanel } from "./ChatPanel";

const backend = vi.hoisted(() => ({ listChats: vi.fn(), chatMessages: vi.fn() }));
const listeners = vi.hoisted(() => new Map<string, Set<(payload: unknown) => void>>());
const markdownWork = vi.hoisted(() => vi.fn());
const originalScroll = Object.getOwnPropertyDescriptor(Element.prototype, "scrollIntoView");
vi.mock("@/lib/ipc", () => ({ cmd: backend, subscribe: (name: string, handler: (payload: unknown) => void) => {
  const handlers = listeners.get(name) ?? new Set();
  handlers.add(handler);
  listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));
// Keep the real parser/DOM, and observe how often messages reach that boundary.
vi.mock("./Markdown", async (importOriginal) => {
  const original = await importOriginal<typeof import("./Markdown")>();
  return { ...original, Markdown: (props: ComponentProps<typeof original.Markdown>) => {
    markdownWork(props.text);
    return <original.Markdown {...props} />;
  } };
});

function emit<K extends keyof Events>(name: K, payload: Events[K]) {
  listeners.get(name)?.forEach((handler) => handler(payload));
}
function token(delta: string, chat_id = "long-chat") {
  emit("chat_token", { chat_id, message_id: "live", delta });
}
function message(id: string, content: string): ChatMessage {
  return { id, chat_id: "long-chat", role: "assistant", content, citations: [], created_at: "2026-11-02T00:00:00Z" };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  listeners.clear();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  backend.listChats.mockResolvedValue([{ id: "long-chat", title: "Long chat" }]);
  backend.chatMessages.mockResolvedValue(Array.from({ length: 128 }, (_, index) => message(
    `history-${index}`,
    `## Completed answer ${index}\n\n${"- **Discussion** and a `code` example [1:05 @ Meeting]\n".repeat(12)}`,
  )));
});
afterEach(() => {
  cleanup(); vi.useRealTimers(); vi.restoreAllMocks();
  if (originalScroll) Object.defineProperty(Element.prototype, "scrollIntoView", originalScroll);
  else Reflect.deleteProperty(Element.prototype, "scrollIntoView");
});

describe("long chat streaming work budget", () => {
  it("renders one live answer per batch during sustained streaming and never revisits completed history", async () => {
    render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
    await act(async () => {});
    expect(screen.getByText("Completed answer 127")).toBeInTheDocument();
    expect(markdownWork).toHaveBeenCalledTimes(128);
    markdownWork.mockClear();
    vi.mocked(Element.prototype.scrollIntoView).mockClear();

    for (const draft of ["Q", "Question", "Question about this meeting"]) {
      fireEvent.change(screen.getByRole("textbox"), { target: { value: draft } });
    }
    expect(markdownWork).not.toHaveBeenCalled();

    let expected = "";
    // 1,200 provider events over 60 separate flushes exercises continued growth,
    // rather than letting React batch the entire synthetic stream into one act.
    for (let batch = 0; batch < 60; batch++) {
      act(() => {
        for (let delta = 0; delta < 20; delta++) {
          const text = `${batch}:${delta} `;
          expected += text;
          token(text);
        }
      });
      await act(() => vi.advanceTimersByTimeAsync(49));
      expect(markdownWork).toHaveBeenCalledTimes(batch);
      await act(() => vi.advanceTimersByTimeAsync(1));
      expect(markdownWork).toHaveBeenCalledTimes(batch + 1);
      expect(markdownWork).toHaveBeenLastCalledWith(expected);
    }
    expect(screen.getByText(expected.trim())).toBeInTheDocument();
    expect(screen.getByText("Completed answer 0")).toBeInTheDocument();
    expect(screen.getByText("Completed answer 127")).toBeInTheDocument();
    expect(markdownWork.mock.calls.every(([text]) => !text.includes("Completed answer"))).toBe(true);
    // Token deltas must not request a smooth scroll for every provider event.
    expect(Element.prototype.scrollIntoView).toHaveBeenCalledTimes(1);
    expect(backend.chatMessages).toHaveBeenCalledTimes(1);

    const final = message("live", `## Final answer\n\n${expected}`);
    act(() => emit("chat_done", { chat_id: "long-chat", message: final, error: null }));
    expect(screen.getByText("Final answer")).toBeInTheDocument();
    expect(markdownWork).toHaveBeenCalledTimes(61);
    markdownWork.mockClear();
    act(() => { for (let index = 0; index < 1000; index++) token("late delta"); });
    await act(() => vi.advanceTimersByTimeAsync(1000));
    expect(markdownWork).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does no parser work for another window's stream and releases pending work on unmount", async () => {
    const { unmount } = render(<ChatPanel scopeKind="all" onSeek={vi.fn()} />);
    await act(async () => {});
    markdownWork.mockClear();
    act(() => { for (let index = 0; index < 1000; index++) token("unrelated", "other-chat"); });
    await act(() => vi.advanceTimersByTimeAsync(500));
    expect(markdownWork).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);

    act(() => token("queued text"));
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
    expect([...listeners.values()].every((handlers) => handlers.size === 0)).toBe(true);
    await act(() => vi.advanceTimersByTimeAsync(500));
    expect(markdownWork).not.toHaveBeenCalled();
  });
});

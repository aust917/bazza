import { useTranslation } from "react-i18next";
import { ArrowUp, Plus, Square, Trash2 } from "lucide-react";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { displayError } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import { cmd, subscribe, type Chat, type ChatMessage, type Segment } from "@/lib/ipc";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { Markdown } from "./Markdown";
import { SubscriptionChat } from "./SubscriptionChat";
import { Button } from "./ui";

interface ChatPanelProps {
  scopeKind: "meeting" | "all";
  scopeId?: string | null;
  /** Called for a citation click; `meetingId` is null when unresolved. */
  onSeek?: (ms: number, meetingId: string | null) => void;
  placeholder?: string;
  className?: string;
  transcript?: { segments: Segment[]; useClean: boolean };
}

const Message = memo(function Message({ message: m, onSeek }: { message: ChatMessage; onSeek: ChatPanelProps["onSeek"] }) {
  return <div className={cn("flex", m.role === "user" ? "justify-end" : "justify-start")}>
    <div className={cn("max-w-[85%] rounded-xl px-3.5 py-2", m.role === "user" ? "bg-ink text-canvas" : "border border-line bg-canvas shadow-card")}>
      {m.role === "user" ? <p className="text-[14px] leading-6">{m.content}</p>
        : <Markdown text={m.content} onSeek={onSeek ? (ms, label) => onSeek(ms, resolveMeeting(m, label)) : undefined} />}
    </div>
  </div>;
});

function upsertMessage(messages: ChatMessage[], message: ChatMessage): ChatMessage[] {
  return messages.some((m) => m.id === message.id)
    ? messages.map((m) => m.id === message.id ? message : m)
    : [...messages, message];
}

/** A different scope owns a fresh session, including outstanding async requests. */
export function ChatPanel(props: ChatPanelProps) {
  return <ChatSession key={JSON.stringify([props.scopeKind, props.scopeId ?? null])} {...props} />;
}

type RunningChat = { pending: boolean; messageId: string | null; startedAt?: number; receiving?: boolean };

/** Chat over one meeting or the whole library. Streams the assistant reply. */
function ChatSession({ scopeKind, scopeId = null, onSeek, placeholder, className, transcript }: ChatPanelProps) {
  const { t: tr } = useTranslation("meeting");
  const [chats, setChats] = useState<Chat[]>([]);
  const [chatId, setChatId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [runs, setRuns] = useState<Record<string, RunningChat>>({});
  const [error, setError] = useState<unknown>(null);
  const [subscriptionOpen, setSubscriptionOpen] = useState(false);
  const bottom = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const activeRuns = useRef(new Map<string, RunningChat>());
  const drafts = useRef(new Map<string, string>());
  const errors = useRef(new Map<string, unknown>());
  const selectedChat = useRef<string | null>(null);
  const selectionRevision = useRef(0);
  const chatListRequest = useRef(0);
  const emptyChats = useRef(new Set<string>());
  const mounted = useRef(false);
  const restoreComposerFocus = useRef<string | null>(null);
  const completed = useRef(new Set<string>());
  const viewKey = chatId ?? "";
  const run = runs[viewKey];
  const streaming = run?.messageId ?? null;
  const sending = run?.pending ?? false;

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; chatListRequest.current++; };
  }, []);

  const selectChat = useCallback((id: string | null) => {
    if (selectedChat.current === id) return;
    selectedChat.current = id;
    selectionRevision.current++;
    chatListRequest.current++;
    restoreComposerFocus.current = null;
    setChatId(id);
    setMessages([]);
    setDraft(drafts.current.get(id ?? "") ?? "");
    setError(errors.current.get(id ?? "") ?? null);
  }, []);
  const updateRun = (id: string, run?: RunningChat) => {
    if (run) activeRuns.current.set(id, run);
    else activeRuns.current.delete(id);
    setRuns((previous) => {
      const next = { ...previous };
      if (run) next[id] = run;
      else delete next[id];
      return next;
    });
  };
  const updateDraft = (id: string | null, text: string) => {
    drafts.current.set(id ?? "", text);
    if (selectedChat.current === id) setDraft(text);
  };
  const updateError = (id: string | null, failure: unknown) => {
    errors.current.set(id ?? "", failure);
    if (selectedChat.current === id) setError(failure);
  };

  const loadChats = async () => {
    const request = ++chatListRequest.current;
    try {
      const list = await cmd.listChats(scopeKind, scopeId);
      if (!mounted.current || request !== chatListRequest.current) return;
      setChats(list);
      const selected = selectedChat.current;
      if (!selected || !list.some((c) => c.id === selected)) selectChat(list[0]?.id ?? null);
    } catch (failure) {
      if (mounted.current && request === chatListRequest.current) updateError(selectedChat.current, failure);
    }
  };

  useEffect(() => {
    void loadChats();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scopeKind, scopeId]);

  useEffect(() => {
    if (!chatId || emptyChats.current.delete(chatId)) return;
    let active = true;
    void cmd.chatMessages(chatId).then((rows) => {
      if (!active) return;
      // Tokens or a send may have updated this view while the snapshot was read.
      // Keep those message objects so an older placeholder cannot erase live text.
      setMessages((current) => {
        const newer = new Map(current.map((message) => [message.id, message]));
        const next = rows.map((message) => {
          const latest = newer.get(message.id) ?? message;
          newer.delete(message.id);
          return latest;
        });
        return [...next, ...newer.values()];
      });
    }, (failure) => { if (active) updateError(chatId, failure); });
    return () => { active = false; };
  }, [chatId]);

  useEffect(() => {
    const deltas = new Map<string, string>();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const flush = () => {
      timer = undefined;
      if (deltas.size === 0) return;
      const batch = new Map(deltas);
      deltas.clear();
      const createdAt = new Date().toISOString();
      setMessages((messages) => {
        const remaining = new Map(batch);
        const next = messages.map((m) => {
          const delta = remaining.get(m.id);
          remaining.delete(m.id);
          return delta === undefined ? m : { ...m, content: m.content + delta };
        });
        // A fast provider may stream before send_chat returns the assistant row.
        for (const [id, content] of remaining) next.push({ id, chat_id: chatId!, role: "assistant", content, citations: [], created_at: createdAt });
        return next;
      });
    };
    const a = subscribe("chat_token", (p) => {
      if (completed.current.has(p.message_id)) return;
      const run = activeRuns.current.get(p.chat_id);
      if (p.delta && run?.startedAt !== undefined && !run.receiving && (!run.messageId || run.messageId === p.message_id)) {
        updateRun(p.chat_id, { ...run, receiving: true });
      }
      if (p.chat_id !== chatId) return;
      deltas.set(p.message_id, (deltas.get(p.message_id) ?? "") + p.delta);
      if (timer === undefined) timer = setTimeout(flush, 50);
    });
    const b = subscribe("chat_done", (p) => {
      // Track completion for our off-screen runs too, without changing the view.
      if (p.chat_id !== selectedChat.current && !activeRuns.current.has(p.chat_id)) return;
      completed.current.add(p.message.id);
      deltas.delete(p.message.id);
      if (deltas.size === 0) { clearTimeout(timer); timer = undefined; }
      const run = activeRuns.current.get(p.chat_id);
      updateRun(p.chat_id, run?.pending ? { pending: true, messageId: null } : undefined);
      updateError(p.chat_id, p.error);
      // The authoritative final message also includes any unflushed deltas.
      if (p.chat_id === selectedChat.current) setMessages((ms) => upsertMessage(ms, p.message));
      void loadChats();
    });
    return () => {
      clearTimeout(timer);
      a();
      b();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatId]);

  useEffect(() => {
    bottom.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length, streaming]);
  useEffect(() => {
    if (!sending && restoreComposerFocus.current === viewKey) {
      composer.current?.focus();
      restoreComposerFocus.current = null;
    }
  }, [sending, viewKey]);

  const send = async () => {
    const text = draft.trim();
    if (!text || activeRuns.current.has(viewKey)) return;
    const selection = selectionRevision.current;
    let id = chatId;
    let requestKey = viewKey;
    const pendingRun: RunningChat = { pending: true, messageId: null, startedAt: performance.now() };
    restoreComposerFocus.current = requestKey;
    updateRun(requestKey, pendingRun);
    updateError(id, null);
    let temporaryId: string | null = null;
    try {
      if (!id) {
        const c = await cmd.createChat(scopeKind, scopeId, undefined);
        if (!mounted.current) return;
        id = c.id;
        updateRun(requestKey);
        requestKey = id;
        updateRun(id, pendingRun);
        emptyChats.current.add(id);
        setChats((cs) => [c, ...cs]);
        if (selection === selectionRevision.current) {
          selectChat(id);
          restoreComposerFocus.current = id;
        }
      }
      updateDraft(id, "");
      const userMsg: ChatMessage = { id: `tmp-${Date.now()}`, chat_id: id, role: "user", content: text, citations: [], created_at: new Date().toISOString() };
      temporaryId = userMsg.id;
      if (selectedChat.current === id) setMessages((ms) => [...ms, userMsg]);
      const assistant = await cmd.sendChat(id, text);
      if (!mounted.current) return;
      if (selectedChat.current === id) setMessages((ms) => ms.some((m) => m.id === assistant.id) ? ms : [...ms, assistant]);
      updateRun(id, completed.current.has(assistant.id) ? undefined : {
        ...pendingRun, pending: false, messageId: assistant.id,
        receiving: activeRuns.current.get(id)?.receiving || assistant.content !== "",
      });
    } catch (e) {
      if (!mounted.current) return;
      updateRun(requestKey);
      updateError(id, e);
      if (selectedChat.current === id && temporaryId !== null) setMessages((ms) => ms.filter((m) => m.id !== temporaryId));
      updateDraft(id, text);
    } finally {
      const run = activeRuns.current.get(requestKey);
      if (mounted.current && run?.pending) updateRun(requestKey, run.messageId ? { ...run, pending: false } : undefined);
    }
  };

  const newChat = async () => {
    const selection = selectionRevision.current;
    try {
      const c = await cmd.createChat(scopeKind, scopeId, undefined);
      if (!mounted.current) return;
      emptyChats.current.add(c.id);
      setChats((cs) => [c, ...cs]);
      if (selection === selectionRevision.current) selectChat(c.id);
    } catch (failure) {
      if (mounted.current && selection === selectionRevision.current) updateError(selectedChat.current, failure);
    }
  };

  const removeChat = async (id: string) => {
    try {
      await cmd.deleteChat(id);
      if (!mounted.current) return;
      if (selectedChat.current === id) selectChat(null);
      await loadChats();
    } catch (failure) {
      if (mounted.current) updateError(id, failure);
    }
  };

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      <details className="mb-2" onToggle={(event) => setSubscriptionOpen(event.currentTarget.open)}>
        <summary className="cursor-pointer text-[13px] text-ink-3">{tr("subscription.chatTitle")}</summary>
        {subscriptionOpen && <SubscriptionChat chatKey={scopeId ? `meeting/${scopeId}` : "library"} transcript={transcript} />}
      </details>
      {chats.length > 0 && (
        <div className="mb-2 flex items-center gap-1 overflow-x-auto pb-1 text-[12px]">
          {chats.map((c) => (
            <span key={c.id} className={cn("group inline-flex shrink-0 items-center gap-1 rounded-full border px-2.5 py-0.5", c.id === chatId ? "border-ink bg-canvas" : "border-line text-ink-3 hover:text-ink")}>
              <button onClick={() => selectChat(c.id)} className="max-w-[160px] truncate">{c.title}</button>
              <button onClick={() => void removeChat(c.id)} className="text-ink-3 opacity-0 group-hover:opacity-100 hover:text-ember" title={tr("chat.delete")}>
                <Trash2 size={11} />
              </button>
            </span>
          ))}
          <button onClick={() => void newChat()} className="inline-flex shrink-0 items-center gap-1 rounded-full border border-dashed border-line-2 px-2.5 py-0.5 text-ink-3 hover:text-ink" title={tr("chat.new")}>
            <Plus size={12} /> {tr("chat.newShort")}
          </button>
        </div>
      )}

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto pr-1">
        {messages.length === 0 && (
          <div className="rise my-8 text-center text-[13px] text-ink-3">
            {placeholder ?? tr("chat.empty")}
          </div>
        )}
        {messages.map((m) => m.id === streaming && m.content === "" ? null : <Message key={m.id} message={m} onSeek={onSeek} />)}
        {error != null && <div role="alert" className="text-[12px] text-ember">{displayError(error)}</div>}
        <div ref={bottom} />
      </div>

      {run?.startedAt !== undefined && <OperationProgress
        className="mt-2 px-1"
        label={tr(run.receiving ? "chat.receivingReply" : "chat.waitingForProvider")}
        startedAt={run.startedAt}
        announceStage
      />}
      <form
        className="mt-3 flex items-end gap-2 rounded-xl border border-line-2 bg-canvas p-1.5 pl-3 shadow-card focus-within:border-ink"
        aria-busy={sending || undefined}
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <textarea
          ref={composer}
          aria-label={tr("chat.placeholder")}
          disabled={sending}
          value={draft}
          onChange={(e) => updateDraft(chatId, e.target.value)}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            }
          }}
          rows={1}
          placeholder={tr("chat.placeholder")}
          className="max-h-32 min-h-[28px] flex-1 resize-none bg-transparent py-1 text-[14px] outline-none placeholder:text-ink-3"
        />
        {streaming ? (
          <Button variant="soft" size="icon" type="button" onClick={() => chatId && void cmd.cancelChat(chatId)} title={tr("actions.stop")} aria-label={tr("chat.stop")}>
            <Square size={14} fill="currentColor" />
          </Button>
        ) : (
          <Button variant="primary" size="icon" type="submit" disabled={sending || !draft.trim()} title={tr("actions.send")} aria-label={tr("chat.send")}>
            <ArrowUp size={16} />
          </Button>
        )}
      </form>
    </div>
  );
}

function resolveMeeting(m: ChatMessage, label: string): string | null {
  const cites = Array.isArray(m.citations) ? (m.citations as { ms: number; meeting_id: string | null; label: string }[]) : [];
  return cites.find((c) => c.label === label)?.meeting_id ?? null;
}

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Caption, LiveSnapshot } from "@/lib/ipc";
import { LiveTranscript, useLiveFeed } from "./LiveView";

const listeners = vi.hoisted(() => new Map<string, (payload: unknown) => void>());
const liveSnapshot = vi.hoisted(() => vi.fn());
vi.mock("@/lib/ipc", () => {
  const listen = (name: string, callback: (payload: unknown) => void) => {
    listeners.set(name, callback);
    return () => { listeners.delete(name); };
  };
  return { cmd: { liveSnapshot }, on: async (...args: Parameters<typeof listen>) => listen(...args), subscribe: listen };
});

const emptySnapshot: LiveSnapshot = { meeting_id: "m1", finals: [], partial: null, notes: "", notes_updated_ms: null };
const alice = { meeting_id: "m1", speaker_id: "a", speaker_name: "Alice" };
const bob = { meeting_id: "m1", speaker_id: "b", speaker_name: "Bob" };

function transcript() {
  function Fixture() {
    const live = useLiveFeed("m1");
    return <LiveTranscript finals={live?.finals ?? []} partials={live?.partials ?? []} />;
  }
  return render(<Fixture />);
}

function emit(caption: Caption) {
  act(() => listeners.get("caption")?.(caption));
}

beforeEach(() => {
  listeners.clear();
  liveSnapshot.mockReset().mockResolvedValue(emptySnapshot);
  HTMLElement.prototype.scrollIntoView ??= vi.fn();
});
afterEach(cleanup);

describe("meeting live transcript", () => {
  it("keeps simultaneous partials and updates or clears only the corresponding speaker", async () => {
    await act(async () => { transcript(); });
    emit({ ...alice, start_ms: 10, end_ms: 30, text: "First draft", is_final: false });
    emit({ ...bob, start_ms: 20, end_ms: 40, text: "Second draft", is_final: false });
    const aliceRow = screen.getByText("Alice").closest("li");
    expect(aliceRow).toHaveTextContent("First draft");
    expect(screen.getByText("Bob").closest("li")).toHaveTextContent("Second draft");

    emit({ ...alice, start_ms: 10, end_ms: 45, text: "First revised", is_final: false });
    expect(screen.getByText("First revised").closest("li")).toBe(aliceRow);
    expect(screen.queryByText("First draft")).not.toBeInTheDocument();
    expect(screen.getByText("Second draft")).toBeVisible();
    emit({ ...alice, start_ms: 10, end_ms: 50, text: "First final", is_final: true });
    expect(screen.getByText("First final").closest("li")).not.toHaveAttribute("aria-live");
    expect(screen.getByText("Second draft").closest("li")).toHaveAttribute("aria-live", "polite");
    emit({ ...bob, start_ms: 20, end_ms: 40, text: "", is_final: true });
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
  });

  it("merges events during snapshot loading and scopes named captions to the meeting", async () => {
    let resolve!: (value: LiveSnapshot) => void;
    liveSnapshot.mockReturnValueOnce(new Promise((done) => { resolve = done; }));
    await act(async () => { transcript(); });
    const final = { ...alice, start_ms: 10, end_ms: 50, text: "Already confirmed", is_final: true };
    emit({ ...alice, start_ms: 10, end_ms: 40, text: "Stale draft", is_final: false });
    emit(final);
    emit({ ...bob, start_ms: 20, end_ms: 65, text: "Live revision", is_final: false });
    emit({ ...bob, meeting_id: "other", start_ms: 20, end_ms: 75, text: "Other meeting", is_final: false });
    await act(async () => resolve({
      ...emptySnapshot,
      finals: [final],
      partials: [
        { ...alice, start_ms: 55, end_ms: 62, text: "Next turn from snapshot", is_final: false },
        { ...bob, start_ms: 20, end_ms: 60, text: "Older snapshot draft", is_final: false },
      ],
    }));
    expect(screen.getAllByText("Already confirmed")).toHaveLength(1);
    expect(screen.getByText("Next turn from snapshot")).toBeVisible();
    expect(screen.getByText("Live revision")).toBeVisible();
    expect(screen.queryByText("Older snapshot draft")).not.toBeInTheDocument();
    expect(screen.queryByText("Other meeting")).not.toBeInTheDocument();
  });

  it("labels recording audio independently from a participant speaking at the same time", async () => {
    await act(async () => { transcript(); });
    const audio = { meeting_id: "m1", speaker_id: "local:mix", start_ms: 10, end_ms: 30 };
    emit({ ...alice, start_ms: 10, end_ms: 30, text: "Voice chat", is_final: false });
    emit({ ...audio, text: "Screen share", is_final: false });
    expect(screen.getByText("Recording audio").closest("li")).toHaveTextContent("Screen share");
    expect(screen.getByText("Alice").closest("li")).toHaveTextContent("Voice chat");

    emit({ ...audio, end_ms: 40, text: "Final share caption", is_final: true });
    expect(screen.queryByText("Screen share")).not.toBeInTheDocument();
    expect(screen.getByText("Recording audio").closest("li")).toHaveTextContent("Final share caption");
    expect(screen.getByText("Voice chat")).toBeVisible();
  });

  it("loads and replaces one unnamed partial on the standard wire shape", async () => {
    const partial = { start_ms: 10, end_ms: 30, text: "Plain draft", is_final: false };
    liveSnapshot.mockResolvedValueOnce({ ...emptySnapshot, partial });
    await act(async () => { transcript(); });
    expect(screen.getByText("Plain draft")).toBeVisible();
    emit({ ...partial, end_ms: 40, text: "Plain revised" });
    expect(screen.queryByText("Plain draft")).not.toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    emit({ ...partial, end_ms: 50, text: "Plain final", is_final: true });
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByText("Plain final").closest("li")).not.toHaveAttribute("aria-live");
    expect(screen.queryByText("Plain revised")).not.toBeInTheDocument();
  });
});

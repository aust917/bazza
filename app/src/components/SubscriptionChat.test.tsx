import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { cmd, type Segment, type SubscriptionJob } from "@/lib/ipc";
import { SubscriptionChat } from "./SubscriptionChat";

vi.mock("@/lib/ipc", () => ({
  cmd: { subscriptionAgents: vi.fn(), getSubscriptionJob: vi.fn(), startSubscription: vi.fn(), cancelSubscription: vi.fn() },
}));

const chatKey = "meeting/test-meeting";
const request: SubscriptionJob = { request_id: "first", agent: "codex", status: "running", reply: null, error: null };
const answer: SubscriptionJob = {
  ...request, status: "succeeded", reply: { markdown: "Synthetic answer", model: "Codex CLI" },
};

function segment(id: number, start_ms: number, text: string, clean_text: string | null): Segment {
  return { id, meeting_id: "test-meeting", start_ms, end_ms: start_ms + 750, text, clean_text, speaker: "Ada" };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(cmd.getSubscriptionJob).mockResolvedValue(null);
  vi.mocked(cmd.subscriptionAgents).mockResolvedValue([
    { agent: "codex", status: "ready" },
    { agent: "claude", status: "ready" },
  ]);
});

async function draft(agent = "codex", questionOnly = false) {
  await screen.findByRole("option", { name: "Codex · signed in" });
  fireEvent.change(screen.getByLabelText("Question"), { target: { value: "What was decided?" } });
  const send = screen.getByRole("button", { name: "Send to provider" });
  expect(send).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Subscription provider"), { target: { value: agent } });
  if (questionOnly) {
    expect(send).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: "Send only my question" }));
  }
  await waitFor(() => expect(send).toBeEnabled());
  return send;
}

it("previews only the selected meeting lines and requires explicit send to the disclosed provider", async () => {
  vi.mocked(cmd.startSubscription).mockImplementation(async () => {
    vi.mocked(cmd.getSubscriptionJob).mockResolvedValue(answer);
    return answer;
  });
  render(<SubscriptionChat chatKey={chatKey} transcript={{ segments: [
    segment(1, 500, "Before", null),
    segment(2, 1_000, "Raw text", "Reviewed text"),
    segment(3, 2_000, "Outside range", null),
  ], useClean: true }} />);
  const context = screen.getByLabelText("Context to share (optional)");
  expect(context).toHaveValue("");
  fireEvent.change(screen.getByLabelText("From"), { target: { value: "0:01" } });
  fireEvent.change(screen.getByLabelText("To"), { target: { value: "0:02" } });
  fireEvent.click(screen.getByRole("button", { name: "Preview in context field" }));
  expect(context).toHaveValue("[0:01] Ada: Reviewed text");

  const send = await draft("claude");
  expect(screen.getByText(/Anthropic through Claude Code/)).toBeVisible();
  expect(cmd.startSubscription).not.toHaveBeenCalled();
  fireEvent.click(send);
  expect(await screen.findByText("Synthetic answer")).toBeVisible();
  expect(cmd.startSubscription).toHaveBeenCalledExactlyOnceWith(chatKey, "claude", "What was decided?", "[0:01] Ada: Reviewed text");
});

it("rejects simultaneous submits and restores the running request and its completed reply after reopening", async () => {
  let finish!: (job: SubscriptionJob) => void;
  vi.mocked(cmd.startSubscription).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
  const first = render(<SubscriptionChat chatKey={chatKey} />);
  const send = await draft("codex", true);
  const form = send.closest("form")!;
  act(() => {
    fireEvent.submit(form);
    fireEvent.submit(form);
  });
  expect(cmd.startSubscription).toHaveBeenCalledExactlyOnceWith(chatKey, "codex", "What was decided?", null);
  first.unmount();

  vi.mocked(cmd.getSubscriptionJob).mockResolvedValue(request);
  const second = render(<SubscriptionChat chatKey={chatKey} />);
  expect(await screen.findByText(/Waiting for a reply from OpenAI through Codex/)).toBeVisible();
  expect(screen.getByRole("progressbar", { name: /Waiting for a reply/ })).not.toHaveAttribute("aria-valuenow");
  expect(screen.getByRole("button", { name: "Send to provider" })).toBeDisabled();
  await act(async () => { finish(request); });
  second.unmount();

  vi.mocked(cmd.getSubscriptionJob).mockResolvedValue(answer);
  render(<SubscriptionChat chatKey={chatKey} />);
  expect(await screen.findByText("Synthetic answer")).toBeVisible();
  expect(screen.getByRole("progressbar", { name: "Reply ready" })).toHaveAttribute("aria-valuenow", "100");
  expect(cmd.startSubscription).toHaveBeenCalledOnce();
});

it("cancels the recovered request ID and waits for confirmed cancellation", async () => {
  vi.mocked(cmd.getSubscriptionJob).mockResolvedValue(request);
  vi.mocked(cmd.cancelSubscription).mockImplementation(async () => {
    vi.mocked(cmd.getSubscriptionJob).mockResolvedValue({ ...request, status: "cancelled" });
    return { ...request, status: "cancelling" };
  });
  render(<SubscriptionChat chatKey={chatKey} />);
  fireEvent.click(await screen.findByRole("button", { name: "Cancel request" }));
  expect(cmd.cancelSubscription).toHaveBeenCalledExactlyOnceWith(chatKey, "first");
  expect(await screen.findByText("Cancelling the request…")).toBeVisible();
  expect(screen.getByRole("button", { name: "Send to provider" })).toBeDisabled();
  expect(await screen.findByText("Request cancelled.")).toBeVisible();
  expect(screen.queryByText("Synthetic answer")).not.toBeInTheDocument();
});

it("reports provider failure without making another request or losing the draft", async () => {
  vi.mocked(cmd.startSubscription).mockImplementation(async () => {
    const failure = { ...request, status: "failed", error: { code: "native.subscriptionRequestFailed" } } as const;
    vi.mocked(cmd.getSubscriptionJob).mockResolvedValue(failure);
    return failure;
  });
  render(<SubscriptionChat chatKey={chatKey} />);
  fireEvent.click(await draft("codex", true));
  expect(await screen.findByRole("alert")).toHaveTextContent("could not answer");
  expect(screen.getByLabelText("Question")).toHaveValue("What was decided?");
  expect(cmd.startSubscription).toHaveBeenCalledOnce();
});

it("clears drafts and ignores a late status read when the chat changes meetings", async () => {
  const secondKey = "meeting/another-meeting";
  let finishOldRead!: (job: SubscriptionJob) => void;
  vi.mocked(cmd.getSubscriptionJob).mockImplementation((key) => key === chatKey
    ? new Promise((resolve) => { finishOldRead = resolve; })
    : Promise.resolve(null));
  const { rerender } = render(<SubscriptionChat chatKey={chatKey} />);
  await screen.findByRole("option", { name: "Codex · signed in" });
  fireEvent.change(screen.getByLabelText("Question"), { target: { value: "Private first draft" } });
  fireEvent.change(screen.getByLabelText("Context to share (optional)"), { target: { value: "First meeting only" } });
  fireEvent.change(screen.getByLabelText("Subscription provider"), { target: { value: "codex" } });
  rerender(<SubscriptionChat chatKey={secondKey} />);
  await waitFor(() => expect(cmd.getSubscriptionJob).toHaveBeenCalledWith(secondKey));
  await act(async () => { finishOldRead(answer); });
  expect(screen.getByLabelText("Question")).toHaveValue("");
  expect(screen.getByLabelText("Context to share (optional)")).toHaveValue("");
  expect(screen.getByLabelText("Subscription provider")).toHaveValue("");
  expect(screen.queryByText("Synthetic answer")).not.toBeInTheDocument();
  expect(cmd.startSubscription).not.toHaveBeenCalled();
});

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { SummaryControls } from "./SummaryControls";
import { cmd } from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({ cmd: { subscriptionAgents: vi.fn(), summaryJob: vi.fn(), generateSummary: vi.fn(), cancelSummary: vi.fn() } }));
const props = { meetingId: "m1", templates: [], templateId: "general", onTemplateChange: vi.fn(), disabled: false, writing: false, hasSummary: true, onStarted: vi.fn(), onFinished: vi.fn(), onError: vi.fn() };
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(cmd.subscriptionAgents).mockResolvedValue([{ agent: "codex", status: "ready" }, { agent: "claude", status: "sign_in_required" }]);
  vi.mocked(cmd.generateSummary).mockResolvedValue();
  vi.mocked(cmd.cancelSummary).mockResolvedValue();
  vi.mocked(cmd.summaryJob).mockResolvedValue({ running: false, agent: null });
});

it("restores a running job after reopening and allows cancelling it", async () => {
  vi.mocked(cmd.summaryJob).mockResolvedValue({ running: true, agent: "codex" });
  render(<SummaryControls {...props} />);
  fireEvent.click(await screen.findByRole("button", { name: "Cancel summary" }));
  expect(cmd.cancelSummary).toHaveBeenCalledWith("m1");
  expect(screen.getByRole("button", { name: "Regenerate" })).toBeDisabled();
  expect(screen.getByLabelText("Summary provider")).toHaveValue("");
});

it("requires an explicit generate action and discloses the selected recipient", async () => {
  render(<SummaryControls {...props} />);
  await waitFor(() => expect(screen.getByRole("option", { name: /Codex/ })).not.toBeDisabled());
  fireEvent.change(screen.getByLabelText("Summary provider"), { target: { value: "codex" } });
  expect(screen.getByText(/OpenAI through Codex/)).toBeInTheDocument();
  expect(screen.getByText(/participants and highlight timestamps/)).toBeInTheDocument();
  expect(screen.getByRole("option", { name: "Claude Code · sign in to the CLI" })).toBeDisabled();
  expect(cmd.generateSummary).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Regenerate" }));
  await waitFor(() => expect(cmd.generateSummary).toHaveBeenCalledWith("m1", "general", "codex"));
});

it("keeps the configured model available when the CLI check fails", async () => {
  vi.mocked(cmd.subscriptionAgents).mockRejectedValue(new Error("check failed"));
  render(<SummaryControls {...props} />);
  expect(await screen.findByRole("option", { name: "Codex · could not check sign-in" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Regenerate" }));
  await waitFor(() => expect(cmd.generateSummary).toHaveBeenCalledWith("m1", "general", undefined));
});

it("reports launch failure and offers cancellation while the agent is writing", async () => {
  vi.mocked(cmd.generateSummary).mockRejectedValue(new Error("No subscription login"));
  const { rerender } = render(<SummaryControls {...props} />);
  await waitFor(() => expect(screen.getByRole("option", { name: /Codex/ })).not.toBeDisabled());
  fireEvent.change(screen.getByLabelText("Summary provider"), { target: { value: "codex" } });
  fireEvent.click(screen.getByRole("button", { name: "Regenerate" }));
  await waitFor(() => expect(props.onError).toHaveBeenCalledWith("Error: No subscription login"));
  rerender(<SummaryControls {...props} writing />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel summary" }));
  expect(cmd.cancelSummary).toHaveBeenCalledWith("m1");
});

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { SummaryControls } from "./SummaryControls";
import { cmd } from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({ cmd: { summaryAgents: vi.fn(), summaryJob: vi.fn(), generateSummary: vi.fn(), cancelSummary: vi.fn() } }));
const props = { meetingId: "m1", templates: [], templateId: "general", onTemplateChange: vi.fn(), disabled: false, writing: false, hasSummary: true, onStarted: vi.fn(), onFinished: vi.fn(), onError: vi.fn() };
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(cmd.summaryAgents).mockResolvedValue([{ agent: "codex", installed: true }, { agent: "claude", installed: false }]);
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
});

it("requires an explicit generate action and discloses the selected recipient", async () => {
  render(<SummaryControls {...props} />);
  await waitFor(() => expect(screen.getByRole("option", { name: /Codex/ })).not.toBeDisabled());
  fireEvent.change(screen.getByLabelText("Summary provider"), { target: { value: "codex" } });
  expect(screen.getByText(/OpenAI through Codex/)).toBeInTheDocument();
  expect(cmd.generateSummary).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Regenerate" }));
  await waitFor(() => expect(cmd.generateSummary).toHaveBeenCalledWith("m1", "general", "codex"));
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

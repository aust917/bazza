import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { VoicesPage } from "./Voices";

const api = vi.hoisted(() => ({
  listVoiceProfiles: vi.fn(), listMeetings: vi.fn(), listVoiceSamples: vi.fn(),
  createVoiceProfile: vi.fn(), renameVoiceProfile: vi.fn(), addVoiceSample: vi.fn(),
  deleteVoiceSample: vi.fn(), deleteKnownSpeaker: vi.fn(), recomputeVoiceProfile: vi.fn(), voiceSampleAudio: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ cmd: api }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const profile = { id: "p1", name: "Ann", model_id: "current", sample_count: 0, has_voiceprint: false };

async function chooseProfile() {
  render(<MemoryRouter><VoicesPage /></MemoryRouter>);
  fireEvent.click(await screen.findByRole("button", { name: /Ann/ }));
  await screen.findByLabelText("Profile name");
}

describe("VoicesPage", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    api.listVoiceProfiles.mockResolvedValue([profile]);
    api.listMeetings.mockResolvedValue([{ id: "m1", title: "Control recording", status: "ready", duration_ms: 20_000 }]);
    api.listVoiceSamples.mockResolvedValue([]);
    api.addVoiceSample.mockResolvedValue({});
  });

  it("rejects an invalid speech interval before invoking native extraction", async () => {
    await chooseProfile();
    fireEvent.change(screen.getByLabelText("Sample end seconds"), { target: { value: "0.5" } });
    fireEvent.submit(screen.getByRole("button", { name: "Add sample" }).closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("Choose 1–120 seconds");
    expect(api.addVoiceSample).not.toHaveBeenCalled();
  });

  it("passes the selected recording interval in milliseconds and refreshes saved samples", async () => {
    await chooseProfile();
    fireEvent.change(screen.getByLabelText("Sample source"), { target: { value: "meeting" } });
    fireEvent.change(screen.getByLabelText("Sample recording"), { target: { value: "m1" } });
    fireEvent.change(screen.getByLabelText("Sample start seconds"), { target: { value: "0.029" } });
    fireEvent.change(screen.getByLabelText("Sample end seconds"), { target: { value: "2.029" } });
    fireEvent.submit(screen.getByRole("button", { name: "Add sample" }).closest("form")!);
    await waitFor(() => expect(api.addVoiceSample).toHaveBeenCalledWith("p1", { kind: "meeting", meeting_id: "m1", start_ms: 29, end_ms: 2029 }));
    await waitFor(() => expect(api.listVoiceSamples).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows a model or extraction failure without creating a sample in the UI", async () => {
    api.addVoiceSample.mockRejectedValue(new Error("download speaker recognition models first"));
    await chooseProfile();
    fireEvent.change(screen.getByLabelText("Sample source"), { target: { value: "meeting" } });
    fireEvent.change(screen.getByLabelText("Sample recording"), { target: { value: "m1" } });
    fireEvent.submit(screen.getByRole("button", { name: "Add sample" }).closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("download speaker recognition models first");
    expect(screen.getByText("Add a speech sample to remember this voice.")).toBeInTheDocument();
    expect(api.listVoiceSamples).toHaveBeenCalledTimes(1);
  });
});

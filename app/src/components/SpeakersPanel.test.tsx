import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import type { MeetingSpeaker, SpeakerStat } from "@/lib/ipc";
import { SpeakersPanel } from "./SpeakersPanel";

const stats: SpeakerStat[] = [
  { label: "Sarah", segments: 12, ms: 240_000, first_ms: 0 },
  { label: "Speaker 2", segments: 5, ms: 60_000, first_ms: 30_000 },
];

const speakers: MeetingSpeaker[] = [
  { meeting_id: "m", label: "Speaker 2", speaker_id: null, suggested_id: "s1", suggested_name: "Priya", suggested_score: 0.91 },
];

describe("SpeakersPanel", () => {
  it("renames a label from its row", () => {
    const onRename = vi.fn();
    render(<SpeakersPanel stats={stats} speakers={[]} onRename={onRename} />);
    fireEvent.click(screen.getByRole("button", { name: "Speaker 2" }));
    const input = screen.getByLabelText("Name for Speaker 2") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "  Marcus  " } });
    fireEvent.submit(input.closest("form")!);
    expect(onRename).toHaveBeenCalledWith("Speaker 2", "Marcus");
  });

  it("merges by naming a row after another speaker", () => {
    const onRename = vi.fn();
    render(<SpeakersPanel stats={stats} speakers={[]} onRename={onRename} />);
    fireEvent.click(screen.getByRole("button", { name: "Speaker 2" }));
    const input = screen.getByLabelText("Name for Speaker 2") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "Sarah" } });
    fireEvent.submit(input.closest("form")!);
    expect(onRename).toHaveBeenCalledWith("Speaker 2", "Sarah");
  });

  it("merges from the 'same person as' picker, offering only the other speakers", () => {
    const onRename = vi.fn();
    render(<SpeakersPanel stats={stats} speakers={[]} onRename={onRename} />);
    const select = screen.getByLabelText("Merge Speaker 2 into another speaker") as HTMLSelectElement;
    expect([...select.options].map((o) => o.value)).toEqual(["", "Sarah"]);
    fireEvent.change(select, { target: { value: "Sarah" } });
    expect(onRename).toHaveBeenCalledWith("Speaker 2", "Sarah");
  });

  it("offers a voice-memory suggestion and keeps placeholders out of the name list", () => {
    const accept = vi.fn();
    render(<SpeakersPanel stats={stats} speakers={speakers} nameOptions={["Marcus Webb"]} onRename={() => {}} onAcceptSuggestion={accept} />);
    expect(screen.getByText("Priya")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("Yes, this is Priya"));
    expect(accept).toHaveBeenCalledWith("Speaker 2");

    fireEvent.click(screen.getByRole("button", { name: "Sarah" }));
    const list = screen.getByLabelText("Name for Sarah").getAttribute("list")!;
    const values = [...document.querySelectorAll(`#${CSS.escape(list)} option`)].map((o) => o.getAttribute("value"));
    expect(values).toContain("Marcus Webb");
    expect(values).toContain("Sarah");
    expect(values).not.toContain("Speaker 2");
  });
});

it("localizes the merge picker while passing the original placeholder value", async () => {
  await i18n.changeLanguage("ru");
  const onRename = vi.fn();
  const rows = [{ label: "Speaker 1", segments: 21, ms: 1000, first_ms: 0 }, { label: "Speaker 2", segments: 5, ms: 1000, first_ms: 1000 }];
  render(<SpeakersPanel stats={rows} speakers={[]} onRename={onRename} />);
  const select = screen.getByLabelText("Объединить Участник 2 с другим участником");
  expect(screen.getByRole("option", { name: "Участник 1" })).toHaveValue("Speaker 1");
  expect(screen.getByText("21 реплика")).toBeInTheDocument();
  expect(screen.getByText("5 реплик")).toBeInTheDocument();
  fireEvent.change(select, { target: { value: "Speaker 1" } });
  expect(onRename).toHaveBeenCalledWith("Speaker 2", "Speaker 1");
});

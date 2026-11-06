import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { SpeakerChip } from "./SpeakerChip";

describe("SpeakerChip", () => {
  it("renames with a trimmed name on submit", () => {
    const onRename = vi.fn();
    render(<SpeakerChip label="Speaker 1" colorClass="" onRename={onRename} />);
    fireEvent.click(screen.getByText("Speaker 1"));
    const input = screen.getByLabelText("Speaker name") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "  Sarah  " } });
    fireEvent.submit(input.closest("form")!);
    expect(onRename).toHaveBeenCalledWith("Sarah");
  });

  it("shows a confirmable suggestion", () => {
    const accept = vi.fn();
    render(<SpeakerChip label="Speaker 2" colorClass="" suggestion={{ name: "Bob", score: 0.8 }} onRename={() => {}} onAcceptSuggestion={accept} />);
    expect(screen.getByText("Bob")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Yes"));
    expect(accept).toHaveBeenCalled();
  });
});

it("displays a Russian placeholder without saving it as a person's name", async () => {
  await i18n.changeLanguage("ru");
  const onRename = vi.fn();
  render(<SpeakerChip label="Speaker 1" colorClass="" onRename={onRename} />);
  fireEvent.click(screen.getByRole("button", { name: "Участник 1" }));
  const input = screen.getByLabelText("Имя участника");
  expect(input).toHaveValue("Speaker 1");
  fireEvent.submit(input.closest("form")!);
  expect(onRename).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Участник 1" })).toBeInTheDocument();
});

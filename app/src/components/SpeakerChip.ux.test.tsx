import { useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { SpeakerChip } from "./SpeakerChip";

afterEach(cleanup);

it("keeps a speaker rename available through pending and failure, then lets the user retry", async () => {
  let fail!: (reason: Error) => void;
  const rename = vi.fn()
    .mockImplementationOnce(() => new Promise<void>((_resolve, reject) => { fail = reject; }))
    .mockResolvedValue(undefined);
  function Subject() {
    const [label, setLabel] = useState("Speaker 1");
    return <SpeakerChip label={label} colorClass="" onRename={async (name) => {
      await rename(name);
      setLabel(name);
    }} />;
  }
  render(<Subject />);
  const trigger = screen.getByRole("button", { name: "Speaker 1" });
  trigger.focus();
  fireEvent.click(trigger);
  const editor = screen.getByRole("textbox", { name: "Speaker name" });
  expect(editor).toHaveFocus();
  fireEvent.change(editor, { target: { value: "  Sarah  " } });
  fireEvent.submit(editor.closest("form")!);
  expect(rename).toHaveBeenCalledExactlyOnceWith("Sarah");
  expect(screen.getByRole("textbox", { name: "Speaker name" })).toHaveValue("  Sarah  ");
  expect(screen.getByRole("button", { name: "Save name" })).toBeDisabled();
  fireEvent.submit(editor.closest("form")!);
  expect(rename).toHaveBeenCalledTimes(1);

  await act(async () => { fail(new Error("Temporary save failure")); });
  expect(screen.getByRole("alert")).toHaveTextContent("Temporary save failure");
  expect(screen.getByRole("textbox", { name: "Speaker name" })).toHaveValue("  Sarah  ");
  expect(screen.getByRole("textbox", { name: "Speaker name" })).toHaveFocus();
  expect(screen.getByRole("button", { name: "Save name" })).toBeEnabled();
  await act(async () => { fireEvent.submit(screen.getByRole("textbox").closest("form")!); });
  expect(rename).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Sarah" })).toHaveFocus();
});

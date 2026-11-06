import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Transcript } from "./Transcript";

afterEach(cleanup);

it("returns keyboard focus to the correction action after cancelling and completing an edit", async () => {
  let finish!: () => void;
  const onSetCorrection = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
  const onSeek = vi.fn();
  render(<Transcript
    segments={[{ id: 7, start_ms: 5000, end_ms: 9000, speaker: null, text: "Original statement", clean_text: null }]}
    currentMs={0} onSeek={onSeek} onSetCorrection={onSetCorrection} autoScroll={false}
  />);
  const trigger = screen.getByRole("button", { name: "Edit correction at 0:05" });
  trigger.focus();
  fireEvent.click(trigger);
  const firstEditor = screen.getByRole("textbox", { name: "Corrected text" });
  expect(firstEditor).toHaveFocus();
  fireEvent.change(firstEditor, { target: { value: "Discard this draft" } });
  fireEvent.keyDown(firstEditor, { key: "Escape" });
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  expect(onSetCorrection).not.toHaveBeenCalled();

  fireEvent.click(trigger);
  const secondEditor = screen.getByRole("textbox", { name: "Corrected text" });
  expect(secondEditor).toHaveValue("Original statement");
  fireEvent.change(secondEditor, { target: { value: "Corrected statement" } });
  fireEvent.keyDown(secondEditor, { key: "Enter", ctrlKey: true });
  expect(onSetCorrection).toHaveBeenCalledExactlyOnceWith(7, "Corrected statement");
  expect(secondEditor).toBeDisabled();
  await act(async () => { finish(); });
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  expect(onSeek).not.toHaveBeenCalled();
});

it("returns focus to a failed correction draft so keyboard retry can complete the task", async () => {
  const pending = (() => {
    let reject!: (error: Error) => void;
    const promise = new Promise<void>((_resolve, fail) => { reject = fail; });
    return { promise, reject };
  })();
  const onSetCorrection = vi.fn().mockReturnValueOnce(pending.promise).mockResolvedValue(undefined);
  render(<Transcript
    segments={[{ id: 7, start_ms: 5000, end_ms: 9000, speaker: null, text: "Original statement", clean_text: null }]}
    currentMs={0} onSeek={vi.fn()} onSetCorrection={onSetCorrection} autoScroll={false}
  />);
  const trigger = screen.getByRole("button", { name: "Edit correction at 0:05" });
  fireEvent.click(trigger);
  const editor = screen.getByRole("textbox", { name: "Corrected text" });
  fireEvent.change(editor, { target: { value: "Retain this correction" } });
  const save = screen.getByRole("button", { name: "Save correction" });
  save.focus();
  fireEvent.click(save);
  await act(async () => { pending.reject(new Error("Temporary save failure")); });
  expect(screen.getByRole("alert")).toHaveTextContent("Temporary save failure");
  expect(editor).toHaveValue("Retain this correction");
  expect(editor).toHaveFocus();
  await act(async () => { fireEvent.keyDown(editor, { key: "Enter", ctrlKey: true }); });
  expect(onSetCorrection).toHaveBeenCalledTimes(2);
  expect(onSetCorrection).toHaveBeenLastCalledWith(7, "Retain this correction");
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
});

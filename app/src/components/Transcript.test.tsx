import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import * as format from "@/lib/format";
import { Transcript } from "./Transcript";

const segs = [
  { id: 1, start_ms: 0, end_ms: 1000, speaker: "Me", text: "hello there", clean_text: null },
  { id: 2, start_ms: 1000, end_ms: 2500, speaker: "Sarah", text: "hi um how are you", clean_text: "Hi, how are you?" },
  { id: 3, start_ms: 2500, end_ms: 4000, speaker: "Sarah", text: "good", clean_text: null },
];

describe("Transcript", () => {
  it("shows source and uncertainty badges and refuses unmapped bot timestamps", () => {
    const onSeek = vi.fn();
    render(<Transcript segments={[
      { ...segs[0], provenance: { source: "bot", provider: "discord", source_start_ms: 10_000, source_end_ms: 11_000, uncertain: true } },
      { ...segs[1], provenance: { source: "system", source_start_ms: 1000, source_end_ms: 2500, uncertain: false } },
      { ...segs[2], provenance: { source: "microphone", source_start_ms: 2500, source_end_ms: 4000, uncertain: false } },
    ]} currentMs={0} onSeek={onSeek} autoScroll={false} />);
    expect(screen.getByText("Bot track · Discord")).toBeVisible();
    expect(screen.getByText("System audio")).toBeVisible();
    expect(screen.getByText("Microphone")).toBeVisible();
    expect(screen.getByText("Source or timing uncertain")).toBeVisible();
    expect(screen.getByText("hello there").closest("button")).toBeDisabled();
    expect(screen.getByText("hello there").closest("button")).not.toHaveAttribute("data-active");
    fireEvent.click(screen.getByText("hello there"));
    fireEvent.click(screen.getByText("hi um how are you"));
    expect(onSeek).toHaveBeenCalledExactlyOnceWith(1000);
  });

  it("keeps text and correction controls without playback highlights or seek buttons", () => {
    const { container } = render(<Transcript segments={segs} currentMs={1200} onSetCorrection={vi.fn()} />);
    expect(screen.getByText("hi um how are you").closest("button")).toBeDisabled();
    expect(container.querySelector("[data-active]")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit correction at 0:01" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:01" }));
    expect(screen.getByRole("textbox", { name: "Corrected text" })).toHaveValue("Hi, how are you?");
  });

  it("reuses row presentation between playback ticks and updates only the changed active rows", () => {
    const stamp = vi.spyOn(format, "fmtStamp");
    const onSeek = vi.fn();
    const { rerender } = render(<Transcript segments={segs} currentMs={0} onSeek={onSeek} autoScroll={false} />);
    stamp.mockClear();
    rerender(<Transcript segments={segs} currentMs={900} onSeek={onSeek} autoScroll={false} />);
    expect(stamp).not.toHaveBeenCalled();
    rerender(<Transcript segments={segs} currentMs={1200} onSeek={onSeek} autoScroll={false} />);
    expect(stamp).toHaveBeenCalledTimes(2);
    expect(document.querySelector("[data-active]")).toHaveTextContent("hi um how are you");
    stamp.mockRestore();
  });

  it("keeps correction drafts and live locale updates while playback moves to another row", async () => {
    const onSeek = vi.fn(), onSetCorrection = vi.fn().mockResolvedValue(undefined);
    const { rerender } = render(<Transcript segments={segs} currentMs={0} onSeek={onSeek} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Draft retained during playback" } });
    rerender(<Transcript segments={segs} currentMs={1200} onSeek={onSeek} onSetCorrection={onSetCorrection} autoScroll={false} />);
    expect(screen.getByRole("textbox")).toHaveValue("Draft retained during playback");
    await act(async () => { await i18n.changeLanguage("ru"); });
    expect(screen.getByRole("textbox", { name: "Исправленный текст" })).toHaveValue("Draft retained during playback");
    expect(screen.getByRole("button", { name: "Исправить текст в 0:02" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Сохранить исправление" }));
    await waitFor(() => expect(onSetCorrection).toHaveBeenCalledWith(1, "Draft retained during playback"));
  });

  it("highlights the segment covering currentMs", () => {
    render(<Transcript segments={segs} currentMs={1200} onSeek={() => {}} autoScroll={false} />);
    const active = document.querySelector("[data-active]") as HTMLElement;
    expect(active).toHaveTextContent("hi um how are you");
  });

  it("calls onSeek with the segment start when clicked", () => {
    const onSeek = vi.fn();
    render(<Transcript segments={segs} currentMs={0} onSeek={onSeek} autoScroll={false} />);
    fireEvent.click(screen.getByText("good"));
    expect(onSeek).toHaveBeenCalledWith(2500);
  });

  it("uses verified playback times for clicks and highlights even when track order differs", () => {
    const onSeek = vi.fn();
    const timings = new Map([
      [1, { start_ms: 5200, end_ms: 6500 }],
      [2, { start_ms: 4100, end_ms: 5100 }],
    ]);
    render(<Transcript segments={segs} currentMs={4300} onSeek={onSeek} playbackTimings={timings} autoScroll={false} />);
    const line = screen.getByText("hi um how are you");
    expect(line.closest("button")).toHaveAttribute("data-active", "true");
    expect(line.closest("button")).toHaveTextContent("0:04");
    const unmapped = screen.getByText("good");
    expect(unmapped.closest("button")).toBeDisabled();
    fireEvent.click(unmapped);
    fireEvent.click(line);
    expect(onSeek).toHaveBeenCalledExactlyOnceWith(4100);
  });

  it("shows clean text when asked and groups consecutive speakers", () => {
    render(<Transcript segments={segs} currentMs={0} onSeek={() => {}} useClean autoScroll={false} />);
    expect(screen.getByText("Hi, how are you?")).toBeInTheDocument();
    expect(screen.getAllByText("Sarah")).toHaveLength(1);
  });

  it("saves a correction with Ctrl+Enter without seeking or changing the read-only original", async () => {
    const onSeek = vi.fn(), onSetCorrection = vi.fn().mockResolvedValue(undefined);
    render(<Transcript segments={segs} currentMs={0} onSeek={onSeek} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    const input = screen.getByRole("textbox", { name: "Corrected text" });
    expect(input).toHaveValue("hello there");
    expect(screen.getByText("Original text")).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "Hello, corrected." } });
    fireEvent.keyDown(input, { key: "Enter", ctrlKey: true });
    await waitFor(() => expect(screen.queryByRole("textbox")).not.toBeInTheDocument());
    expect(onSetCorrection).toHaveBeenCalledExactlyOnceWith(1, "Hello, corrected.");
    expect(onSeek).not.toHaveBeenCalled();
    expect(screen.getByText("hello there")).toBeInTheDocument();
    expect(document.querySelector("button button")).toBeNull();
    fireEvent.click(screen.getByText("good"));
    expect(onSeek).toHaveBeenCalledWith(2500);
  });

  it("cancels editing with Escape and preserves the draft when saving fails", async () => {
    const onSetCorrection = vi.fn().mockRejectedValue({ code: "native.transcriptCorrectionSaveFailed", args: {} });
    render(<Transcript segments={segs} currentMs={0} onSeek={() => {}} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Discard this" } });
    fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(onSetCorrection).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Keep the failed draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Save correction" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/save/i);
    expect(screen.getByRole("textbox")).toHaveValue("Keep the failed draft");
    expect(screen.getByRole("button", { name: "Save correction" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel correction" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("saves an empty correction verbatim and resets a manual correction with null", async () => {
    const onSetCorrection = vi.fn().mockResolvedValue(undefined);
    render(<Transcript segments={[{ ...segs[0], clean_text: "Existing correction", clean_text_manual: true }]} currentMs={0} onSeek={() => {}} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Save correction" }));
    await waitFor(() => expect(screen.queryByRole("textbox")).not.toBeInTheDocument());
    expect(onSetCorrection).toHaveBeenLastCalledWith(1, "");
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.click(screen.getByRole("button", { name: "Reset correction" }));
    await waitFor(() => expect(screen.queryByRole("textbox")).not.toBeInTheDocument());
    expect(onSetCorrection).toHaveBeenLastCalledWith(1, null);
  });

  it("shows an intentionally blank corrected representation while Raw keeps the words", () => {
    const segments = [{ ...segs[0], clean_text: "", clean_text_manual: true }];
    const { rerender } = render(<Transcript segments={segments} currentMs={0} onSeek={() => {}} useClean autoScroll={false} />);
    expect(screen.getByText("Text hidden by a manual correction")).toBeInTheDocument();
    expect(screen.queryByText("hello there")).not.toBeInTheDocument();
    rerender(<Transcript segments={segments} currentMs={0} onSeek={() => {}} useClean={false} autoScroll={false} />);
    expect(screen.getByText("hello there")).toBeInTheDocument();
  });

  it("does not let an old pending save close another meeting's editor", async () => {
    let finish!: () => void;
    const onSetCorrection = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    const { rerender } = render(<Transcript key="old" segments={[segs[0]]} currentMs={0} onSeek={() => {}} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.click(screen.getByRole("button", { name: "Save correction" }));
    expect(screen.getByRole("button", { name: "Cancel correction" })).toBeDisabled();
    rerender(<Transcript key="new" segments={[{ ...segs[0], text: "Another meeting" }]} currentMs={0} onSeek={() => {}} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Edit correction at 0:00" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "New meeting draft" } });
    await act(async () => { finish(); });
    expect(screen.getByRole("textbox")).toHaveValue("New meeting draft");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("localizes correction controls and native validation errors in Russian", async () => {
    await i18n.changeLanguage("ru");
    const onSetCorrection = vi.fn().mockRejectedValue({ code: "native.transcriptCorrectionTooLarge", args: { maxBytes: 65536 } });
    render(<Transcript segments={[segs[0]]} currentMs={0} onSeek={() => {}} onSetCorrection={onSetCorrection} autoScroll={false} />);
    fireEvent.click(screen.getByRole("button", { name: "Исправить текст в 0:00" }));
    expect(screen.getByRole("textbox", { name: "Исправленный текст" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Сохранить исправление" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("65536");
    expect(screen.getByRole("button", { name: "Отменить исправление" })).toBeInTheDocument();
  });
});

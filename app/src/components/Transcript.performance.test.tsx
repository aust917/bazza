import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as format from "@/lib/format";
import { Transcript, type TranscriptProps } from "./Transcript";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function longTranscript(count: number) {
  let startReads = 0;
  const segments: TranscriptProps["segments"] = Array.from({ length: count }, (_, index) => ({
    id: index + 1,
    // Count input work rather than elapsed jsdom time. A linear active-row search
    // may look fast on a developer machine but grows on every playback tick.
    get start_ms() { startReads++; return index * 4000; },
    end_ms: index * 4000 + 1000,
    speaker: `Speaker ${Math.floor(index / 4) % 4}`,
    text: `Original words ${index}`,
    clean_text: `Corrected words ${index}`,
  }));
  return { segments, resetReads: () => { startReads = 0; }, reads: () => startReads };
}

describe("transcript playback work budget", () => {
  it.each([128, 2048])("keeps lookup logarithmic and row presentation bounded with %i segments", (count) => {
    const fixture = longTranscript(count);
    const stamp = vi.spyOn(format, "fmtStamp");
    const onSeek = vi.fn();
    const onSetCorrection = vi.fn();
    const props = { segments: fixture.segments, onSeek, onSetCorrection, useClean: true, autoScroll: false };
    const { container, rerender } = render(<Transcript {...props} currentMs={0} />);
    const activeText = () => container.querySelector("[data-active]")?.textContent;
    expect(activeText()).toContain("Corrected words 0");

    const checkTick = (currentMs: number, changedRows: number, active: number | null) => {
      stamp.mockClear();
      fixture.resetReads();
      rerender(<Transcript {...props} currentMs={currentMs} />);
      // Editable rows format a timestamp for both the row and its edit action.
      // The budget is independent of total transcript length.
      expect(stamp).toHaveBeenCalledTimes(changedRows * 2);
      expect(fixture.reads()).toBeLessThanOrEqual(Math.ceil(Math.log2(count)) + changedRows * 2 + 1);
      if (active === null) expect(activeText()).toBeUndefined();
      else expect(activeText()).toContain(`Corrected words ${active}`);
    };

    // Repeated media timeupdate events in one utterance must do no row work.
    for (const time of [100, 200, 400, 800, 999]) checkTick(time, 0, 0);
    checkTick(4000, 2, 1);
    checkTick((count - 1) * 4000, 2, count - 1);
    checkTick((count - 1) * 4000 + 3000, 1, null); // past the trailing grace period
    checkTick(0, 1, 0); // seek backwards after playback ends

    const target = container.querySelector("[data-active]") as HTMLButtonElement;
    fireEvent.click(target);
    expect(onSeek).toHaveBeenCalledExactlyOnceWith(0);
    expect(onSetCorrection).not.toHaveBeenCalled();
  });
});

import { describe, expect, it } from "vitest";
import { safeFileName, segmentText, transcriptMd, transcriptSrt, transcriptTxt } from "./export";
import type { Segment } from "./ipc";

const segs: Segment[] = [
  { id: 1, meeting_id: "m", start_ms: 0, end_ms: 1500, speaker: "Sarah", text: "um hi", clean_text: "Hi." },
  { id: 2, meeting_id: "m", start_ms: 61_000, end_ms: 62_000, speaker: null, text: "no speaker", clean_text: null },
  { id: 3, meeting_id: "m", start_ms: 3_661_200, end_ms: 3_661_200, speaker: "Me", text: "late", clean_text: null },
];

describe("export", () => {
  it("txt uses clean text when asked and stamps hours only past 1h", () => {
    expect(transcriptTxt(segs, true)).toBe("[0:00] Sarah: Hi.\n[1:01] no speaker\n[1:01:01] Me: late");
    expect(transcriptTxt(segs, false).startsWith("[0:00] Sarah: um hi")).toBe(true);
  });

  it("srt numbers cues, pads timestamps and never emits a zero-length cue", () => {
    const srt = transcriptSrt(segs, true);
    expect(srt).toContain("1\n00:00:00,000 --> 00:00:01,500\nSarah: Hi.\n");
    expect(srt).toContain("3\n01:01:01,200 --> 01:01:01,700\nMe: late\n");
  });

  it("keeps deliberately blank corrections out of corrected exports while raw stays available", () => {
    for (const clean_text of ["", " \n\t "]) {
      const corrected = [{ ...segs[0], text: "Original words stay available", clean_text }];
      expect(segmentText(corrected[0], true)).toBe("");
      expect(transcriptTxt(corrected, true)).toBe("[0:00] Sarah: ");
      expect(transcriptSrt(corrected, true)).toBe("");
      expect(transcriptSrt(corrected, true)).not.toContain("Original words");
      expect(transcriptMd("Sync", "Sep 15", corrected, true)).not.toContain("Original words");
      expect(transcriptTxt(corrected, false)).toContain("Original words stay available");
      expect(transcriptSrt(corrected, false)).toContain("Original words stay available");
      expect(transcriptMd("Sync", "Sep 15", corrected, false)).toContain("Original words stay available");
    }
  });

  it("omits blank corrected cues with or without speakers and renumbers the rest", () => {
    const corrected: Segment[] = [
      { ...segs[0], clean_text: "", clean_text_manual: true },
      { ...segs[1], clean_text: " \n\t ", clean_text_manual: true },
      segs[2],
    ];
    expect(transcriptSrt(corrected, true)).toBe("1\n01:01:01,200 --> 01:01:01,700\nMe: late\n");
    const original = transcriptSrt(corrected, false);
    expect(original).toContain("1\n00:00:00,000 --> 00:00:01,500\nSarah: um hi\n");
    expect(original).toContain("2\n00:01:01,000 --> 00:01:02,000\nno speaker\n");
    expect(original).toContain("3\n01:01:01,200 --> 01:01:01,700\nMe: late\n");
  });

  it("md puts the summary before the transcript", () => {
    const md = transcriptMd("Sync", "Sep 15", segs, true, "## Decisions\n- ship");
    expect(md.indexOf("## Decisions")).toBeLessThan(md.indexOf("## Transcript"));
    expect(md).toContain("**0:00** **Sarah:** Hi.");
    expect(transcriptMd("Sync", "Sep 15", segs, true, null)).not.toContain("---");
  });

  it("file names drop reserved characters", () => {
    expect(safeFileName('Q3: "plan" <draft>')).toBe("Q3 plan draft");
    expect(safeFileName("   ")).toBe("meeting");
  });
});

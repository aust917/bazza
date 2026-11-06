import { expect, it } from "vitest";
import type { Meeting, MeetingDetail } from "./ipc";
import { mockInvoke } from "./mock";

const ids = (rows: Meeting[]) => rows.map((meeting) => meeting.id);
async function filtered(folder?: unknown, tag_id: string | null = null) {
  return mockInvoke<Meeting[]>("list_meetings_filtered", { filter: { folder, tag_id } });
}

it("matches native all, unfiled and folder filters without collapsing [null]", async () => {
  const all = await mockInvoke<Meeting[]>("list_meetings");
  expect(await filtered()).toEqual(all);
  expect(await filtered(null)).toEqual(all);
  expect(ids(await filtered([null]))).toEqual(["m2", "m4"]);
  expect(ids(await filtered(["f1"]))).toEqual(["m1"]);
  expect(ids(await filtered(["f2"]))).toEqual(["m3"]);
  expect(ids(await filtered("f1"))).toEqual(["m1"]);
  expect(await filtered(["missing"])).toEqual([]);
});

it("intersects tag and folder selections", async () => {
  expect(ids(await filtered(null, "t2"))).toEqual(["m1", "m2"]);
  expect(ids(await filtered([null], "t2"))).toEqual(["m2"]);
  expect(await filtered(["f1"], "t1")).toEqual([]);
});

it("rejects malformed folder arrays like the native contract", async () => {
  for (const folder of [[], ["f1", "f2"], [4], false]) await expect(filtered(folder)).rejects.toThrow();
});

it("provides an explicit text-only route fixture without changing the recorded sample library", async () => {
  const text = await mockInvoke<MeetingDetail>("get_meeting", { id: "text-only" });
  expect(text.media).toEqual({ audio: null, video: null });
  expect(text.segments[0].meeting_id).toBe("text-only");
  expect(text.segments[0].text).toBe("Text-only fixture transcript.");
  expect(text.meeting.duration_ms).toBe(1000);
  expect(await mockInvoke("get_summary", { id: "text-only" })).toBeNull();
  expect(ids(await mockInvoke<Meeting[]>("list_meetings"))).toEqual(["m1", "m2", "m3", "m4"]);
  expect((await mockInvoke<MeetingDetail>("get_meeting", { id: "m1" })).media.audio).toBe("");
});

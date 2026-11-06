import { afterEach, expect, it, vi } from "vitest";
import i18n from "./i18n";
import { fmtDate, fmtTime } from "./format";

afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); });

it("bounds formatter construction by locale and year shape, regardless of library size", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-11-02T12:00:00Z"));
  const NativeDateTimeFormat = Intl.DateTimeFormat;
  const constructors = vi.spyOn(Intl, "DateTimeFormat");
  // toLocale* constructs a formatter internally, bypassing the observable JS constructor.
  const perRowDates = vi.spyOn(Date.prototype, "toLocaleDateString");
  const perRowTimes = vi.spyOn(Date.prototype, "toLocaleTimeString");
  const dates = Array.from({ length: 1_000 }, (_, i) =>
    new Date(Date.UTC(i % 2 ? 2026 : 2025, i % 12, i % 28 + 1, 12, i % 60)),
  );

  for (const locale of ["en", "ru", "en"] as const) {
    await i18n.changeLanguage(locale);
    const language = locale === "ru" ? "ru-RU" : "en-US";
    const sameYear = new NativeDateTimeFormat(language, { month: "short", day: "numeric" });
    const otherYear = new NativeDateTimeFormat(language, { month: "short", day: "numeric", year: "numeric" });
    const time = new NativeDateTimeFormat(language, { hour: "numeric", minute: "2-digit" });
    for (const date of dates) {
      const iso = date.toISOString();
      expect(fmtDate(iso)).toBe((date.getFullYear() === 2026 ? sameYear : otherYear).format(date));
      expect(fmtTime(iso)).toBe(time.format(date));
    }
  }
  // Two date shapes and one time shape per supported locale, not per meeting.
  expect(constructors.mock.calls.length).toBeLessThanOrEqual(6);
  const coldConstructions = constructors.mock.calls.length;
  for (const date of dates) {
    fmtDate(date.toISOString(), "ru");
    fmtTime(date.toISOString(), "ru");
  }
  expect(constructors).toHaveBeenCalledTimes(coldConstructions);
  expect(perRowDates).not.toHaveBeenCalled();
  expect(perRowTimes).not.toHaveBeenCalled();

  // Crossing New Year must select the already cached year-bearing shape.
  vi.setSystemTime(new Date("2027-01-02T12:00:00Z"));
  const iso = "2026-05-10T12:34:00Z";
  expect(fmtDate(iso, "ru")).toBe(new NativeDateTimeFormat("ru-RU", {
    month: "short", day: "numeric", year: "numeric",
  }).format(new Date(iso)));
  expect(fmtDate("invalid", "en")).toBe("Invalid Date");
  expect(fmtTime("invalid", "ru")).toBe("Invalid Date");
  expect(constructors).toHaveBeenCalledTimes(coldConstructions);
});

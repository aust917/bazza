import { createInstance } from "i18next";
import { describe, expect, it, vi } from "vitest";
import i18n, { connectLocale, displayError, displayMessage, resources, type UiLocale } from "./i18n";
import { fmtBytes, fmtDate, fmtDuration, fmtStamp, fmtTime, speakerLabel } from "./format";

function entries(value: object, prefix = ""): [string, string][] {
  return Object.entries(value).flatMap(([key, item]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    return typeof item === "string" ? [[path, item]] : entries(item, path);
  });
}
const baseKey = (key: string) => key.replace(/_(one|few|many|other|zero|two)$/, "");
function parametersByMessage(value: object) {
  const result = new Map<string, Set<string>>();
  for (const [key, text] of entries(value)) {
    const parameters = result.get(baseKey(key)) ?? new Set<string>();
    for (const match of text.matchAll(/{{\s*([\w]+)(?:,[^}]*)?\s*}}/g)) parameters.add(match[1]);
    result.set(baseKey(key), parameters);
  }
  return result;
}

describe("bundled locale catalogs", () => {
  it("covers the same messages and interpolation parameters in Russian and English", () => {
    const en = parametersByMessage(resources.en);
    const ru = parametersByMessage(resources.ru);
    expect([...ru.keys()].sort()).toEqual([...en.keys()].sort());
    for (const [key, parameters] of en) expect([...ru.get(key)!].sort(), key).toEqual([...parameters].sort());
    for (const [key] of entries(resources.ru)) {
      if (key.endsWith("_other")) {
        const keys = entries(resources.ru).map(([path]) => path);
        for (const suffix of new Intl.PluralRules("ru").resolvedOptions().pluralCategories) {
          expect(keys, `${baseKey(key)}_${suffix}`).toContain(`${baseKey(key)}_${suffix}`);
        }
      }
    }
  });

  it("uses a real English fallback when a Russian resource is unavailable", async () => {
    const instance = createInstance();
    await instance.init({ lng: "ru", fallbackLng: "en", initAsync: false,
      resources: { ru: { common: {} }, en: { common: resources.en.common } }, defaultNS: "common" });
    expect(instance.t("operationFailed")).toBe("Could not complete the action.");
  });

  it("renders owned errors by code and keeps unknown technical diagnostics", async () => {
    await i18n.changeLanguage("ru");
    const text = displayMessage({ code: "native.operationFailed", message: "English diagnostic", detail: "EIO" });
    expect(text).toMatch(/[А-Яа-я]/);
    expect(text).toContain("EIO");
    expect(displayError("legacy detail")).toContain("legacy detail");
    expect(displayMessage({ code: "future.unknown", message: "provider message" })).toContain("provider message");
  });

  it("resolves Russian plural-only native notices without falling back to English", async () => {
    await i18n.changeLanguage("ru");
    for (const [count, suffix] of [[0, "встреч."], [1, "встреча."], [2, "встречи."], [5, "встреч."], [11, "встреч."], [21, "встреча."], [22, "встречи."], [25, "встреч."], [101, "встреча."]] as const) {
      const text = displayMessage({ code: "native.modelReady", args: { count }, message: "Old English fallback" });
      expect(text).toContain(String(count));
      expect(text.endsWith(suffix)).toBe(true);
      expect(text).not.toContain("English");
    }
    expect(displayMessage({ code: "native.speakerMerged", args: { from: "Speaker 1", name: "Анна", count: 21 } })).toContain("Участник 1");
  });

  it("formats presentation in the chosen locale while preserving machine timestamps and names", async () => {
    await i18n.changeLanguage("ru");
    expect(document.documentElement.lang).toBe("ru");
    expect(fmtBytes(1536)).toBe("1,5 КБ");
    expect(fmtDate("2025-11-02T12:34:00Z")).toMatch(/нояб/);
    expect(fmtTime("2025-11-02T12:34:00Z")).toMatch(/^\d{1,2}:\d{2}$/);
    expect(fmtStamp(3725000)).toBe("1:02:05");
    expect(fmtDuration(65000)).toBe("1:05");
    expect(speakerLabel("Speaker 12")).toBe("Участник 12");
    expect(speakerLabel("Me")).toBe("Me");
    expect(speakerLabel("Мария")).toBe("Мария");
    await i18n.changeLanguage("en");
    expect(fmtBytes(1536)).toBe("1.5 KB");
    expect(speakerLabel("Speaker 12")).toBe("Speaker 12");
  });
});

describe("locale synchronization lifecycle", () => {
  it("keeps main, hidden popup and recordbar connected and initializes later windows", async () => {
    let saved: UiLocale = "ru";
    const listeners = new Set<(locale: UiLocale) => void>();
    const source = {
      read: async () => saved,
      listen: async (handler: (locale: UiLocale) => void) => { listeners.add(handler); return () => { listeners.delete(handler); }; },
    };
    const main = vi.fn(), popup = vi.fn(), recordbar = vi.fn();
    const dispose = await Promise.all([main, popup, recordbar].map((apply) => connectLocale(source, apply)));
    expect(listeners.size).toBe(3);
    saved = "en";
    listeners.forEach((listener) => listener(saved));
    for (const window of [main, popup, recordbar]) expect(window.mock.calls.map(([locale]) => locale)).toEqual(["ru", "en"]);
    dispose[1]();
    const reopened = vi.fn();
    const closeReopened = await connectLocale(source, reopened);
    expect(reopened).toHaveBeenLastCalledWith("en");
    dispose.forEach((close) => close());
    closeReopened();
    expect(listeners.size).toBe(0);
  });

  it("does not replace a newer event with a stale initial read", async () => {
    let resolveRead!: (value: UiLocale) => void;
    let notify!: (locale: UiLocale) => void;
    const apply = vi.fn();
    const pending = connectLocale({
      read: () => new Promise<UiLocale>((resolve) => { resolveRead = resolve; }),
      listen: async (handler) => { notify = handler; return () => {}; },
    }, apply);
    await Promise.resolve();
    notify("en");
    resolveRead("ru");
    const close = await pending;
    expect(apply.mock.calls).toEqual([["en"]]);
    close();
  });

  it("releases the subscription if reading fails", async () => {
    const close = vi.fn();
    await expect(connectLocale({ read: async () => { throw new Error("read failed"); }, listen: async () => close })).rejects.toThrow("read failed");
    expect(close).toHaveBeenCalledOnce();
  });
});

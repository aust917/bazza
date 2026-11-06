import { describe, expect, it } from "vitest";
import i18n from "@/lib/i18n";
import type { Template } from "@/lib/ipc";
import { templateName, templateDescription } from "./templateLabels";

const original: Template = { id: "general", name: "General meeting", description: "Balanced notes for any meeting: summary, decisions, action items, open questions, key moments.", body: "Keep this exact prompt.", builtin: true };

describe("builtin template display metadata", () => {
  it("translates shipped metadata without changing stored values", async () => {
    await i18n.changeLanguage("ru");
    const tr = i18n.getFixedT("ru", "meeting");
    expect(templateName(original, tr)).toBe("Общая встреча");
    expect(templateDescription(original, tr)).toContain("Универсальные заметки");
    expect(original.name).toBe("General meeting");
    expect(original.body).toBe("Keep this exact prompt.");
  });
  it("preserves edited builtin names and user templates with a builtin id", async () => {
    await i18n.changeLanguage("ru");
    const tr = i18n.getFixedT("ru", "meeting");
    expect(templateName({ ...original, name: "My edited name" }, tr)).toBe("My edited name");
    expect(templateDescription({ ...original, description: "My description" }, tr)).toBe("My description");
    expect(templateName({ ...original, builtin: false }, tr)).toBe("General meeting");
  });
});

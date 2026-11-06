import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { cmd } from "@/lib/ipc";
import { TemplatesEditor } from "./TemplatesEditor";

vi.mock("@/lib/ipc", () => ({ cmd: { listTemplates: vi.fn(), saveTemplate: vi.fn() } }));
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(cmd.listTemplates).mockResolvedValue([{ id: "custom", name: "My template", description: "", body: "Original prompt", builtin: false }]);
  vi.mocked(cmd.saveTemplate).mockRejectedValue(new Error("disk unavailable"));
});

it("updates a mounted save error when the interface language changes", async () => {
  render(<TemplatesEditor defaultId="custom" onDefaultChange={() => {}} />);
  fireEvent.click(await screen.findByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Error: disk unavailable"));
  await act(async () => { await i18n.changeLanguage("ru"); });
  expect(screen.getByRole("status")).toHaveTextContent("Не удалось выполнить действие.");
  expect(screen.getByRole("status")).toHaveTextContent("disk unavailable");
  expect(screen.getByLabelText("Промпт шаблона")).toHaveValue("Original prompt");
});

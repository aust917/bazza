import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { LanguageSelect } from "./LanguageSelect";

const setUiLocale = vi.hoisted(() => vi.fn());
vi.mock("@/lib/ipc", () => ({ cmd: { setUiLocale } }));
afterEach(() => { cleanup(); setUiLocale.mockReset(); });

describe("interface language preference", () => {
  it("does not overwrite a newer cross-window event with an older save response", async () => {
    let finish!: (locale: string) => void;
    setUiLocale.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    render(<LanguageSelect />);
    fireEvent.change(screen.getByLabelText("Interface language"), { target: { value: "ru" } });
    await act(async () => { await i18n.changeLanguage("en"); });
    await act(async () => { finish("ru"); });
    expect(screen.getByLabelText("Interface language")).toHaveValue("en");
  });
  it("persists the choice and renders the successful result", async () => {
    setUiLocale.mockResolvedValue("ru");
    render(<LanguageSelect />);
    fireEvent.change(screen.getByLabelText("Interface language"), { target: { value: "ru" } });
    await screen.findByLabelText("Язык интерфейса");
    expect(setUiLocale).toHaveBeenCalledWith("ru");
    expect(screen.getByLabelText("Язык интерфейса")).toHaveValue("ru");
  });
  it("keeps the previous language when persistence fails and updates an existing error on language change", async () => {
    setUiLocale.mockRejectedValue(new Error("disk unavailable"));
    render(<LanguageSelect />);
    fireEvent.change(screen.getByLabelText("Interface language"), { target: { value: "ru" } });
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Could not save"));
    expect(screen.getByLabelText("Interface language")).toHaveValue("en");
    await act(async () => { await i18n.changeLanguage("ru"); });
    expect(screen.getByRole("alert")).toHaveTextContent("Не удалось сохранить язык интерфейса");
    expect(screen.getByRole("alert")).toHaveTextContent("disk unavailable");
  });
});

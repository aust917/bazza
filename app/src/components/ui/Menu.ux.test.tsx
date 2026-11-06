import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Menu } from "./index";

afterEach(() => cleanup());

function menuItems(onSelect = vi.fn()) {
  return [
    { label: "Unavailable first", onSelect, disabled: true },
    { label: "Copy transcript", onSelect },
    { label: "Unavailable middle", onSelect, disabled: true },
    { label: "Save subtitles", onSelect },
    { label: "Unavailable last", onSelect, disabled: true },
  ];
}

it("opens from either arrow key and navigates enabled actions with arrows, Home and End", () => {
  render(<Menu label="Export" items={menuItems()} />);
  const trigger = screen.getByRole("button", { name: "Export" });
  trigger.focus();
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  const first = screen.getByRole("menuitem", { name: "Copy transcript" });
  const last = screen.getByRole("menuitem", { name: "Save subtitles" });
  expect(first).toHaveFocus();
  expect(trigger).toHaveAttribute("aria-expanded", "true");
  fireEvent.keyDown(first, { key: "ArrowDown" });
  expect(last).toHaveFocus();
  fireEvent.keyDown(last, { key: "ArrowDown" });
  expect(first).toHaveFocus();
  fireEvent.keyDown(first, { key: "ArrowUp" });
  expect(last).toHaveFocus();
  fireEvent.keyDown(last, { key: "Home" });
  expect(first).toHaveFocus();
  fireEvent.keyDown(first, { key: "End" });
  expect(last).toHaveFocus();
  fireEvent.keyDown(last, { key: "Escape" });
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  expect(trigger).toHaveAttribute("aria-expanded", "false");

  fireEvent.keyDown(trigger, { key: "ArrowUp" });
  expect(screen.getByRole("menuitem", { name: "Save subtitles" })).toHaveFocus();
});

it("focuses mouse-opened actions, selects once, restores focus and lets Tab leave normally", () => {
  const select = vi.fn();
  render(<><Menu label="Export" items={menuItems(select)} /><button>Next control</button></>);
  const trigger = screen.getByRole("button", { name: "Export" });
  fireEvent.click(trigger);
  const first = screen.getByRole("menuitem", { name: "Copy transcript" });
  expect(first).toHaveFocus();
  fireEvent.click(screen.getByRole("menuitem", { name: "Unavailable middle" }));
  expect(select).not.toHaveBeenCalled();
  expect(screen.getByRole("menu")).toBeInTheDocument();
  fireEvent.click(first);
  expect(select).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();

  fireEvent.click(trigger);
  const tab = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
  fireEvent(screen.getByRole("menuitem", { name: "Copy transcript" }), tab);
  expect(tab.defaultPrevented).toBe(false);
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  expect(select).toHaveBeenCalledTimes(1);

  fireEvent.click(trigger);
  fireEvent.mouseDown(screen.getByRole("button", { name: "Next control" }));
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  expect(select).toHaveBeenCalledTimes(1);
});

it("keeps a disabled trigger closed and lets an all-disabled menu escape without choosing an action", () => {
  const select = vi.fn();
  const items = [{ label: "Not available yet", onSelect: select, disabled: true }];
  const { rerender } = render(<Menu label="Export" items={items} disabled />);
  const trigger = screen.getByRole("button", { name: "Export" });
  expect(trigger).toBeDisabled();
  fireEvent.click(trigger);
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();

  rerender(<Menu label="Export" items={items} />);
  fireEvent.keyDown(trigger, { key: "ArrowUp" });
  expect(screen.getByRole("menu")).toHaveFocus();
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  expect(trigger).toHaveFocus();
  expect(select).not.toHaveBeenCalled();
});

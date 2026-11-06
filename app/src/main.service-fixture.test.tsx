import { beforeEach, expect, it, vi } from "vitest";

const startup = vi.hoisted(() => ({ initializeLocale: vi.fn(), render: vi.fn() }));
vi.mock("./lib/i18n", async (original) => ({ ...await original<typeof import("./lib/i18n")>(), initializeLocale: startup.initializeLocale }));
vi.mock("react-dom/client", () => ({ default: { createRoot: () => ({ render: startup.render }) } }));
vi.mock("./App", () => ({ default: () => null }));

beforeEach(() => { vi.resetModules(); startup.initializeLocale.mockReset(); startup.render.mockClear(); });

it("does not initialize the legacy locale or its event listener in the fixture window", async () => {
  history.replaceState(null, "", "/?window=service-fixture");
  document.body.innerHTML = '<div id="root"></div>';
  const listener = vi.spyOn(window, "addEventListener");
  await import("./main");
  expect(startup.render).toHaveBeenCalledTimes(1);
  expect(startup.initializeLocale).not.toHaveBeenCalled();
  expect(listener.mock.calls.some(([name]) => name === "pagehide")).toBe(false);
  listener.mockRestore();
  history.replaceState(null, "", "/");
});

it("still waits for ordinary locale initialization and disposes its listener on page exit", async () => {
  history.replaceState(null, "", "/");
  document.body.innerHTML = '<div id="root"></div>';
  let finish!: (dispose: () => void) => void;
  startup.initializeLocale.mockReturnValueOnce(new Promise<() => void>((resolve) => { finish = resolve; }));
  const dispose = vi.fn();
  await import("./main");
  expect(startup.initializeLocale).toHaveBeenCalledTimes(1);
  expect(startup.render).not.toHaveBeenCalled();
  finish(dispose);
  await vi.waitFor(() => expect(startup.render).toHaveBeenCalledTimes(1));
  const event = new Event("pagehide");
  Object.defineProperty(event, "persisted", { value: false });
  window.dispatchEvent(event);
  expect(dispose).toHaveBeenCalledTimes(1);
});

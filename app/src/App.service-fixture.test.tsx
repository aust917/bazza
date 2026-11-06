import { render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";

const legacy = vi.hoisted(() => {
  history.replaceState(null, "", "/?window=service-fixture");
  return { getSettings: vi.fn() };
});
vi.mock("@/lib/theme", () => ({ installTheme: vi.fn() }));
vi.mock("@/lib/ipc", () => ({ cmd: legacy }));
vi.mock("@/routes/ServiceFixture", () => ({ ServiceFixture: () => <p>Isolated fixture branch</p> }));
import App from "./App";

it("selects the fixture before settings, onboarding, and the main shell mount", () => {
  render(<App />);
  expect(screen.getByText("Isolated fixture branch")).toBeInTheDocument();
  expect(legacy.getSettings).not.toHaveBeenCalled();
  history.replaceState(null, "", "/");
});

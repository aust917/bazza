import "@testing-library/jest-dom/vitest";
import { beforeEach } from "vitest";
import i18n from "./lib/i18n";

// Existing English regression fixtures are explicit; Russian tests override this.
beforeEach(async () => { await i18n.changeLanguage("en"); });

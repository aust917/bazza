import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import commonEn from "@/locales/en/common";
import commonRu from "@/locales/ru/common";
import recordingEn from "@/locales/en/recording";
import recordingRu from "@/locales/ru/recording";
import mainEn from "@/locales/en/main";
import mainRu from "@/locales/ru/main";
import meetingEn from "@/locales/en/meeting";
import meetingRu from "@/locales/ru/meeting";
import nativeEn from "@/locales/en/native";
import nativeRu from "@/locales/ru/native";
import { cmd, on } from "./ipc";

export type UiLocale = "ru" | "en";
export const resources = {
  en: { common: commonEn, recording: recordingEn, main: mainEn, meeting: meetingEn, native: nativeEn },
  ru: { common: commonRu, recording: recordingRu, main: mainRu, meeting: meetingRu, native: nativeRu },
} as const;

void i18n.use(initReactI18next).init({
  resources, lng: "ru", fallbackLng: "en", supportedLngs: ["ru", "en"],
  defaultNS: "common", initAsync: false, interpolation: { escapeValue: false },
});

export function currentLocale(): UiLocale {
  return i18n.resolvedLanguage === "ru" ? "ru" : "en";
}

let localeRevision = 0;
export function languageRevision() { return localeRevision; }

function updateDocumentLanguage(locale: string) {
  localeRevision++;
  document.documentElement.lang = locale;
}
if (typeof document !== "undefined") {
  updateDocumentLanguage(currentLocale());
  i18n.on("languageChanged", updateDocumentLanguage);
}

export type UiMessage = {
  code?: string; args?: Record<string, string | number>; detail?: string | null; message?: string;
};

function diagnostic(value: unknown): string {
  if (value instanceof Error) return String(value);
  if (typeof value === "string") return value;
  if (value && typeof value === "object") {
    const message = value as UiMessage;
    return message.detail ?? message.message ?? JSON.stringify(value);
  }
  return value == null ? "" : String(value);
}

/** Render stable native message codes; never identify errors by English wording. */
export function displayMessage(value: UiMessage | string): string {
  if (typeof value === "object" && value.code) {
    const [ns, ...path] = value.code.split(".");
    const key = path.join(".");
    const args = { ...value.args };
    if (value.code === "native.speakerMerged") {
      for (const field of ["from", "name"]) {
        const placeholder = typeof args[field] === "string" ? /^Speaker (\d+)$/i.exec(String(args[field])) : null;
        if (placeholder) args[field] = i18n.t("speaker", { number: placeholder[1] });
      }
    }
    // These keys are validated by the native-catalog parity test, not user input.
    if (ns === "native" && i18n.exists(key, { ns, ...args })) {
      const text = String(i18n.t(key as never, { ns, ...args }));
      return value.detail ? `${text} ${value.detail}` : text;
    }
  }
  const detail = diagnostic(value);
  return currentLocale() === "en" ? detail : `${i18n.t("applicationMessage")} ${detail}`;
}

/** Old persisted strings and external diagnostics remain available verbatim. */
export function displayError(error: unknown): string {
  if (error && typeof error === "object" && "code" in error) return displayMessage(error as UiMessage);
  const detail = diagnostic(error);
  return currentLocale() === "en" ? detail : `${i18n.t("operationFailed")} ${detail}`;
}

/** Subscribe before reading so changes during bootstrap cannot be lost. */
export async function connectLocale(source: {
  read: () => Promise<UiLocale>;
  listen: (listener: (locale: UiLocale) => void) => Promise<() => void>;
}, apply: (locale: UiLocale) => void = (locale) => { void i18n.changeLanguage(locale); }) {
  let revision = 0;
  let disposed = false;
  const unlisten = await source.listen((locale) => {
    if (disposed) return;
    revision++;
    apply(locale === "ru" ? "ru" : "en");
  });
  const initialRevision = revision;
  try {
    const locale = await source.read();
    if (revision === initialRevision) apply(locale === "ru" ? "ru" : "en");
  } catch (error) {
    unlisten();
    throw error;
  }
  return () => { disposed = true; unlisten(); };
}

export async function initializeLocale(): Promise<() => void> {
  return connectLocale({
    read: cmd.getUiLocale,
    listen: (listener) => on("ui-locale-changed", ({ locale }) => listener(locale)),
  }).catch(async (error) => {
    // A transient initial-read error must not disconnect this window forever.
    console.error("Interface language read failed; retaining bundled default", error);
    return on("ui-locale-changed", ({ locale }) => { void i18n.changeLanguage(locale); });
  });
}

export default i18n;

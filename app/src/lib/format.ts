import i18n, { currentLocale, type UiLocale } from "./i18n";
import type { VideoSource } from "./ipc";

const intlLocale = (locale: UiLocale) => locale === "ru" ? "ru-RU" : "en-US";
const dateFormats = new Map<string, Intl.DateTimeFormat>();
const timeFormats = new Map<UiLocale, Intl.DateTimeFormat>();

/** 65000 -> "1:05", 3725000 -> "1:02:05". */
export function fmtDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return `${h > 0 ? h + ":" : ""}${mm}:${String(s).padStart(2, "0")}`;
}

/** Timestamp for transcripts: always m:ss or h:mm:ss. */
export const fmtStamp = fmtDuration;

export function fmtDate(iso: string, locale: UiLocale = currentLocale()): string {
  const d = new Date(iso);
  const now = new Date();
  const sameYear = d.getFullYear() === now.getFullYear();
  if (Number.isNaN(d.getTime())) return "Invalid Date";
  const key = `${locale}:${sameYear}`;
  let format = dateFormats.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(intlLocale(locale), { month: "short", day: "numeric", year: sameYear ? undefined : "numeric" });
    dateFormats.set(key, format);
  }
  return format.format(d);
}

export function fmtTime(iso: string, locale: UiLocale = currentLocale()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "Invalid Date";
  let format = timeFormats.get(locale);
  if (!format) {
    format = new Intl.DateTimeFormat(intlLocale(locale), { hour: "numeric", minute: "2-digit" });
    timeFormats.set(locale, format);
  }
  return format.format(d);
}

export function fmtBytes(n: number, locale: UiLocale = currentLocale()): string {
  const unit = (key: "b" | "kb" | "mb" | "gb" | "tb") => i18n.t(`bytes.${key}`, { ns: "common", lng: locale });
  if (n < 1024) return `${n.toLocaleString(intlLocale(locale))} ${unit("b")}`;
  const units = ["kb", "mb", "gb", "tb"] as const;
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const digits = v < 10 ? 1 : 0;
  return `${v.toLocaleString(intlLocale(locale), { minimumFractionDigits: digits, maximumFractionDigits: digits })} ${unit(units[i])}`;
}

/** Normalize dBFS (-100..0) to 0..1 for meters, with a gentle curve. */
export function dbToLevel(db: number): number {
  const clamped = Math.min(0, Math.max(-60, db));
  return Math.pow((clamped + 60) / 60, 1.6);
}

export const APP_LABELS: Record<string, string> = {
  import: "Imported file",
  zoom: "Zoom",
  teams: "Teams",
  meet: "Google Meet",
  webex: "Webex",
  discord: "Discord",
  slack: "Slack",
  facetime: "FaceTime",
  gotomeeting: "GoToMeeting",
  unknown: "Call",
};

export function appLabel(app: string | null | undefined): string {
  if (!app) return i18n.t("app.recording", { ns: "common" });
  if (app === "import" || app === "unknown") return i18n.t(`app.${app}`, { ns: "common" });
  return APP_LABELS[app] ?? app;
}

/** Presentation only: callers must retain the original value for editing/IPC. */
export function speakerLabel(label: string): string {
  const placeholder = /^Speaker (\d+)$/i.exec(label.trim());
  return placeholder ? i18n.t("speaker", { ns: "common", number: placeholder[1] }) : label;
}

export function sourceLabel(source: Pick<VideoSource, "label" | "is_primary">): string {
  return source.is_primary ? i18n.t("primarySource", { ns: "common", label: source.label }) : source.label;
}

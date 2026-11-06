import type { VideoSource } from "./ipc";

export function screenSourceKey(source: VideoSource): string {
  return source.screen_id ?? `m:${source.target.index}`;
}

export function isMeetUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.origin === "https://meet.google.com" && !url.username && !url.password
      && /^\/(?:meet\/)?[a-z]{3}-[a-z]{4}-[a-z]{3}\/?$/.test(url.pathname);
  } catch {
    return false;
  }
}

// Separate experimental GUI bridge. It has no browser mock or legacy IPC fallback.
import { invoke } from "@tauri-apps/api/core";

export interface RepositoryIdentity { library_id: string; library_epoch: string; cursor_scope: string }
export interface Snapshot<T> { identity: RepositoryIdentity; data: T }
export interface Preferences { revision: string; ui_locale: "ru" | "en"; close_to_tray: boolean; hotkey: string }
export interface Bootstrap { preferences: Preferences; change_sequence: number; cursor_scope: string }
export interface MutationOutcome { entity_id: string; revision: string; change_sequence: number; job_id: string | null; dedup_until: string }
export interface PreferencesRequest {
  expected_revision: string; idempotency_key: string;
  patch: { ui_locale?: "ru" | "en"; close_to_tray?: boolean; hotkey?: string };
}
export interface RenameRequest { meeting_id: string; expected_revision: string; idempotency_key: string; title: string }
export interface DeleteMeetingRequest { meeting_id: string; expected_lifecycle_revision: string; idempotency_key: string }
export interface DesktopMeeting {
  id: string; title: string; app: string | null; started_at: string; ended_at: string | null;
  duration_ms: number; has_video: boolean; status: string; folder_id: string | null; created_at: string;
  error: string | null; error_truncated: boolean; title_auto: boolean; participants: string[]; calendar_uid: string | null;
  content_kind: string; metadata_revision: string; lifecycle_revision: string;
  transcript_revision: string; transcript_state: string; summary_revision: string; summary_state: string;
  media: { audio: boolean; video: boolean };
}
export type MeetingFilter = {
  folder: { kind: "all" } | { kind: "unfiled" } | { kind: "subtree"; folder_id: string };
  tag_id: string | null; content_kind: "text" | "recorded" | null;
};
export interface MeetingCursor { library_scope: string; snapshot_sequence: number; query_hash: string; offset: number }
export interface MeetingContinuation { library_id: string; library_epoch: string; cursor: MeetingCursor }
export interface MeetingsRequest { filter: MeetingFilter; cursor: MeetingContinuation | null; limit: number; max_bytes: number }
export interface MeetingPage { identity: RepositoryIdentity; items: DesktopMeeting[]; next: MeetingContinuation | null; snapshot_sequence: number }
export interface Change { sequence: number; entity_kind: string; entity_id: string; revision: string; actor: string }
export interface ChangeContinuation { library_id: string; library_epoch: string; cursor: { library_scope: string; after: number } }
export interface ChangesRequest { cursor: ChangeContinuation | null; limit: number; max_bytes: number }
export interface ChangePage { identity: RepositoryIdentity; items: Change[]; next: ChangeContinuation; has_more: boolean; current_sequence: number }
export type DocumentState = "absent" | "present" | "deleted";
export type Representation = "raw" | "effective";
export type SummaryField = "markdown" | "structured_json";
export interface TranscriptState { meeting_id: string; revision: string; current_source_id: string | null; document_state: DocumentState; deletion_epoch: number }
export interface TranscriptCursor {
  library_scope: string; meeting_id: string; revision: string; current_source_id: string | null; source_id: string | null;
  representation: Representation; segment_index: number; byte_offset: number; query_hash: string;
}
export interface TranscriptContinuation { library_id: string; library_epoch: string; cursor: TranscriptCursor }
export interface TranscriptRequest {
  meeting_id: string; source_id: string | null; expected_revision: string; representation: Representation;
  cursor: TranscriptContinuation | null; limit: number; max_bytes: number;
}
export interface TranscriptFragment {
  segment_index: number; segment_id: number; start_ms: number; end_ms: number; speaker: string | null;
  text: string; byte_offset: number; total_bytes: number; has_clean_text: boolean; clean_text_manual: boolean;
  effective_origin: "raw" | "automatic" | "manual";
}
export interface TranscriptPage {
  identity: RepositoryIdentity; state: TranscriptState; source_id: string | null;
  representation: Representation; items: TranscriptFragment[]; next: TranscriptContinuation | null;
}
export interface SummaryState {
  meeting_id: string; revision: string; document_state: DocumentState; deletion_epoch: number;
  automatic_generation_suppressed: boolean; origin: string; auto_replace: boolean;
  source_label: string | null; source_revision: string | null; source_missing: boolean;
}
export interface SummaryCursor { library_scope: string; meeting_id: string; revision: string; field: SummaryField; byte_offset: number; query_hash: string }
export interface SummaryContinuation { library_id: string; library_epoch: string; cursor: SummaryCursor }
export interface SummaryRequest { meeting_id: string; expected_revision: string; field: SummaryField; cursor: SummaryContinuation | null; max_bytes: number }
export interface SummaryPage {
  identity: RepositoryIdentity; state: SummaryState; field: SummaryField; text: string;
  byte_offset: number; total_bytes: number; next: SummaryContinuation | null;
}
export interface FixtureError { code: string; message: string; retryable: boolean }
export interface FixtureCapture {
  state: "idle" | "starting" | "recording" | "paused" | "finishing" | "pending";
  meeting_id: string | null;
  elapsed_ms: number;
  job_id: string | null;
  screen?: "recording" | "stopped" | "failed";
}

export function fixtureAvailable(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
function call<T>(name: string, request?: unknown): Promise<T> {
  if (!fixtureAvailable()) return Promise.reject<never>({ code: "SERVICE_UNAVAILABLE", message: "The fixture service is unavailable.", retryable: false } satisfies FixtureError);
  return invoke<T>(`service_fixture_${name}`, request === undefined ? undefined : { request });
}
export const serviceFixture = {
  bootstrap: () => call<Snapshot<Bootstrap>>("bootstrap"),
  preferences: () => call<Snapshot<Preferences>>("preferences"),
  reconnect: () => call<RepositoryIdentity>("reconnect"),
  updatePreferences: (request: PreferencesRequest) => call<Snapshot<MutationOutcome>>("update_preferences", request),
  renameMeeting: (request: RenameRequest) => call<Snapshot<MutationOutcome>>("rename_meeting", request),
  deleteMeeting: (request: DeleteMeetingRequest) => call<Snapshot<MutationOutcome>>("delete_meeting", request),
  meeting: (meeting_id: string) => call<Snapshot<DesktopMeeting | null>>("meeting", { meeting_id }),
  meetings: (request: MeetingsRequest) => call<MeetingPage>("meetings", request),
  changes: (request: ChangesRequest) => call<ChangePage>("changes", request),
  transcript: (request: TranscriptRequest) => call<TranscriptPage>("transcript", request),
  summary: (request: SummaryRequest) => call<SummaryPage>("summary", request),
  captureStatus: () => call<FixtureCapture>("capture_status"),
  captureStart: (capture_screen = false, use_configured_audio = false) =>
    call<FixtureCapture>("capture_start", { title: null, capture_screen, use_configured_audio }),
  capturePause: () => call<FixtureCapture>("capture_pause"),
  captureResume: () => call<FixtureCapture>("capture_resume"),
  captureStop: () => call<FixtureCapture>("capture_stop"),
};

export function sameRepository(a: RepositoryIdentity | null, b: RepositoryIdentity): boolean {
  return a !== null && a.library_id === b.library_id && a.library_epoch === b.library_epoch && a.cursor_scope === b.cursor_scope;
}

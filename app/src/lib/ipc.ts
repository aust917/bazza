// Typed bridge to the Rust side. Every command and event lives here so the
// rest of the UI never touches Tauri APIs directly.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isMock, mockInvoke, mockListen } from "./mock";
import type { UiLocale, UiMessage } from "./i18n";

// Outside Tauri (plain `pnpm dev` in a browser) fall back to sample data so the UI can be developed and screenshotted.
const invoke: typeof tauriInvoke = isMock ? (mockInvoke as typeof tauriInvoke) : tauriInvoke;
const listen: typeof tauriListen = isMock ? mockListen as typeof tauriListen : tauriListen;

export type MeetingStatus = "recording" | "processing" | "ready" | "failed";

export type CodexMcpStatus = { enabled: boolean };
export type ClaudeMcpClient = "code" | "desktop";
export type ClaudeMcpStatus = { code_enabled: boolean; desktop_enabled: boolean };

export interface CalEvent {
  uid: string;
  title: string;
  start: string;
  end: string;
  attendees: string[];
  url: string | null;
  location: string | null;
}

export interface ParticipantsPayload {
  meeting_id: string;
  participants: string[];
}

export interface Meeting {
  id: string;
  title: string;
  app: string | null;
  started_at: string;
  ended_at: string | null;
  duration_ms: number;
  has_video: boolean;
  status: MeetingStatus;
  folder_id: string | null;
  created_at: string;
  error: string | null;
  title_auto: boolean;
  participants: string[];
  calendar_uid: string | null;
  metadata_revision?: string;
  lifecycle_revision?: string;
}

export interface Segment {
  id: number;
  meeting_id: string;
  start_ms: number;
  end_ms: number;
  speaker: string | null;
  text: string;
  clean_text: string | null;
  /** Missing on older payloads means false; the original text remains immutable. */
  clean_text_manual?: boolean;
  provenance?: TranscriptProvenance;
}

export interface MeetingSpeaker {
  meeting_id: string;
  label: string;
  speaker_id: string | null;
  suggested_id: string | null;
  suggested_name: string | null;
  suggested_score: number | null;
}

export interface Speaker {
  id: string;
  name: string;
}

export interface VoiceProfile {
  id: string; name: string; model_id: string; sample_count: number; has_voiceprint: boolean;
}
export interface VoiceSample {
  id: string; speaker_id: string; model_id: string; source: "legacy" | "meeting" | "file";
  source_label: string; meeting_id: string | null; start_ms: number | null; end_ms: number | null;
  has_audio: boolean; created_at: string;
}
export type VoiceSampleSource =
  | { kind: "file"; path: string; start_ms: number; end_ms: number }
  | { kind: "meeting"; meeting_id: string; start_ms: number; end_ms: number };

export interface SpeakerStat {
  label: string;
  segments: number;
  /** Total speaking time in milliseconds. */
  ms: number;
  first_ms: number;
}

export interface RenameOutcome {
  speaker: Speaker | null;
  /** The label every affected segment now carries. */
  name: string;
  /** Set when this rename merged two labels into one person. */
  merged_from: string | null;
  moved_segments: number;
}

export interface MeetingDetail {
  meeting: Meeting;
  segments: Segment[];
  media: { audio: string | null; video: string | null; lease?: string };
  highlights: number[];
  speakers: MeetingSpeaker[];
  speaker_stats: SpeakerStat[];
}

export interface SearchHit {
  meeting_id: string;
  meeting_title: string;
  segment_id: number;
  start_ms: number;
  snippet: string;
}

export type MonitorTarget = { kind: "monitor"; index: number };
export type WindowTarget = { kind: "window"; title: string; identity?: { hwnd: string; pid: number } };
export type VideoTarget = MonitorTarget | WindowTarget;
export type Tier = "cpu_low" | "cpu_high" | "gpu";
export type SttProvider = "local" | "soniox";

export interface VideoSource {
  is_primary?: boolean;
  screen_id?: string | null;
  target: MonitorTarget;
  label: string;
}

export interface Settings {
  service_revision?: string;
  onboarded: boolean;
  user_name: string;
  language: string | null;
  mic_device: string | null;
  loopback_device: string | null;
  capture_system: boolean;
  video_enabled: boolean;
  video_target: VideoTarget;
  video_fps: number;
  video_max_height: number;
  tier_override: Tier | null;
  live_asr_model: string | null;
  quality_asr_model: string | null;
  stt_provider: SttProvider;
  soniox_model: string;
  soniox_consent: boolean;
  llm_model: string | null;
  hotkey: string;
  detection_enabled: boolean;
  audio_activity_enabled: boolean;
  never_apps: string[];
  popup_timeout_secs: number;
  close_to_tray: boolean;
  llm_backend: "bundled" | "openai";
  llm_endpoint: string;
  llm_endpoint_model: string;
  llm_api_key: string | null;
  cleanup_enabled: boolean;
  diarize_enabled: boolean;
  summary_enabled: boolean;
  default_template_id: string;
  share_port: number;
  calendar_sources: string[];
  calendar_refresh_min: number;
  webhook_url: string;
  webhook_enabled: boolean;
  auto_update_check: boolean;
}

export interface Template {
  id: string;
  name: string;
  description: string;
  body: string;
  builtin: boolean;
}

export interface Summary {
  meeting_id: string;
  template_id: string;
  markdown: string;
  structured: {
    summary: string;
    action_items: string[];
    decisions: string[];
    open_questions: string[];
    key_moments: { ms: number | null; text: string }[];
    sections: Record<string, string>;
    generation?: { agent: SummaryAgent; model: string; template_id: string };
  } | null;
  model: string;
  created_at: string;
}

export type SummaryAgent = "codex" | "claude";
export type SubscriptionStatus = "ready" | "missing" | "sign_in_required" | "check_failed";
export interface SubscriptionAvailability { agent: SummaryAgent; status: SubscriptionStatus }
export interface SubscriptionReply { markdown: string; model: string }
export interface SubscriptionJob {
  request_id: string;
  agent: SummaryAgent;
  status: "running" | "cancelling" | "succeeded" | "failed" | "cancelled";
  reply: SubscriptionReply | null;
  error: UiMessage | null;
  started_at_ms?: number;
  finished_at_ms?: number | null;
}
export interface ServiceSummaryPreview {
  preview_id: string;
  prompt: string;
}
export interface ServiceSummaryJob {
  preview_id: string;
  agent: SummaryAgent;
  status: "prepared" | "running" | "cancelling" | "saving" | "succeeded" | "failed" | "cancelled";
  prompt: string | null;
  error: UiMessage | null;
  started_at_ms?: number | null;
  finished_at_ms?: number | null;
}
export interface SummaryJobStatus { running: boolean; agent: SummaryAgent | null }

export interface Chat {
  id: string;
  scope_kind: "meeting" | "all";
  scope_id: string | null;
  title: string;
  created_at: string;
}

export interface ChatMessage {
  id: string;
  chat_id: string;
  role: "user" | "assistant" | string;
  content: string;
  citations: unknown;
  created_at: string;
}

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  default_template_id: string | null;
  meeting_count: number;
  revision?: string;
}

export interface Tag {
  id: string;
  name: string;
  meeting_count: number;
}

export type ServiceFolderMutation =
  | { op: "folder_create"; name: string; parent_id: string | null }
  | { op: "folder_rename"; folder_id: string; name: string; expected_revision: string }
  | { op: "folder_delete"; folder_id: string; expected_revision: string };

export type ServiceLibraryMutation = (ServiceFolderMutation | {
  op: "meeting_move"; meeting_id: string; folder_id: string | null; expected_revision: string;
}) & { idempotency_key: string };

export interface MeetingFilter {
  /** null = all; [null] = unfiled; [id] = folder subtree */
  folder: null | [string | null];
  tag_id: string | null;
}

export interface Share {
  token: string;
  meeting_id: string;
  meeting_title: string;
  kind: "meeting" | "clip";
  start_ms: number | null;
  end_ms: number | null;
  enabled: boolean;
  created_at: string;
}

export interface ShareInfo {
  share: Share;
  url: string;
}

export interface ImportReport {
  imported: number;
  skipped_existing: number;
  errors: string[];
}

export interface AudioDevice {
  id: string;
  name: string;
  is_default: boolean;
}

export interface Hardware {
  cpu_cores: number;
  cpu_name: string;
  ram_gb: number;
  gpu: { name: string; vram_gb: number; backend: "cuda" | "metal" } | null;
  os: string;
  arch: string;
}

export interface ModelSpec {
  id: string;
  kind: "asr" | "llm" | "embedding" | "diarize";
  name: string;
  file: string;
  url: string;
  sha256: string;
  size_bytes: number;
  note: string;
}

export interface ModelRow {
  spec: ModelSpec;
  present: boolean;
  downloading: boolean;
  roles: ("live" | "quality" | "llm" | "speakers" | "search")[];
}

export interface RecordingState {
  state: "idle" | "starting" | "recording" | "paused" | "finishing" | "pending";
  meeting_id: string | null;
  elapsed_ms: number;
}

export interface DiscordBot {
  id: string;
  name: string;
}

export interface DiscordGuild {
  id: string;
  name: string;
}

export interface DiscordRecordingSelection {
  guild_id: string;
  channel_id: string;
}

export interface DiscordChannel extends DiscordRecordingSelection {
  guild_name: string;
  channel_name: string;
}

export interface DiscordSettingsStatus {
  bot: DiscordBot | null;
  channel: DiscordChannel | null;
}

export interface DiscordRecordingStatus {
  meeting_id: string;
  state: "connecting" | "connected" | "receiving" | "unavailable" | "paused";
}

export interface AttendeeSettings {
  apiUrl: string;
  callbackUrl: string;
  bindAddress: string;
  certificatePath: string;
  privateKeyPath: string;
}

export type AttendeeSettingsStatus = { [K in keyof AttendeeSettings]: string | null } & {
  configured: boolean;
  managed?: boolean;
  setupAvailable?: boolean;
};

export interface AttendeeSetupProgress {
  phase: "preparing" | "enablingWsl" | "downloading" | "installing" | "starting" | "checking";
  message: string | null;
}

export interface AttendeeRecordingStatus {
  meetingId: string;
  phase: "connecting" | "waiting" | "waiting_admission" | "connected" | "receiving" | "failed" | "finished";
  message: string | null;
}

export interface AttendeeTrack {
  id: number;
  name: string | null;
}

export type BotTranscriptProvider = "meet" | "discord";

export interface TranscriptVersion {
  id: string;
  source: "live" | "recording" | BotTranscriptProvider | "existing" | "combined";
  model: string | null;
  language: string | null;
  created_at: string;
  partial?: boolean;
}

export interface TranscriptVersions {
  versions: TranscriptVersion[];
  initial_id: string | null;
  participant_source: BotTranscriptProvider | null;
  effective_id?: string | null;
}

export interface TranscriptProvenance {
  source: "bot" | "system" | "microphone";
  provider?: BotTranscriptProvider | null;
  participant_id?: string | null;
  source_start_ms: number;
  source_end_ms: number;
  uncertain: boolean;
}

export interface TranscriptPlaybackTiming {
  start_ms: number;
  end_ms: number;
}

export interface TranscriptVersionSegment extends Segment {
  /** Present only for a participant segment verified against the recording. */
  playback?: TranscriptPlaybackTiming;
}

export interface TranscriptVersionOptions {
  source: "recording" | "participants" | "combined";
  model?: string;
  language?: string;
}

export interface TranscriptVersionProgress {
  meeting_id: string;
  phase: "queued" | "reading" | "loading" | "recognizing" | "aligning" | "saving";
  progress: number | null;
}

export interface TranscriptVersionJob {
  id: string;
  phase: TranscriptVersionProgress["phase"] | "cancelling" | "cancelled" | "succeeded" | "failed";
  progress: number | null;
  elapsed_ms: number;
  can_cancel: boolean;
  version: TranscriptVersion | null;
  error: unknown;
}

export type RecordingBotTarget =
  | { provider: "meet"; meetingUrl: string }
  | ({ provider: "discord" } & DiscordRecordingSelection);

export interface RecordingBotConnectionStatus {
  requestId: string;
  revision: number;
  phase: "connecting" | "waiting_admission" | "connected" | "failed" | "disconnected";
  message?: string | UiMessage | null;
}

export interface BotTranscriptSegment {
  speaker_id: string;
  speaker_name: string | null;
  start_ms: number;
  end_ms: number;
  playback_start_ms?: number | null;
  playback_end_ms?: number | null;
  text: string;
}

export interface BotTranscript {
  timing: "approximate" | "relative";
  partial: boolean;
  segments: BotTranscriptSegment[];
}

export interface BotTranscriptSource {
  source: BotTranscriptProvider;
  track_count: number;
  transcript: BotTranscript | null;
}

export interface BotTranscriptJob {
  id: string;
  source: BotTranscriptProvider;
  stage: "queued" | "reading" | "loading" | "transcribing" | "aligning" | "saving" | "cancelling" | "cancelled" | "done" | "failed";
  elapsed_ms: number;
  completed: number | null;
  total: number | null;
  model: string | null;
  backend: string | null;
  backend_reason: string | null;
  can_cancel: boolean;
  error: UiMessage | null;
}

export interface TranscriptionProgress {
  phase: "reading" | "loading" | "decoding";
  progress: number | null;
  model: string | null;
}

export interface StartOptions {
  title?: string;
  app?: string;
  video?: boolean;
  target?: MonitorTarget;
  screen_id?: string;
  discord?: DiscordRecordingSelection;
  attendee?: { meetingUrl: string };
  bot_connection_id?: string;
}

export const cmd = {
  listMeetings: () => invoke<Meeting[]>("list_meetings"),
  getMeeting: (id: string) => invoke<MeetingDetail>("get_meeting", { id }),
  releaseMeetingMedia: (lease: string) => invoke<void>("release_meeting_media", { lease }),
  renameMeeting: (id: string, title: string, expectedMetadataRevision?: string, idempotencyKey?: string) =>
    invoke<Meeting>("rename_meeting", expectedMetadataRevision ? { id, title, expectedMetadataRevision, idempotencyKey } : { id, title }),
  deleteMeeting: (id: string, expectedLifecycleRevision?: string, idempotencyKey?: string) =>
    invoke<void>("delete_meeting", expectedLifecycleRevision ? { id, expectedLifecycleRevision, idempotencyKey } : { id }),
  search: (query: string, limit = 50) => invoke<SearchHit[]>("search", { query, limit }),
  testWebhook: (url: string) => invoke<number>("test_webhook", { url }),
  listVideoSources: () => invoke<VideoSource[]>("list_video_sources").then((sources) =>
    sources.filter((source) => source.target.kind === "monitor")),
  listUpcoming: (hours?: number) => invoke<CalEvent[]>("list_upcoming", { hours }),
  refreshCalendar: () => invoke<string[]>("refresh_calendar"),
  retranscribe: (id: string, model?: string | null) => invoke<void>("retranscribe", { id, model: model ?? null }),
  transcriptVersions: (id: string) => invoke<TranscriptVersions>("transcript_versions", { id }),
  transcriptVersion: (id: string, versionId: string) => invoke<TranscriptVersionSegment[]>("transcript_version", { id, versionId }),
  regenerateTranscript: (id: string, options: TranscriptVersionOptions) => invoke<TranscriptVersion>("regenerate_transcript", { id, options }),
  transcriptVersionJob: (id: string) => invoke<TranscriptVersionJob | null>("transcript_version_job", { id }),
  cancelTranscriptVersion: (id: string, jobId: string) => invoke<boolean>("cancel_transcript_version", { id, jobId }),
  setTranscriptCorrection: (id: string, segmentId: number, text: string | null) => invoke<Segment>("set_transcript_correction", { id, segmentId, text }),
  rediarize: (id: string) => invoke<void>("rediarize", { id }),
  reembed: (id: string) => invoke<void>("reembed", { id }),
  importRecording: (path: string, title?: string, transcriptPath?: string) => invoke<Meeting>("import_recording", { path, title: title ?? null, transcriptPath: transcriptPath ?? null }),
  saveTextFile: (path: string, text: string) => invoke<void>("save_text_file", { path, text }),

  startRecording: (opts?: StartOptions) => invoke<Meeting>("start_recording", { opts }),
  connectRecordingBot: (requestId: string, target: RecordingBotTarget) =>
    invoke<RecordingBotConnectionStatus>("connect_recording_bot", { requestId, target }),
  disconnectRecordingBot: (requestId: string) => invoke<void>("disconnect_recording_bot", { requestId }),
  stopRecording: () => invoke<Meeting>("stop_recording"),
  pauseRecording: () => invoke<void>("pause_recording"),
  resumeRecording: () => invoke<void>("resume_recording"),
  markHighlight: () => invoke<number>("mark_highlight"),
  recordingStatus: () => invoke<RecordingState>("recording_status"),
  discordRecordingStatus: () => invoke<DiscordRecordingStatus | null>("discord_recording_status"),
  attendeeRecordingStatus: () => invoke<AttendeeRecordingStatus | null>("attendee_recording_status"),
  attendeeTracks: (meetingId: string) => invoke<AttendeeTrack[]>("attendee_tracks", { meetingId }),
  attendeeOpenTrack: (meetingId: string, trackId: number) => invoke<void>("attendee_open_track", { meetingId, trackId }),
  botTranscriptSources: (meetingId: string) => invoke<BotTranscriptSource[]>("bot_transcript_sources", { meetingId }),
  botTranscriptJobs: (meetingId: string) => invoke<BotTranscriptJob[]>("bot_transcript_jobs", { meetingId }),
  cancelBotTranscript: (meetingId: string, source: BotTranscriptProvider, jobId: string) =>
    invoke<boolean>("cancel_bot_transcript", { meetingId, source, jobId }),
  transcribeBotTracks: (meetingId: string, source: BotTranscriptProvider) =>
    invoke<BotTranscript>("transcribe_bot_tracks", { meetingId, source }),
  liveSnapshot: () => invoke<LiveSnapshot | null>("live_snapshot"),
  openRecordPicker: () => invoke<void>("open_record_picker"),
  dismissDetection: (appId: string, mode: "now" | "never" | "snooze") =>
    invoke<void>("dismiss_detection", { appId, mode }),
  openMain: () => invoke<void>("open_main"),

  getSettings: () => invoke<Settings>("get_settings"),
  getUiLocale: () => invoke<UiLocale>("get_ui_locale"),
  setUiLocale: (locale: UiLocale) => invoke<UiLocale>("set_ui_locale", { locale }),
  setSettings: ({ service_revision, ...settings }: Settings) => invoke<Settings>("set_settings", service_revision
    ? { new: settings, expectedRevision: service_revision } : { new: settings }),
  getCodexMcpStatus: () => invoke<CodexMcpStatus | null>("get_codex_mcp_status"),
  setCodexMcpEnabled: (enabled: boolean) => invoke<CodexMcpStatus>("set_codex_mcp_enabled", { enabled }),
  getClaudeMcpStatus: () => invoke<ClaudeMcpStatus | null>("get_claude_mcp_status"),
  setClaudeMcpEnabled: (client: ClaudeMcpClient, enabled: boolean) =>
    invoke<ClaudeMcpStatus>("set_claude_mcp_enabled", { client, enabled }),
  sonioxKeyStatus: () => invoke<boolean>("soniox_key_status"),
  saveSonioxKey: (key: string | null) => invoke<boolean>("save_soniox_key", { key }),
  discordStatus: () => invoke<DiscordSettingsStatus>("discord_status"),
  discordOpenDeveloperPortal: () => invoke<void>("discord_open_developer_portal"),
  discordSaveToken: (token: string) => invoke<DiscordSettingsStatus>("discord_save_token", { token }),
  discordDisconnect: () => invoke<DiscordSettingsStatus>("discord_disconnect"),
  discordGuilds: () => invoke<DiscordGuild[]>("discord_guilds"),
  discordChannels: (guildId: string) => invoke<DiscordChannel[]>("discord_channels", { guildId }),
  discordSetChannel: (input: string) => invoke<DiscordChannel>("discord_set_channel", { input }),
  discordInviteUrl: (guildId: string | null) => invoke<string>("discord_invite_url", { guildId }),
  attendeeStatus: () => invoke<AttendeeSettingsStatus>("attendee_status"),
  attendeeSetup: () => invoke<AttendeeSettingsStatus>("attendee_setup"),
  attendeeConfigure: (settings: AttendeeSettings, apiKey: string | null) =>
    invoke<AttendeeSettingsStatus>("attendee_configure", { settings, apiKey }),
  attendeeDisconnect: () => invoke<AttendeeSettingsStatus>("attendee_disconnect"),
  sttTranscript: (id: string) => invoke<SttTranscript | null>("stt_transcript", { id }),
  dataInfo: () => invoke<{ data_dir: string; recordings_bytes: number; models_bytes: number }>("data_info"),

  probeHardware: () => invoke<{ hardware: Hardware; tier: Tier; effective_tier: Tier }>("probe_hardware"),
  listModels: () => invoke<ModelRow[]>("list_models"),
  downloadModel: (id: string) => invoke<void>("download_model", { id }),
  cancelDownload: (id: string) => invoke<void>("cancel_download", { id }),
  removeModel: (id: string) => invoke<void>("remove_model", { id }),

  meetingSpeakers: (id: string) => invoke<MeetingSpeaker[]>("meeting_speakers", { id }),
  renameSpeaker: (id: string, label: string, name: string) =>
    invoke<RenameOutcome>("rename_speaker", { id, label, name }),
  acceptSpeakerSuggestion: (id: string, label: string) => invoke<Speaker>("accept_speaker_suggestion", { id, label }),
  listKnownSpeakers: () => invoke<Speaker[]>("list_known_speakers"),
  deleteKnownSpeaker: (id: string) => invoke<void>("delete_known_speaker", { id }),
  listVoiceProfiles: () => invoke<VoiceProfile[]>("list_voice_profiles"),
  createVoiceProfile: (name: string) => invoke<VoiceProfile>("create_voice_profile", { name }),
  renameVoiceProfile: (id: string, name: string) => invoke<VoiceProfile>("rename_voice_profile", { id, name }),
  listVoiceSamples: (speakerId: string) => invoke<VoiceSample[]>("list_voice_samples", { speakerId }),
  addVoiceSample: (speakerId: string, source: VoiceSampleSource) => invoke<VoiceSample>("add_voice_sample", { speakerId, source }),
  deleteVoiceSample: (id: string) => invoke<void>("delete_voice_sample", { id }),
  recomputeVoiceProfile: (id: string) => invoke<VoiceProfile>("recompute_voice_profile", { id }),
  voiceSampleAudio: (id: string) => invoke<string>("voice_sample_audio", { id }),
  rerunCleanup: (id: string) => invoke<void>("rerun_cleanup", { id }),
  testLlmEndpoint: (url: string, apiKey: string | null, model: string) =>
    invoke<string[]>("test_llm_endpoint", { url, apiKey, model }),

  listTemplates: () => invoke<Template[]>("list_templates"),
  saveTemplate: (template: Template) => invoke<Template>("save_template", { template }),
  deleteTemplate: (id: string) => invoke<void>("delete_template", { id }),
  getSummary: (id: string) => invoke<Summary | null>("get_summary", { id }),
  getServiceSummary: (id: string) => invoke<{ markdown: string } | null>("get_service_summary", { id }),
  prepareServiceSummary: (id: string, agent: SummaryAgent, segmentIds: number[]) =>
    invoke<ServiceSummaryPreview>("prepare_service_summary", { id, agent, segmentIds }),
  generateServiceSummary: (previewId: string) =>
    invoke<ServiceSummaryJob>("generate_service_summary", { previewId }),
  getServiceSummaryJob: (id: string) => invoke<ServiceSummaryJob | null>("get_service_summary_job", { id }),
  cancelServiceSummary: (previewId: string) =>
    invoke<ServiceSummaryJob | null>("cancel_service_summary", { previewId }),
  generateSummary: (id: string, templateId?: string, agent?: SummaryAgent) => invoke<void>("generate_summary", { id, templateId, agent: agent ?? null }),
  subscriptionAgents: () => invoke<SubscriptionAvailability[]>("subscription_agents"),
  getSubscriptionJob: (chatKey: string) => invoke<SubscriptionJob | null>("get_subscription_job", { chatKey }),
  startSubscription: (chatKey: string, agent: SummaryAgent, question: string, context: string | null) =>
    invoke<SubscriptionJob>("start_subscription", { chatKey, agent, question, context }),
  cancelSubscription: (chatKey: string, requestId: string) =>
    invoke<SubscriptionJob | null>("cancel_subscription", { chatKey, requestId }),
  summaryJob: (id: string) => invoke<SummaryJobStatus>("summary_job", { id }),
  cancelSummary: (id: string) => invoke<void>("cancel_summary", { id }),
  draftFollowup: (id: string) => invoke<string>("draft_followup", { id }),
  listChats: (scopeKind: "meeting" | "all", scopeId: string | null) => invoke<Chat[]>("list_chats", { scopeKind, scopeId }),
  createChat: (scopeKind: "meeting" | "all", scopeId: string | null, title?: string) => invoke<Chat>("create_chat", { scopeKind, scopeId, title }),
  deleteChat: (id: string) => invoke<void>("delete_chat", { id }),
  chatMessages: (chatId: string) => invoke<ChatMessage[]>("chat_messages", { chatId }),
  sendChat: (chatId: string, text: string) => invoke<ChatMessage>("send_chat", { chatId, text }),
  cancelChat: (chatId: string) => invoke<void>("cancel_chat", { chatId }),

  listFolders: () => invoke<Folder[]>("list_folders"),
  serviceLibraryOrganization: () => invoke<{ folders: Folder[]; tags: Tag[] }>("get_service_library_organization"),
  mutateServiceLibraryOrganization: (request: ServiceLibraryMutation) => invoke<void>("mutate_service_library_organization", { request }),
  createFolder: (name: string, parentId: string | null) => invoke<Folder>("create_folder", { name, parentId }),
  updateFolder: (id: string, name: string, parentId: string | null, defaultTemplateId: string | null) =>
    invoke<void>("update_folder", { id, name, parentId, defaultTemplateId }),
  deleteFolder: (id: string) => invoke<void>("delete_folder", { id }),
  moveMeeting: (id: string, folderId: string | null) => invoke<void>("move_meeting", { id, folderId }),
  listMeetingsFiltered: (filter: { folder: null | (string | null)[]; tag_id: string | null }) =>
    invoke<Meeting[]>("list_meetings_filtered", { filter }),
  listTags: () => invoke<Tag[]>("list_tags"),
  meetingTags: (id: string) => invoke<Tag[]>("meeting_tags", { id }),
  tagMeeting: (id: string, name: string) => invoke<Tag>("tag_meeting", { id, name }),
  untagMeeting: (id: string, tagId: string) => invoke<void>("untag_meeting", { id, tagId }),
  exportClip: (id: string, startMs: number, endMs: number, out: string) => invoke<string>("export_clip", { id, startMs, endMs, out }),
  createShare: (id: string, startMs?: number, endMs?: number) => invoke<ShareInfo>("create_share", { id, startMs, endMs }),
  listShares: () => invoke<ShareInfo[]>("list_shares"),
  setShareEnabled: (token: string, enabled: boolean) => invoke<void>("set_share_enabled", { token, enabled }),
  deleteShare: (token: string) => invoke<void>("delete_share", { token }),
  exportMeetings: (ids: string[], out: string, includeVideo: boolean) => invoke<number>("export_meetings", { ids, out, includeVideo }),
  importMeetings: (path: string) => invoke<ImportReport>("import_meetings", { path }),
  exportHtml: (id: string, outDir: string) => invoke<string>("export_html", { id, outDir }),
  changeDataDir: (newDir: string) => invoke<string>("change_data_dir", { newDir }),

  listAudioDevices: () => invoke<{ inputs: AudioDevice[]; outputs: AudioDevice[] }>("list_audio_devices"),
  sampleLevels: (mic: string | null, loopback: string | null) =>
    invoke<[number, number]>("sample_levels", { mic, loopback }),
};

export interface Caption {
  start_ms: number;
  end_ms: number;
  text: string;
  is_final: boolean;
  meeting_id?: string;
  speaker_id?: string;
  speaker_name?: string;
}

export interface SttTranscript {
  provider: SttProvider;
  model: string;
  language: string | null;
  status: "streaming" | "complete" | "incomplete";
  error: string | null;
  captions: Caption[];
  /** Display labels supplied by the live source, when saved with the original transcript. */
  live_speaker_labels?: (string | null)[] | null;
}

/** Captions and running notes of the recording in progress. */
export interface LiveSnapshot {
  meeting_id: string;
  finals: Caption[];
  partial: Caption | null;
  /** In-flight participant captions, separate from the legacy unnamed partial. */
  partials?: Caption[];
  notes: string;
  notes_updated_ms: number | null;
  caption_unavailable?: "model_missing" | "model_load_failed";
}

export interface Events {
  "tauri://drag-drop": { paths: string[]; position: { x: number; y: number } };
  "ui-locale-changed": { locale: UiLocale };
  detection: { app: string; label: string; title: string; confidence: number; event: string | null };
  levels: { mic_db: number; sys_db: number };
  caption: Caption;
  transcript_version_progress: TranscriptVersionProgress;
  live_notes: { meeting_id: string; notes: string; updated_ms: number };
  recording_state: RecordingState;
  recording_bot_connection_status: RecordingBotConnectionStatus;
  discord_recording_status: DiscordRecordingStatus;
  attendee_recording_status: AttendeeRecordingStatus;
  attendee_setup_progress: AttendeeSetupProgress;
  model_progress: { id: string; done: number; total: number; status: "downloading" | "done" | "failed"; error: string | null };
  processing: { meeting_id: string; stage: string; progress: number; error: string | null; transcription?: TranscriptionProgress };
  notice: { level: "info" | "warning" | "error"; message: string } & UiMessage;
  chat_token: { chat_id: string; message_id: string; delta: string };
  chat_done: { chat_id: string; message: ChatMessage; error: string | null };
  participants: ParticipantsPayload;
}

export function on<K extends keyof Events>(name: K, handler: (payload: Events[K]) => void): Promise<UnlistenFn> {
  if (name === "tauri://drag-drop" && !isMock) {
    return getCurrentWebview().listen<Events[K]>(name, (e) => handler(e.payload));
  }
  return listen<Events[K]>(name, (e) => handler(e.payload));
}

/** Subscribe inside a React effect; returns a cleanup that unsubscribes. */
export function subscribe<K extends keyof Events>(name: K, handler: (payload: Events[K]) => void): () => void {
  let un: UnlistenFn | null = null;
  let cancelled = false;
  on(name, handler).then((f) => {
    if (cancelled) f();
    else un = f;
  });
  return () => {
    cancelled = true;
    un?.();
  };
}

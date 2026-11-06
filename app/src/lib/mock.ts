// Dev-only stand-in for the Rust backend so the UI can run in a plain browser
// (`pnpm dev` outside Tauri). Enabled automatically when Tauri is absent.
import recordingEn from "@/locales/en/recording";
import recordingRu from "@/locales/ru/recording";
import type { AttendeeSettings, AttendeeSettingsStatus, BotTranscript, BotTranscriptSource, ChatMessage, DiscordChannel, DiscordSettingsStatus, Folder, Meeting, MeetingDetail, RecordingBotConnectionStatus, RecordingBotTarget, ServiceLibraryMutation, Settings, Summary, Tag, Template, TranscriptVersion, TranscriptVersionOptions, TranscriptVersionSegment, VideoSource, VoiceProfile } from "./ipc";

const now = Date.now();
const voiceProfiles: VoiceProfile[] = [];
const mockListeners = new Map<string, Set<(event: { event: string; id: number; payload: unknown }) => void>>();
const mockVersions = new Map<string, { version: TranscriptVersion; segments: TranscriptVersionSegment[] }[]>();
const localeKey = "bazza.dev.ui_locale";
function mockLocale(): "ru" | "en" {
  const saved = localStorage.getItem(localeKey);
  return saved === null || saved === "ru" ? "ru" : "en";
}
export function emitMockEvent(event: string, payload: unknown) {
  mockListeners.get(event)?.forEach((handler) => handler({ event, id: 0, payload }));
}
export async function mockListen(event: string, handler: (event: { event: string; id: number; payload: unknown }) => void) {
  const handlers = mockListeners.get(event) ?? new Set();
  handlers.add(handler);
  mockListeners.set(event, handlers);
  return () => { handlers.delete(handler); };
}
if (typeof window !== "undefined") {
  window.addEventListener("storage", (event) => {
    if (event.key === localeKey) emitMockEvent("ui-locale-changed", { locale: mockLocale() });
  });
}
const iso = (minsAgo: number) => new Date(now - minsAgo * 60_000).toISOString();

const meetings: Meeting[] = [
  { id: "m1", title: "Q4 launch planning", app: "zoom", started_at: iso(95), ended_at: iso(50), duration_ms: 45 * 60_000, has_video: true, status: "ready", folder_id: "f1", created_at: iso(95), error: null, title_auto: false, participants: ["Sarah Chen", "Marcus Webb", "Priya Patel"], calendar_uid: null },
  { id: "m2", title: "Priya 1:1", app: "meet", started_at: iso(24 * 60), ended_at: iso(24 * 60 - 28), duration_ms: 28 * 60_000, has_video: false, status: "ready", folder_id: null, created_at: iso(24 * 60), error: null, title_auto: true, participants: ["Priya Patel"], calendar_uid: null },
  { id: "m3", title: "Acme discovery call", app: "teams", started_at: iso(3 * 24 * 60), ended_at: iso(3 * 24 * 60 - 41), duration_ms: 41 * 60_000, has_video: true, status: "ready", folder_id: "f2", created_at: iso(3 * 24 * 60), error: null, title_auto: true, participants: [], calendar_uid: null },
  { id: "m4", title: "Standup", app: "discord", started_at: iso(4 * 24 * 60), ended_at: null, duration_ms: 12 * 60_000, has_video: false, status: "processing", folder_id: null, created_at: iso(4 * 24 * 60), error: null, title_auto: true, participants: [], calendar_uid: null },
];

const lines: [number, string, string, string | null][] = [
  [0, "Dallen", "Okay so the main thing today is the launch date for the new site.", null],
  [12_000, "Priya", "right um design is done but the the checkout integration slipped a week i think march third is realistic", "Right. Design is done, but the checkout integration slipped a week. I think March 3rd is realistic."],
  [30_000, "Dallen", "Fine, March 3rd it is. Can you own the Stripe piece and have it in staging by Friday?", null],
  [41_000, "Priya", "yes one question though do we still want the annual plan at launch or push it", "Yes. One question though: do we still want the annual plan at launch, or push it?"],
  [55_000, "Dallen", "Push it. Monthly only for launch. I'll tell Sam to update the pricing page.", null],
  [68_000, "Priya", "got it also the budget we're at 42k of the 50k so we have room for the QA contractor", "Got it. Also, the budget: we're at 42k of the 50k, so we have room for the QA contractor."],
  [80_000, "Dallen", "Approved, bring them in for two weeks.", null],
  [92_000, "Sam", "um I can have the pricing page done by Wednesday if the copy is final", "I can have the pricing page done by Wednesday if the copy is final."],
];

const detail: MeetingDetail = {
  meeting: meetings[0],
  segments: lines.map(([start_ms, speaker, text, clean_text], i) => ({ id: i + 1, meeting_id: "m1", start_ms, end_ms: start_ms + 9000, speaker, text, clean_text })),
  media: { audio: "", video: null },
  highlights: [30_000, 80_000],
  speakers: [
    { meeting_id: "m1", label: "Sam", speaker_id: null, suggested_id: "s1", suggested_name: "Sam Ortiz", suggested_score: 0.83 },
    { meeting_id: "m1", label: "Speaker 3", speaker_id: null, suggested_id: null, suggested_name: null, suggested_score: null },
  ],
  speaker_stats: [
    { label: "Sam", segments: 14, ms: 5 * 60_000 + 12_000, first_ms: 0 },
    { label: "Priya", segments: 9, ms: 3 * 60_000 + 40_000, first_ms: 18_000 },
    { label: "Speaker 3", segments: 4, ms: 52_000, first_ms: 96_000 },
  ],
};

const meetingDetails = new Map(meetings.map((meeting, index) => [meeting.id, {
  ...detail,
  meeting,
  segments: detail.segments.map((segment) => ({ ...segment, id: segment.id + index * 1000, meeting_id: meeting.id, clean_text_manual: false })),
  speakers: detail.speakers.map((speaker) => ({ ...speaker, meeting_id: meeting.id })),
}]));

// Explicit route fixture; keep the ordinary recorded sample library unchanged.
meetingDetails.set("text-only", {
  ...detail,
  meeting: { ...meetings[0], id: "text-only", title: "Text-only meeting", app: null, has_video: false, participants: [],
    duration_ms: 1000, ended_at: new Date(Date.parse(meetings[0].started_at) + 1000).toISOString() },
  segments: [{ id: 1, meeting_id: "text-only", start_ms: 0, end_ms: 1000, speaker: null,
    text: "Text-only fixture transcript.", clean_text: null, clean_text_manual: false }],
  media: { audio: null, video: null },
  highlights: [], speakers: [], speaker_stats: [],
});

const summary: Summary = {
  meeting_id: "m1",
  template_id: "general",
  model: "Qwen3-8B-Q4_K_M",
  created_at: iso(48),
  structured: null,
  markdown: `## Summary
The team fixed the launch date for the new site at March 3rd after the checkout integration slipped a week. Priya owns the Stripe integration, due in staging by Friday. The annual plan is deferred; launch is monthly-only. A QA contractor was approved for two weeks within the remaining budget.

## Decisions
- Launch date set to March 3rd.
- Monthly plan only at launch; annual plan deferred.
- QA contractor approved for two weeks.

## Action items
- [ ] Stripe integration in staging - Priya (Friday)
- [ ] Update pricing page to monthly-only - Sam (Wednesday)
- [ ] Bring in QA contractor - Dallen

## Open questions
- Final copy for the pricing page.

## Key moments
- [0:12] Priya flags the checkout slip and proposes March 3rd
- [0:55] Decision to launch monthly-only
- [1:08] Budget check: 42k of 50k used`,
};

const settings: Settings = {
  onboarded: true, user_name: "Dallen", language: null, mic_device: null, loopback_device: null, capture_system: true, video_enabled: true,
  video_target: { kind: "monitor", index: 0 }, video_fps: 15, video_max_height: 1080, tier_override: null, live_asr_model: null, quality_asr_model: null, llm_model: null,
  stt_provider: "local", soniox_model: "stt-rt-v5", soniox_consent: false,
  hotkey: "CmdOrCtrl+Shift+R", detection_enabled: true, audio_activity_enabled: true, never_apps: [], popup_timeout_secs: 30, close_to_tray: true,
  llm_backend: "bundled", llm_endpoint: "http://localhost:11434/v1", llm_endpoint_model: "qwen3:8b", llm_api_key: null, cleanup_enabled: false, diarize_enabled: true,
  summary_enabled: false, default_template_id: "general", share_port: 47123, calendar_sources: [], calendar_refresh_min: 15, webhook_url: "", webhook_enabled: false, auto_update_check: false,
};

const templates: Template[] = [
  { id: "general", name: "General meeting", description: "Balanced notes", body: "...{{transcript}}", builtin: true },
  { id: "sales-call", name: "Sales call", description: "Deal notes", body: "...{{transcript}}", builtin: true },
];

const chatMsgs: ChatMessage[] = [
  { id: "c1", chat_id: "chat1", role: "user", content: "What did we decide about pricing?", citations: [], created_at: iso(10) },
  { id: "c2", chat_id: "chat1", role: "assistant", content: "You decided to launch with the **monthly plan only** and push the annual plan to after launch [0:55]. Sam is updating the pricing page by Wednesday, pending final copy [1:32].", citations: [{ ms: 55000, meeting_id: "m1", label: "0:55" }, { ms: 92000, meeting_id: "m1", label: "1:32" }], created_at: iso(10) },
];

const folders: Folder[] = [
  { id: "f1", name: "Launch", parent_id: null, default_template_id: null, meeting_count: 1 },
  { id: "f2", name: "Clients", parent_id: null, default_template_id: null, meeting_count: 1 },
  { id: "f3", name: "Acme", parent_id: "f2", default_template_id: null, meeting_count: 0 },
];
const tags: Tag[] = [{ id: "t1", name: "sales", meeting_count: 1 }, { id: "t2", name: "planning", meeting_count: 2 }];
const meetingTagIds: Record<string, string[]> = { m1: ["t2"], m2: ["t2"], m3: ["t1"] };

function previewLibraryRows(rows: Meeting[]) {
  if (new URLSearchParams(window.location.search).get("mode") !== "service-desktop") return rows;
  return rows.map((meeting) => ({ ...meeting, metadata_revision: meeting.metadata_revision ?? meeting.id }));
}

function mutatePreviewLibrary(request: ServiceLibraryMutation) {
  if (request.op === "folder_create") {
    const id = crypto.randomUUID();
    folders.push({ id, name: request.name, parent_id: request.parent_id, default_template_id: null, meeting_count: 0, revision: id });
    return;
  }
  if (request.op === "meeting_move") {
    const meeting = meetings.find(({ id }) => id === request.meeting_id);
    if (!meeting || (meeting.metadata_revision ?? meeting.id) !== request.expected_revision) throw { code: "native.serviceOrganizationRefreshRequired" };
    meeting.folder_id = request.folder_id;
    meeting.metadata_revision = crypto.randomUUID();
    return;
  }
  const index = folders.findIndex(({ id }) => id === request.folder_id);
  const folder = folders[index];
  if (!folder || (folder.revision ?? folder.id) !== request.expected_revision) throw { code: "native.serviceOrganizationRefreshRequired" };
  if (request.op === "folder_rename") {
    folder.name = request.name;
    folder.revision = crypto.randomUUID();
  } else {
    folders.splice(index, 1);
    for (const child of folders) if (child.parent_id === folder.id) child.parent_id = folder.parent_id;
    for (const meeting of meetings) if (meeting.folder_id === folder.id) meeting.folder_id = folder.parent_id;
  }
}

function filteredMeetings(value: unknown): Meeting[] {
  const filter = value as { folder?: unknown; tag_id?: string | null } | undefined;
  const raw = filter?.folder;
  let folder: string | null | undefined;
  if (Array.isArray(raw)) {
    if (raw.length !== 1 || (raw[0] !== null && typeof raw[0] !== "string")) throw new Error("Expected one folder or null");
    folder = raw[0] as string | null;
  } else if (typeof raw === "string") folder = raw; // Legacy native payload.
  else if (raw != null) throw new Error("Invalid folder filter");
  const subtree = new Set<string>();
  if (typeof folder === "string") {
    subtree.add(folder);
    // Match native folder selection, including descendants without recursive cycles.
    for (let size = -1; size !== subtree.size;) {
      size = subtree.size;
      for (const child of folders) if (child.parent_id && subtree.has(child.parent_id)) subtree.add(child.id);
    }
  }
  return meetings.filter((meeting) =>
    (folder === undefined || (folder === null ? meeting.folder_id === null : meeting.folder_id !== null && subtree.has(meeting.folder_id)))
    && (!filter?.tag_id || meetingTagIds[meeting.id]?.includes(filter.tag_id)));
}

let codexMcpEnabled = false;
const claudeMcpStatus = { code_enabled: false, desktop_enabled: false };
let discordStatus: DiscordSettingsStatus = { bot: null, channel: null };
const discordDemoChannel: DiscordChannel = {
  guild_id: "111000000000000001", guild_name: "Demo server",
  channel_id: "111000000000000002", channel_name: "Meetings",
};
const emptyAttendeeStatus: AttendeeSettingsStatus = {
  apiUrl: null, callbackUrl: null, bindAddress: null, certificatePath: null, privateKeyPath: null,
  configured: false, managed: false, setupAvailable: true,
};
let attendeeStatus = emptyAttendeeStatus;
let mockMeetTranscript: BotTranscript | null = null;
const mockBotConnections = new Map<string, ReturnType<typeof setTimeout>>();

function mockBotSources(meetingId: unknown): BotTranscriptSource[] {
  return meetingId === "m2" ? [{ source: "meet", track_count: 2, transcript: mockMeetTranscript }] : [];
}

const table: Record<string, (args: Record<string, unknown>) => unknown> = {
  get_ui_locale: () => mockLocale(),
  set_ui_locale: ({ locale }) => {
    if (locale !== "ru" && locale !== "en") throw new Error("Unsupported interface language");
    localStorage.setItem(localeKey, locale);
    emitMockEvent("ui-locale-changed", { locale });
    return locale;
  },
  soniox_key_status: () => false,
  save_soniox_key: ({ key }) => Boolean(key),
  discord_status: () => discordStatus,
  discord_open_developer_portal: () => {
    window.open("https://discord.com/developers/applications", "_blank", "noopener,noreferrer");
  },
  discord_save_token: ({ token }) => {
    if (typeof token !== "string" || !token.trim()) throw { code: "native.discordTokenInvalid" };
    discordStatus = { ...discordStatus, bot: { id: "111000000000000003", name: "Demo bot" } };
    return discordStatus;
  },
  discord_disconnect: () => {
    discordStatus = { bot: null, channel: null };
    return discordStatus;
  },
  discord_guilds: () => discordStatus.bot ? [{ id: discordDemoChannel.guild_id, name: discordDemoChannel.guild_name }] : [],
  discord_channels: ({ guildId }) => discordStatus.bot && guildId === discordDemoChannel.guild_id ? [discordDemoChannel] : [],
  discord_set_channel: ({ input }) => {
    if (!discordStatus.bot) throw { code: "native.discordNotConfigured" };
    if (input !== `https://discord.com/channels/${discordDemoChannel.guild_id}/${discordDemoChannel.channel_id}`) throw { code: "native.discordChannelUnavailable" };
    discordStatus = { ...discordStatus, channel: discordDemoChannel };
    return discordDemoChannel;
  },
  discord_invite_url: ({ guildId }) => {
    if (!discordStatus.bot) throw { code: "native.discordNotConfigured" };
    const params = new URLSearchParams({ client_id: discordStatus.bot.id, scope: "bot", permissions: "1049600" });
    if (typeof guildId === "string") params.set("guild_id", guildId);
    return `https://discord.com/oauth2/authorize?${params}`;
  },
  discord_recording_status: () => null,
  attendee_status: () => attendeeStatus,
  attendee_setup: () => {
    attendeeStatus = { ...emptyAttendeeStatus, configured: true, managed: true };
    return attendeeStatus;
  },
  attendee_configure: ({ settings: connection, apiKey }) => {
    if (!attendeeStatus.configured && !apiKey) throw { code: "native.attendeeKeyRequired" };
    attendeeStatus = { ...emptyAttendeeStatus, ...(connection as AttendeeSettings), configured: true };
    return attendeeStatus;
  },
  attendee_disconnect: () => (attendeeStatus = emptyAttendeeStatus),
  attendee_recording_status: () => null,
  attendee_tracks: () => [],
  attendee_open_track: () => { throw { code: "native.attendeeTrackUnavailable" }; },
  bot_transcript_sources: ({ meetingId }) => mockBotSources(meetingId),
  bot_transcript_jobs: () => [],
  cancel_bot_transcript: () => false,
  transcribe_bot_tracks: ({ meetingId, source }) => {
    if (source !== "meet" || mockBotSources(meetingId).length === 0) throw new Error("No participant tracks available");
    mockMeetTranscript = {
      timing: "approximate",
      partial: true,
      segments: lines.filter(([, speaker]) => speaker !== "Sam").slice(1).map(([start_ms, speaker, text]) => ({
        speaker_id: speaker,
        speaker_name: speaker === "Priya" ? "Priya Patel" : speaker,
        start_ms,
        end_ms: start_ms + 9000,
        text,
      })),
    };
    return mockMeetTranscript;
  },
  stt_transcript: () => null,
  transcript_versions: ({ id }) => {
    const key = String(id);
    const current = meetingDetails.get(key);
    const initial: TranscriptVersion[] = current?.segments.length ? [{
      id: "original", source: "existing", model: null, language: null,
      created_at: current.meeting.ended_at ?? current.meeting.created_at,
    }] : [];
    const participants = mockBotSources(key).find(({ track_count }) => track_count > 0);
    return { versions: [...initial, ...(mockVersions.get(key) ?? []).map(({ version }) => version)],
      initial_id: initial[0]?.id ?? null, effective_id: initial[0]?.id ?? null, participant_source: participants?.source ?? null };
  },
  transcript_version: ({ id, versionId }) => {
    const key = String(id);
    const segments = versionId === "original" ? meetingDetails.get(key)?.segments
      : mockVersions.get(key)?.find(({ version }) => version.id === versionId)?.segments;
    if (!segments) throw new Error("Unknown transcript version");
    return structuredClone(segments);
  },
  transcript_version_job: () => null,
  cancel_transcript_version: () => false,
  regenerate_transcript: ({ id, options }) => {
    const key = String(id);
    const current = meetingDetails.get(key);
    const choice = options as TranscriptVersionOptions;
    const participantSource = mockBotSources(key).find(({ track_count }) => track_count > 0)?.source;
    if (choice.source === "combined" && !participantSource) throw new Error("No participant tracks available");
    const source = choice.source === "participants" ? participantSource : choice.source === "combined" && participantSource ? "combined" : "recording";
    if (!source || !current?.segments.length) throw new Error("No sample speech available for this version");
    const version: TranscriptVersion = { id: crypto.randomUUID(), source, model: choice.model ?? null,
      language: choice.language ?? null, created_at: new Date().toISOString() };
    const segments = current.segments.map((segment, index) => ({
      ...segment, id: -(index + 1), clean_text: null, clean_text_manual: false,
      ...(source === "combined" ? { provenance: {
        source: index === 0 ? "bot" as const : "system" as const,
        provider: index === 0 ? participantSource : null,
        participant_id: index === 0 ? "preview-participant" : null,
        source_start_ms: segment.start_ms, source_end_ms: segment.end_ms, uncertain: false,
      }, playback: { start_ms: segment.start_ms, end_ms: segment.end_ms } } : {}),
    }));
    mockVersions.set(key, [...(mockVersions.get(key) ?? []), { version, segments }]);
    return version;
  },
  list_meetings: () => previewLibraryRows(meetings),
  list_meetings_filtered: ({ filter }) => previewLibraryRows(filteredMeetings(filter)),
  get_meeting: ({ id }) => structuredClone(meetingDetails.get(String(id)) ?? meetingDetails.get("m1")),
  release_meeting_media: () => undefined,
  set_transcript_correction: ({ id, segmentId, text }) => {
    const current = meetingDetails.get(String(id));
    if (!current) throw { code: "native.meetingNotFound", args: {} };
    if (current.meeting.status !== "ready") throw { code: "native.transcriptCorrectionNotReady", args: {} };
    const segment = current.segments.find((row) => row.id === segmentId && row.meeting_id === id);
    if (!segment) throw { code: "native.transcriptCorrectionSegment", args: {} };
    if (text !== null && (typeof text !== "string" || text.includes("\0"))) throw { code: "native.transcriptCorrectionInvalid", args: {} };
    if (typeof text === "string" && new TextEncoder().encode(text).length > 65536) throw { code: "native.transcriptCorrectionTooLarge", args: { maxBytes: 65536 } };
    segment.clean_text = text as string | null;
    segment.clean_text_manual = text !== null;
    return { ...segment };
  },
  get_settings: () => settings,
  set_settings: ({ new: s }) => Object.assign(settings, s),
  get_codex_mcp_status: () => ({ enabled: codexMcpEnabled }),
  set_codex_mcp_enabled: ({ enabled }) => {
    codexMcpEnabled = enabled === true;
    return { enabled: codexMcpEnabled };
  },
  get_claude_mcp_status: () => ({ ...claudeMcpStatus }),
  set_claude_mcp_enabled: ({ client, enabled }) => {
    if (client !== "code" && client !== "desktop") throw new Error("Unsupported Claude MCP client");
    claudeMcpStatus[`${client}_enabled`] = enabled === true;
    return { ...claudeMcpStatus };
  },
  recording_status: () => ({ state: "idle", meeting_id: null, elapsed_ms: 0 }),
  connect_recording_bot: ({ requestId, target }) => {
    const id = String(requestId);
    const { provider } = target as RecordingBotTarget;
    mockBotConnections.set(id, setTimeout(() => {
      mockBotConnections.delete(id);
      emitMockEvent("recording_bot_connection_status", {
        requestId: id, revision: 2, phase: "connected",
      } satisfies RecordingBotConnectionStatus);
    }, 650));
    return {
      requestId: id, revision: 1, phase: provider === "meet" ? "waiting_admission" : "connecting",
    } satisfies RecordingBotConnectionStatus;
  },
  disconnect_recording_bot: ({ requestId }) => {
    const id = String(requestId);
    clearTimeout(mockBotConnections.get(id));
    mockBotConnections.delete(id);
  },
  start_recording: () => {
    const recording = mockLocale() === "ru" ? recordingRu : recordingEn;
    throw { message: recording.setup.previewOnly };
  },
  list_upcoming: () => [
    { uid: "e1", title: "Design review with Acme", start: iso(-20), end: iso(-50), attendees: ["Sarah Chen", "Marcus Webb"], url: "https://meet.google.com/abc-defg-hij", location: null },
    { uid: "e2", title: "Weekly standup", start: iso(-150), end: iso(-165), attendees: [], url: null, location: null },
  ],
  refresh_calendar: () => [],
  search: () => [],
  list_folders: () => folders,
  get_service_library_organization: () => ({
    folders: folders.map((folder) => ({ ...folder, revision: folder.revision ?? folder.id,
      meeting_count: meetings.filter(({ folder_id }) => folder_id === folder.id).length })),
    tags,
  }),
  mutate_service_library_organization: ({ request }) => mutatePreviewLibrary(request as ServiceLibraryMutation),
  list_tags: () => tags,
  meeting_tags: ({ id }) => tags.filter((tag) => meetingTagIds[String(id)]?.includes(tag.id)),
  get_summary: ({ id }) => id === "text-only" ? null : summary,
  subscription_agents: () => [{ agent: "codex", status: "missing" }, { agent: "claude", status: "missing" }],
  get_subscription_job: () => null,
  start_subscription: () => { throw { code: "native.subscriptionMissing" }; },
  cancel_subscription: () => null,
  get_service_summary: () => null,
  get_service_summary_job: () => null,
  prepare_service_summary: () => { throw { code: "native.subscriptionMissing" }; },
  generate_service_summary: () => { throw { code: "native.subscriptionMissing" }; },
  cancel_service_summary: () => null,
  summary_job: () => ({ running: false, agent: null }),
  generate_summary: () => undefined,
  cancel_summary: () => undefined,
  list_templates: () => templates,
  list_chats: () => [{ id: "chat1", scope_kind: "meeting", scope_id: "m1", title: "What did we decide about pricing?", created_at: iso(10) }],
  chat_messages: () => chatMsgs,
  list_shares: () => [],
  list_audio_devices: () => ({ inputs: [], outputs: [] }),
  list_known_speakers: () => [{ id: "s1", name: "Sarah Chen" }, { id: "s2", name: "Priya Patel" }],
  list_voice_profiles: () => [...voiceProfiles],
  create_voice_profile: ({ name }) => {
    const profile: VoiceProfile = { id: crypto.randomUUID(), name: String(name), model_id: "demo", sample_count: 0, has_voiceprint: false };
    voiceProfiles.push(profile);
    return profile;
  },
  rename_voice_profile: ({ id, name }) => {
    const p = voiceProfiles.find((p) => p.id === id);
    if (!p) throw new Error("Voice profile not found");
    p.name = String(name);
    return { ...p };
  },
  delete_known_speaker: ({ id }) => { const i = voiceProfiles.findIndex((p) => p.id === id); if (i >= 0) voiceProfiles.splice(i, 1); },
  list_voice_samples: () => [],
  add_voice_sample: () => { throw new Error("Voice sample extraction requires the desktop app and local models."); },
  delete_voice_sample: () => undefined,
  recompute_voice_profile: ({ id }) => voiceProfiles.find((p) => p.id === id),
  voice_sample_audio: () => { throw new Error("Voice sample playback requires the desktop app."); },
  rename_speaker: ({ label, name }) => ({ speaker: { id: "s1", name }, name, merged_from: label === name ? null : null, moved_segments: 3 }),
  list_video_sources: () => [
    { target: { kind: "monitor", index: 0 }, screen_id: "mock-display-1", label: "DISPLAY1 (2560x1440)", is_primary: true },
    { target: { kind: "monitor", index: 1 }, screen_id: "mock-display-2", label: "DISPLAY2 (1920x1080)" },
  ] satisfies VideoSource[],
  sample_levels: async () => {
    // Match the native probe duration so onboarding yields to rendering/input.
    await new Promise((resolve) => setTimeout(resolve, 400));
    return [-30 + Math.random() * 10, -28 + Math.random() * 12];
  },
  probe_hardware: () => ({ hardware: { cpu_cores: 16, cpu_name: "Ryzen 9 5900X", ram_gb: 31.2, gpu: { name: "NVIDIA GeForce RTX 3080", vram_gb: 10, backend: "cuda" }, os: "windows", arch: "x86_64" }, tier: "gpu", effective_tier: "gpu" }),
  list_models: () => [],
  data_info: () => ({ data_dir: "C:\\Users\\dallen\\AppData\\Roaming\\Bazza", recordings_bytes: 1.4e9, models_bytes: 6.9e9 }),
};

export async function mockInvoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const fn = table[cmd];
  if (!fn) return undefined as T;
  return fn(args) as T;
}

export const isMock = typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);

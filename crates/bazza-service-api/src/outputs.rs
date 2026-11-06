//! Public projections contain IDs and availability, never machine paths or credentials.
use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! output {
    ($name:ident { $($fields:tt)* }) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($fields)* }
    };
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AuthRequired,
    AccessRevoked,
    ServiceStarting,
    ServiceUnavailable,
    LibraryUnavailable,
    UpgradeRequired,
    InvalidArgument,
    NotFound,
    RevisionConflict,
    CursorStale,
    IdempotencyConflict,
    ResourceBusy,
    RateLimited,
    SearchUnavailable,
    IndexBuilding,
    SharePreparationFailed,
    BackendUnavailable,
    JobInterrupted,
    ResyncRequired,
    GuiUnavailable,
}

output!(DomainError {
    pub code: ErrorCode,
    pub message: Message,
    pub retryable: bool,
    pub retry_after_ms: Option<Millis>,
    pub current_revision: Option<Revision>,
});

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    IndexPending,
    SourceMissing,
    RetainedEntities,
    PurgePending,
    PartialSemanticCoverage,
    StaleCache,
    GuiUnavailable,
    ListenerUnavailable,
}
output!(Warning { pub code: WarningCode, pub message: Message, });

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Success<T> {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u16,
    pub library_id: LibraryId,
    pub data: T,
    #[schemars(length(max = 32))]
    pub warnings: Vec<Warning>,
}

output!(Failure {
    #[schemars(range(min = 1, max = 1))] pub schema_version: u16,
    /// May be null when authentication/discovery failed before a library is selected.
    pub library_id: Option<LibraryId>,
    pub error: DomainError,
    #[schemars(length(max = 32))] pub warnings: Vec<Warning>,
});

/// Success and failure have disjoint fields and both deny unknown fields.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolResult<T> {
    Success(Success<T>),
    Error(Failure),
}

output!(Page {
    pub next_cursor: Option<Cursor>,
    pub truncated: bool,
});
output!(TextPage {
    pub next_cursor: Option<Cursor>,
    pub truncated: bool,
    #[schemars(range(max = 65_536))] pub returned_utf8_bytes: u32,
});
output!(ContentState {
    pub state: DocumentState,
    pub revision: Revision,
    pub source_id: Option<EntityId>,
});
output!(MediaAvailability { pub audio: MediaState, pub video: MediaState, });

output!(MeetingView {
    pub meeting_id: EntityId,
    pub title: Title,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub duration_ms: Millis,
    pub content_kind: ContentKind,
    pub processing_state: ProcessingState,
    #[schemars(length(max = 64))] pub participants: Vec<Label>,
    pub folder_id: Option<EntityId>,
    #[schemars(length(max = 100))] pub tag_ids: Vec<EntityId>,
    pub calendar_uid: Option<CalendarUid>,
    pub metadata_revision: Revision,
    pub lifecycle_revision: Revision,
    pub transcript: ContentState,
    pub summary: ContentState,
    pub media: MediaAvailability,
});
output!(MeetingsPage {
    #[schemars(length(max = 100))] pub items: Vec<MeetingView>,
    pub page: Page,
});
output!(FolderView {
    pub folder_id: EntityId,
    pub name: Title,
    pub parent_id: Option<EntityId>,
    pub default_template_id: Option<EntityId>,
    pub revision: Revision,
});
output!(TagView { pub tag_id: EntityId, pub name: Title, pub revision: Revision, });
output!(LibraryStructurePage {
    #[schemars(length(max = 100))] pub folders: Vec<FolderView>,
    #[schemars(length(max = 100))] pub tags: Vec<TagView>,
    pub page: Page,
});
output!(IndexHealth {
    pub lexical: IndexState,
    pub search_generation: Revision,
    pub semantic_coverage: SemanticCoverage,
});
output!(JobHealth { pub queued: u32, pub running: u32, pub waiting_for_worker: u32, });
output!(ShareHealth { pub active_count: u32, pub listener_available: bool, });
output!(ServiceStatus {
    pub service_version: Version,
    pub api_major: u16,
    pub api_minor: u16,
    pub schema_version: u32,
    pub library_epoch: Revision,
    pub index: IndexHealth,
    pub jobs: JobHealth,
    pub shares: ShareHealth,
    /// Actual runtime capabilities; merely compiling this catalog implements none.
    #[schemars(length(max = 29))] pub available_tools: Vec<Label>,
});

output!(MutationReceipt {
    pub entity_id: EntityId,
    pub revision: Revision,
    #[schemars(range(max = 9007199254740991.0))] pub change_sequence: u64,
    pub job_id: Option<EntityId>,
    /// Durable deduplication guarantee ends at this instant; TTL policy is separate.
    pub dedup_expires_at: Timestamp,
    pub replayed: bool,
});
output!(DeleteMeetingOutcome {
    pub receipt: MutationReceipt,
    pub purge: PurgeState,
    pub retained_voice_sample_count: u32,
});
output!(DeleteContentOutcome {
    pub receipt: MutationReceipt,
    pub purge: PurgeState,
    #[schemars(length(max = 4))] pub retained_entities: Vec<RetainedEntity>,
});
output!(TranscriptMutationOutcome {
    pub receipt: MutationReceipt,
    pub source_id: Option<EntityId>,
    pub index: IndexState,
    #[schemars(range(max = 1000))] pub affected_segments: u32,
});
output!(SegmentView {
    pub segment_id: EntityId,
    pub start_ms: Millis,
    pub end_ms: Millis,
    pub speaker: Option<Label>,
    pub raw: Option<Fragment>,
    pub effective: Option<Fragment>,
    pub clean_text_manual: bool,
    pub text_complete: bool,
});
output!(SegmentContinuation {
    pub segment_id: EntityId,
    #[schemars(range(max = 9007199254740991.0))] pub offset_utf8_bytes: u64,
    pub cursor: Cursor,
});
output!(TranscriptSourceMetadata {
    pub source_id: EntityId,
    pub origin: TranscriptOrigin,
    pub source_label: Option<Label>,
    pub provider_label: Option<Label>,
    pub model_label: Option<Label>,
    pub language: Option<Language>,
    pub created_at: Timestamp,
});
output!(TranscriptPage {
    pub meeting_id: EntityId,
    pub state: DocumentState,
    pub revision: Revision,
    pub current_source_id: Option<EntityId>,
    pub source_id: Option<EntityId>,
    pub source: Option<TranscriptSourceMetadata>,
    pub representation: Representation,
    #[schemars(length(max = 100))] pub segments: Vec<SegmentView>,
    pub continuation: Option<SegmentContinuation>,
    pub page: TextPage,
});

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    ExternalAgent,
    Service,
    Imported,
    Legacy,
}
output!(DocumentProvenance {
    pub actor_kind: ActorKind,
    pub actor_label: Option<Label>,
    pub source_label: Option<Label>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
});
output!(SummaryPage {
    pub meeting_id: EntityId,
    pub state: DocumentState,
    pub revision: Revision,
    pub origin: Option<SummaryOrigin>,
    pub auto_replace: bool,
    pub automatic_generation_suppressed: bool,
    pub source_missing: bool,
    pub provenance: Option<DocumentProvenance>,
    pub format: SummaryFormat,
    pub content: Option<Fragment>,
    pub page: TextPage,
});
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WrittenSummaryOrigin {
    Manual,
    ExternalAgent,
}

output!(SummaryMutationOutcome {
    pub receipt: MutationReceipt,
    pub origin: WrittenSummaryOrigin,
    #[schemars(extend("const" = false))]
    pub auto_replace: bool,
});
output!(NoteMetadata {
    pub note_id: EntityId,
    pub meeting_id: Option<EntityId>,
    pub title: Title,
    pub revision: Revision,
    pub provenance: DocumentProvenance,
});
output!(NotesPage {
    #[schemars(length(max = 100))] pub items: Vec<NoteMetadata>,
    pub page: Page,
});
output!(NotePage {
    pub note: NoteMetadata,
    pub content: Fragment,
    pub page: TextPage,
});
output!(SearchHit {
    pub kind: SourceKind,
    pub meeting_id: Option<EntityId>,
    pub note_id: Option<EntityId>,
    pub source_id: EntityId,
    pub source_revision: Revision,
    pub representation: Representation,
    pub title: Title,
    pub date: Option<Timestamp>,
    pub excerpt: Excerpt,
    pub source_uri: SourceUri,
    pub start_ms: Option<Millis>,
    pub end_ms: Option<Millis>,
});
output!(SearchDiagnostics {
    pub lexical: IndexState,
    pub semantic_coverage: SemanticCoverage,
    pub mode: SearchMode,
    pub no_matches: bool,
    pub search_generation: Revision,
});
output!(SearchPage {
    #[schemars(length(max = 100))] pub hits: Vec<SearchHit>,
    pub diagnostics: SearchDiagnostics,
    pub page: Page,
});
output!(CalendarEventView {
    pub source_id: EntityId,
    pub uid: CalendarUid,
    pub occurrence_start: Timestamp,
    pub title: Title,
    pub start: Timestamp,
    pub end: Timestamp,
    #[schemars(length(max = 64))] pub participants: Vec<Label>,
    pub linked_meeting_id: Option<EntityId>,
});
output!(CalendarSourceHealth {
    pub source_id: EntityId,
    pub label: Label,
    pub freshness: Freshness,
    pub last_success_at: Option<Timestamp>,
    pub error: Option<DomainError>,
});
output!(CalendarEventsPage {
    #[schemars(length(max = 100))] pub items: Vec<CalendarEventView>,
    #[schemars(length(max = 100))] pub sources: Vec<CalendarSourceHealth>,
    pub page: Page,
});
output!(ShareView {
    pub share_id: EntityId,
    pub meeting_id: EntityId,
    pub revision: Revision,
    pub kind: ShareKind,
    pub mode: ShareMode,
    pub fields: ShareFields,
    pub state: ShareState,
    pub expires_at: Option<Timestamp>,
    pub listener_available: bool,
    /// Only populated when the caller explicitly requested include_url.
    pub url: Option<ShareUrl>,
});
output!(SharesPage {
    #[schemars(length(max = 100))] pub items: Vec<ShareView>,
    pub page: Page,
});
output!(ShareMutationOutcome {
    pub receipt: MutationReceipt,
    pub share: ShareView,
});
output!(JobView {
    pub job_id: EntityId,
    pub revision: Revision,
    pub kind: PublicJobKind,
    pub executor_kind: ExecutorKind,
    pub status: JobState,
    pub stage: Label,
    pub cancel_requested: bool,
    pub cancellable: bool,
    pub cancellation_reason: Option<Message>,
    pub error: Option<DomainError>,
});
output!(JobMutationOutcome {
    pub receipt: MutationReceipt,
    pub job: JobView,
});

//! Strict typed tool inputs. Schema validation precedes serde deserialization.
use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! input {
    ($name:ident { $($fields:tt)* }) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($fields)* }
    };
}

input!(GetStatusInput {});
input!(ListMeetingsInput {
    pub filter: Option<MeetingFilter>,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
});
input!(GetMeetingInput { pub meeting_id: EntityId, });
input!(GetLibraryStructureInput {
    #[serde(default)] pub kind: StructureKind,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
});
input!(CreateMeetingInput {
    pub idempotency_key: IdempotencyKey,
    pub title: Title,
    pub started_at: Timestamp,
    #[serde(default)] #[schemars(length(max = 64))] pub participants: Vec<Label>,
    pub folder_id: Option<EntityId>,
    #[serde(default)] #[schemars(length(max = 100))] pub tag_ids: Vec<EntityId>,
    pub calendar_uid: Option<CalendarUid>,
    #[schemars(length(max = 1000))] pub transcript: Option<Vec<TimedSegment>>,
});
input!(MeetingPatch {
    pub title: Option<Title>,
    pub started_at: Option<Timestamp>,
    #[schemars(length(max = 64))] pub participants: Option<Vec<Label>>,
    pub folder: Option<OptionalIdPatch>,
    pub calendar_link: Option<CalendarLinkPatch>,
    #[serde(default)] #[schemars(length(max = 100))] pub tag_operations: Vec<TagOperation>,
});
input!(UpdateMeetingInput {
    pub meeting_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    pub patch: MeetingPatch,
});
input!(DeleteMeetingInput {
    pub meeting_id: EntityId,
    /// The lifecycle state revision, not a stale metadata snapshot.
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
});
input!(GetTranscriptInput {
    pub meeting_id: EntityId,
    pub source_id: Option<EntityId>,
    #[serde(default)] pub representation: Representation,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
    #[serde(default)] pub max_bytes: PageBytes,
});
input!(ImportTranscriptInput {
    pub meeting_id: EntityId,
    /// Persistent state revision is required even when current transcript is absent.
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    #[schemars(length(min = 1, max = 1000))] pub segments: Vec<TimedSegment>,
    pub source_label: Option<Label>,
    pub language: Option<Language>,
});
input!(ReplaceTranscriptInput {
    pub meeting_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    #[schemars(length(min = 1, max = 1000))] pub segments: Vec<TimedSegment>,
    pub source_label: Option<Label>,
    pub language: Option<Language>,
});
input!(SetTranscriptCorrectionsInput {
    pub meeting_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    #[schemars(length(min = 1, max = 100))] pub corrections: Vec<TranscriptCorrection>,
});
input!(DeleteTranscriptInput {
    pub meeting_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
});
input!(GetSummaryInput {
    pub meeting_id: EntityId,
    #[serde(default)] pub format: SummaryFormat,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub max_bytes: PageBytes,
});
input!(WriteSummaryInput {
    pub meeting_id: EntityId,
    pub mode: SummaryWriteMode,
    /// Persistent summary-state revision, including absent/deleted state.
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    pub markdown: DocumentBody,
    /// Bounded encoded JSON, preserving arbitrary existing summary structures.
    pub structured_json: Option<DocumentBody>,
    pub source_label: Option<Label>,
});
input!(DeleteSummaryInput {
    pub meeting_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
});
input!(ListNotesInput {
    #[serde(default)] pub scope: NoteScope,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
});
input!(GetNoteInput {
    pub note_id: EntityId,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub max_bytes: PageBytes,
});
input!(CreateNoteInput {
    pub idempotency_key: IdempotencyKey,
    pub title: Title,
    pub markdown: DocumentBody,
    pub meeting_id: Option<EntityId>,
});
input!(NotePatch {
    pub title: Option<Title>,
    pub markdown: Option<DocumentBody>,
    /// Explicit Set or Clear avoids ambiguity between omission and null.
    pub association: Option<OptionalIdPatch>,
});
input!(UpdateNoteInput {
    pub note_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    pub patch: NotePatch,
});
input!(DeleteNoteInput {
    pub note_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
});
input!(SearchInput {
    pub query: Query,
    /// Omission searches all four public fields; live AI notes are not user notes.
    #[schemars(length(min = 1, max = 4))] pub fields: Option<Vec<SearchField>>,
    pub filter: Option<MeetingFilter>,
    #[serde(default)] pub representation: Representation,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
});
input!(ListCalendarEventsInput {
    pub from: Timestamp,
    pub to: Timestamp,
    #[schemars(length(max = 100))] pub source_ids: Option<Vec<EntityId>>,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
});
input!(RefreshCalendarInput {
    pub idempotency_key: IdempotencyKey,
    #[schemars(length(max = 100))] pub source_ids: Option<Vec<EntityId>>,
});
input!(ListSharesInput {
    pub meeting_id: Option<EntityId>,
    #[serde(default)] pub include_url: bool,
    pub cursor: Option<Cursor>,
    #[serde(default)] pub limit: PageLimit,
});
input!(CreateShareInput {
    pub meeting_id: EntityId,
    pub idempotency_key: IdempotencyKey,
    pub kind: ShareKind,
    #[serde(default)] pub mode: ShareMode,
    pub fields: ShareFields,
    pub expires_at: Option<Timestamp>,
    #[serde(default)] pub include_url: bool,
});
input!(SharePatch {
    pub enabled: Option<bool>,
    pub expiry: Option<ExpiryPatch>,
    pub mode: Option<ShareMode>,
    pub fields: Option<ShareFields>,
});
input!(UpdateShareInput {
    pub share_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
    pub patch: SharePatch,
    #[serde(default)] pub include_url: bool,
});
input!(DeleteShareInput {
    pub share_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
});
input!(GetJobInput { pub job_id: EntityId, });
input!(CancelJobInput {
    pub job_id: EntityId,
    pub expected_revision: Revision,
    pub idempotency_key: IdempotencyKey,
});

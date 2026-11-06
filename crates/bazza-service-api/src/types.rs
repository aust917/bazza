use schemars::{generate::SchemaGenerator, json_schema, JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Bounded Unicode text. Byte budgets are enforced by the contract validator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Text<const MIN: usize, const MAX: usize>(pub String);

impl<const MIN: usize, const MAX: usize> JsonSchema for Text<MIN, MAX> {
    fn schema_name() -> Cow<'static, str> {
        format!("Text_{MIN}_{MAX}").into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"string","minLength":MIN,"maxLength":MAX,"pattern":"^[^\\u0000]*$"})
    }
}

pub type Title = Text<1, 512>;
pub type Label = Text<1, 256>;
pub type Message = Text<1, 512>;
pub type Fragment = Text<0, 65_536>;
pub type DocumentBody = Text<0, 262_144>;
pub type Query = Text<1, 2_048>;
pub type Excerpt = Text<0, 2_048>;
pub type CalendarUid = Text<1, 512>;

macro_rules! token_type {
    ($name:ident, $max:literal, $pattern:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(
            #[schemars(length(min = 1, max = $max), regex(pattern = $pattern))] pub String,
        );
    };
}

token_type!(EntityId, 128, "^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$");
token_type!(LibraryId, 128, "^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$");
token_type!(Revision, 128, "^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$");
token_type!(IdempotencyKey, 128, "^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$");
token_type!(Cursor, 4096, "^[A-Za-z0-9][A-Za-z0-9_.-]{0,4095}$");
token_type!(
    SourceUri,
    1024,
    "^bazza://[A-Za-z0-9][A-Za-z0-9/_?=&:.-]{0,1014}$"
);
token_type!(Language, 64, "^[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*$");
token_type!(Version, 64, "^[A-Za-z0-9][A-Za-z0-9_.+-]{0,63}$");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Timestamp(
    #[schemars(length(min = 20, max = 64), extend("format" = "date-time"))] pub String,
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ShareUrl(
    #[schemars(length(min = 1, max = 2048), regex(pattern = "^https?://[^\\s]+$"), extend("format" = "uri"))]
    pub String,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Millis(#[schemars(range(max = 9007199254740991.0))] pub u64);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PageLimit(#[schemars(range(min = 1, max = 100))] pub u16);

impl Default for PageLimit {
    fn default() -> Self {
        Self(20)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PageBytes(#[schemars(range(min = 1, max = 65_536))] pub u32);

impl Default for PageBytes {
    fn default() -> Self {
        Self(65_536)
    }
}

macro_rules! wire_enum {
    (default $name:ident { $($(#[$attribute:meta])* $variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$attribute])* $variant),+ }
    };
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

wire_enum!(ContentKind { Recorded, Text });
wire_enum!(ProcessingState {
    Recording,
    Processing,
    Ready,
    Failed
});
wire_enum!(DocumentState {
    Absent,
    Present,
    Deleted
});
wire_enum!(MediaState {
    Available,
    Missing,
    Unavailable,
    None
});
wire_enum!(default Representation {
    Raw,
    #[default]
    Effective,
    Both
});
wire_enum!(SourceKind {
    Title,
    Transcript,
    Summary,
    Note
});
wire_enum!(SearchField {
    Title,
    Transcript,
    Summary,
    Notes
});
wire_enum!(TranscriptOrigin {
    Legacy,
    Imported,
    ExternalAgent,
    Captured
});
wire_enum!(SummaryOrigin {
    Legacy,
    Imported,
    Manual,
    ExternalAgent,
    ConfiguredGeneration
});
wire_enum!(SummaryWriteMode { Create, Replace });
wire_enum!(default SummaryFormat {
    #[default]
    Markdown,
    StructuredJson
});
wire_enum!(default ShareMode { #[default] Snapshot, Live });
wire_enum!(ShareState {
    Preparing,
    Active,
    Disabled,
    Failed,
    Revoked,
    Expired
});
wire_enum!(IndexState {
    Ready,
    Pending,
    Building,
    Unavailable
});
wire_enum!(PurgeState { Complete, Pending });
wire_enum!(RetainedEntity {
    Media,
    Notes,
    Summary,
    VoiceSamples
});
wire_enum!(ExecutorKind { Service, GuiWorker });
wire_enum!(JobState {
    Queued,
    WaitingForWorker,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted
});
wire_enum!(PublicJobKind {
    CalendarRefresh,
    SharePreparation,
    TranscriptImport,
    MeetingDeletion,
    ContentDeletion,
    IndexRebuild
});
wire_enum!(SearchMode { Lexical, Hybrid });
wire_enum!(SemanticCoverage {
    None,
    Partial,
    Full
});
wire_enum!(Freshness {
    Fresh,
    Stale,
    NeverSynced,
    Refreshing
});
wire_enum!(default StructureKind { #[default] All, Folders, Tags });

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum OptionalIdPatch {
    Set { value: EntityId },
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum CalendarLinkPatch {
    Set { value: CalendarUid },
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpiryPatch {
    Set { value: Timestamp },
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum TagOperation {
    Add { tag_id: EntityId },
    Remove { tag_id: EntityId },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptCorrection {
    Set {
        segment_id: EntityId,
        text: Fragment,
    },
    Reset {
        segment_id: EntityId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ShareKind {
    Meeting,
    Clip { start_ms: Millis, end_ms: Millis },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NoteScope {
    #[default]
    All,
    Standalone,
    Meeting {
        meeting_id: EntityId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimedSegment {
    pub start_ms: Millis,
    pub end_ms: Millis,
    pub speaker: Option<Label>,
    pub text: Fragment,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeetingFilter {
    pub from: Option<Timestamp>,
    pub to: Option<Timestamp>,
    #[schemars(length(max = 100))]
    pub meeting_ids: Option<Vec<EntityId>>,
    #[schemars(length(max = 100))]
    pub folder_ids: Option<Vec<EntityId>>,
    #[schemars(length(max = 100))]
    pub tag_ids: Option<Vec<EntityId>>,
    #[schemars(length(max = 64))]
    pub participants: Option<Vec<Label>>,
    pub content_kind: Option<ContentKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShareFields {
    pub transcript: Option<Representation>,
    #[serde(default)]
    pub summary: bool,
    #[serde(default)]
    #[schemars(length(max = 100))]
    pub note_ids: Vec<EntityId>,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub video: bool,
}

//! Closed bridge for implemented public read tools.
use crate::{
    desktop::desktop_body_fits,
    outputs::{DomainError, ErrorCode, Failure},
    types::{LibraryId, Text},
    validate_tool_input, validate_tool_output_for_input,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use uuid::Uuid;

pub const FIXTURE_READ_CAPABILITY: &str = "experimental_fixture_mcp_read";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureReadTool {
    Status,
    ListMeetings,
    GetMeeting,
    Structure,
    Transcript,
    Summary,
    ListNotes,
    GetNote,
    Search,
}

impl FixtureReadTool {
    pub const ALL: [Self; 9] = [
        Self::Status,
        Self::ListMeetings,
        Self::GetMeeting,
        Self::Structure,
        Self::Transcript,
        Self::Summary,
        Self::ListNotes,
        Self::GetNote,
        Self::Search,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Status => "bazza_get_status",
            Self::ListMeetings => "bazza_list_meetings",
            Self::GetMeeting => "bazza_get_meeting",
            Self::Structure => "bazza_get_library_structure",
            Self::Transcript => "bazza_get_transcript",
            Self::Summary => "bazza_get_summary",
            Self::ListNotes => "bazza_list_notes",
            Self::GetNote => "bazza_get_note",
            Self::Search => "bazza_search",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Status => "Read the version, index and job health of the selected Bazza library.",
            Self::ListMeetings => "List meetings and content availability. Pass the returned cursor with the same filter and limit to continue.",
            Self::GetMeeting => "Read metadata, tags and content revisions for one meeting.",
            Self::Structure => "List library folders and tags for meeting filters. Continue with the same kind and limit.",
            Self::Transcript => "Read timed raw or effective transcript text from a meeting. Both returns both forms under one byte budget and counts raw bytes first in a segment continuation. Pass the page's next cursor with the same meeting, source and representation to continue.",
            Self::Summary => "Read a meeting's saved summary and its revision. Request markdown or structured_json and continue with the same meeting and format.",
            Self::ListNotes => "List note metadata for the library or one meeting. Continue with the same scope and limit.",
            Self::GetNote => "Read a note's Markdown and provenance. Use a note search hit's note_id. Continue with the same note ID.",
            Self::Search => "Find passages in meeting titles, transcripts, summaries and notes. Use raw representation to search original transcript text. Pass the returned cursor with the same query and filters to continue.",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureReadRequest {
    pub tool: FixtureReadTool,
    pub arguments: Value,
}

impl fmt::Debug for FixtureReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FixtureReadRequest")
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

impl FixtureReadRequest {
    pub fn valid(&self) -> bool {
        desktop_body_fits(self, FixtureReadReply::MAX_ENCODED_BYTES)
            && validate_tool_input(self.tool.name(), &self.arguments).is_ok()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureReadReply {
    pub output: Value,
}

impl fmt::Debug for FixtureReadReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FixtureReadReply").finish_non_exhaustive()
    }
}

impl FixtureReadReply {
    pub const MAX_ENCODED_BYTES: usize = crate::desktop::MAX_DESKTOP_DATA_BYTES as usize;

    pub fn valid_for(&self, request: &FixtureReadRequest, library_id: Uuid) -> bool {
        self.output.get("library_id").and_then(Value::as_str)
            == Some(library_id.to_string().as_str())
            && desktop_body_fits(self, Self::MAX_ENCODED_BYTES)
            && validate_tool_output_for_input(request.tool.name(), &request.arguments, &self.output)
                .is_ok()
            && self.matches_requested_identity(request)
    }

    fn matches_requested_identity(&self, request: &FixtureReadRequest) -> bool {
        let Some(data) = self.output.get("data") else {
            return true;
        };
        match request.tool {
            FixtureReadTool::GetMeeting => {
                data.get("meeting_id") == request.arguments.get("meeting_id")
            }
            FixtureReadTool::Transcript => {
                data.get("meeting_id") == request.arguments.get("meeting_id")
                    && request
                        .arguments
                        .get("source_id")
                        .and_then(Value::as_str)
                        .is_none_or(|source| {
                            data["state"] != "present" || data["source_id"].as_str() == Some(source)
                        })
            }
            FixtureReadTool::Summary => {
                data.get("meeting_id") == request.arguments.get("meeting_id")
                    && data["format"].as_str()
                        == Some(request.arguments["format"].as_str().unwrap_or("markdown"))
            }
            FixtureReadTool::GetNote => {
                data["note"].get("note_id") == request.arguments.get("note_id")
            }
            FixtureReadTool::ListNotes => match request.arguments["scope"]["kind"].as_str() {
                Some("standalone") => data["items"]
                    .as_array()
                    .is_some_and(|items| items.iter().all(|item| item["meeting_id"].is_null())),
                Some("meeting") => data["items"].as_array().is_some_and(|items| {
                    items
                        .iter()
                        .all(|item| item["meeting_id"] == request.arguments["scope"]["meeting_id"])
                }),
                _ => true,
            },
            FixtureReadTool::Structure => match request.arguments["kind"].as_str() {
                Some("folders") => data["tags"].as_array().is_some_and(Vec::is_empty),
                Some("tags") => data["folders"].as_array().is_some_and(Vec::is_empty),
                _ => true,
            },
            _ => true,
        }
    }

    /// Fixed messages only. Raw transport, SQL and validation errors can contain
    /// library paths or argument text and are never returned through the adapter.
    pub fn failure(library_id: Uuid, code: ErrorCode) -> Self {
        let (message, retryable) = match code {
            ErrorCode::InvalidArgument => ("Invalid tool arguments.", false),
            ErrorCode::CursorStale => ("The page cursor is stale. Start the query again.", false),
            ErrorCode::NotFound => ("The selected item is unavailable.", false),
            ErrorCode::SearchUnavailable => ("The lexical search index is unavailable.", true),
            ErrorCode::ResourceBusy => ("The library is busy.", true),
            ErrorCode::RateLimited => ("Too many Bazza calls are active.", true),
            ErrorCode::LibraryUnavailable => ("The selected library is unavailable.", false),
            ErrorCode::UpgradeRequired => ("The Bazza service is incompatible.", false),
            ErrorCode::ServiceUnavailable => ("The Bazza service is unavailable.", true),
            ErrorCode::AuthRequired => ("The Bazza service requires authentication.", false),
            ErrorCode::AccessRevoked => ("The Bazza service refused access.", false),
            ErrorCode::ResyncRequired => ("The Bazza service identity changed.", false),
            _ => ("The library could not supply a bounded result.", true),
        };
        Self {
            output: serde_json::to_value(Failure {
                schema_version: 1,
                library_id: Some(LibraryId(library_id.to_string())),
                error: DomainError {
                    code,
                    message: Text(message.into()),
                    retryable,
                    retry_after_ms: None,
                    current_revision: None,
                },
                warnings: Vec::new(),
            })
            .expect("fixed failure envelope serializes"),
        }
    }
}

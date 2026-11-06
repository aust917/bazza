//! Private application protocol for immutable alternate transcript versions.

use crate::desktop::{desktop_id, desktop_revision};
use bazza_domain::{
    local_asr::MAX_LOCAL_ASR_BODY_BYTES,
    transcript_versions::{manual_transcript_version_id_valid, INITIAL_TRANSCRIPT_VERSION_ID},
};
pub use bazza_domain::transcript_versions::{
    NewTranscriptVersion, TranscriptVersion, TranscriptVersionSegment, TranscriptVersions,
    TranscriptSegmentProvenance, TranscriptSegmentSource,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DESKTOP_TRANSCRIPT_VERSIONS_CAPABILITY: &str = "desktop_transcript_versions";
pub const MAX_VERSION_TRANSFER_BYTES: usize = MAX_LOCAL_ASR_BODY_BYTES as usize;
/// Read projections may also contain cleaned rows and per-row IPC metadata.
pub const MAX_VERSION_READ_BYTES: usize = 20 * 1024 * 1024;
// Keep a worst-case escaped chunk and its RPC envelope below the desktop budget.
pub const MAX_VERSION_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopTranscriptVersionRequest {
    List {
        meeting_id: String,
    },
    Read {
        meeting_id: String,
        version_id: String,
    },
    ReadNext {
        transfer_id: Uuid,
        offset: usize,
    },
    Begin {
        meeting_id: String,
        expected_lifecycle_revision: String,
        expected_transcript_revision: String,
        version_id: Uuid,
        total_bytes: usize,
    },
    Append {
        transfer_id: Uuid,
        offset: usize,
        text: String,
    },
    Commit {
        transfer_id: Uuid,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopTranscriptVersionReply {
    Listed {
        versions: TranscriptVersions,
    },
    Missing,
    Chunk {
        transfer_id: Uuid,
        offset: usize,
        total_bytes: usize,
        text: String,
    },
    Staged {
        transfer_id: Uuid,
        received_bytes: usize,
    },
    Saved {
        version: TranscriptVersion,
    },
}

impl std::fmt::Debug for DesktopTranscriptVersionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::List { .. } => "TranscriptVersions::List",
            Self::Read { .. } => "TranscriptVersions::Read",
            Self::ReadNext { .. } => "TranscriptVersions::ReadNext",
            Self::Begin { .. } => "TranscriptVersions::Begin",
            Self::Append { .. } => "TranscriptVersions::Append",
            Self::Commit { .. } => "TranscriptVersions::Commit",
        })
    }
}

impl std::fmt::Debug for DesktopTranscriptVersionReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Listed { .. } => "TranscriptVersions::Listed",
            Self::Missing => "TranscriptVersions::Missing",
            Self::Chunk { .. } => "TranscriptVersions::Chunk",
            Self::Staged { .. } => "TranscriptVersions::Staged",
            Self::Saved { .. } => "TranscriptVersions::Saved",
        })
    }
}

pub fn version_id(value: &str) -> bool {
    value == INITIAL_TRANSCRIPT_VERSION_ID || manual_transcript_version_id_valid(value)
}

impl DesktopTranscriptVersionRequest {
    pub fn valid(&self) -> bool {
        match self {
            Self::List { meeting_id } => desktop_id(meeting_id),
            Self::Read {
                meeting_id,
                version_id: version,
            } => desktop_id(meeting_id) && version_id(version),
            Self::ReadNext {
                transfer_id,
                offset,
            } => !transfer_id.is_nil() && (1..MAX_VERSION_READ_BYTES).contains(offset),
            Self::Begin {
                meeting_id,
                expected_lifecycle_revision,
                expected_transcript_revision,
                version_id,
                total_bytes,
            } => {
                desktop_id(meeting_id)
                    && desktop_revision(expected_lifecycle_revision)
                    && desktop_revision(expected_transcript_revision)
                    && !version_id.is_nil()
                    && (1..=MAX_VERSION_TRANSFER_BYTES).contains(total_bytes)
            }
            Self::Append {
                transfer_id,
                offset,
                text,
            } => {
                !transfer_id.is_nil()
                    && !text.is_empty()
                    && text.len() <= MAX_VERSION_CHUNK_BYTES
                    && offset
                        .checked_add(text.len())
                        .is_some_and(|end| end <= MAX_VERSION_TRANSFER_BYTES)
            }
            Self::Commit { transfer_id } => !transfer_id.is_nil(),
        }
    }
}

/// Pick the next complete UTF-8 slice for the bounded JSON transport.
pub fn version_chunk(text: &str, offset: usize) -> Option<&str> {
    if offset >= text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let mut end = (offset + MAX_VERSION_CHUNK_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(offset..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_preserve_unicode_at_transport_boundary_and_are_redacted_in_debug() {
        let text = format!("{}🛰 Captions", "x".repeat(MAX_VERSION_CHUNK_BYTES - 1));
        let first = version_chunk(&text, 0).unwrap();
        assert_eq!(first.len(), MAX_VERSION_CHUNK_BYTES - 1);
        let second = version_chunk(&text, first.len()).unwrap();
        assert_eq!([first, second].concat(), text);
        let request = DesktopTranscriptVersionRequest::Append {
            transfer_id: Uuid::new_v4(),
            offset: first.len(),
            text: second.into(),
        };
        assert!(request.valid());
        assert!(!format!("{request:?}").contains("Captions"));
        assert!(version_chunk(&text, first.len() + 1).is_none());
    }
}

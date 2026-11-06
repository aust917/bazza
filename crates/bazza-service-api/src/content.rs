//! Bounded private desktop content projections. Original transcript text stays
//! separate from its effective representation; callers page large documents.
use crate::desktop::{desktop_id, desktop_revision, desktop_scope, MAX_DESKTOP_DATA_BYTES};
use serde::{Deserialize, Serialize};

pub const MAX_CONTENT_NUMBER: u64 = 9_007_199_254_740_991;
pub const FIXTURE_CONTENT_CAPABILITY: &str = "experimental_fixture_desktop_content";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDocumentState {
    Absent,
    Present,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptState {
    pub meeting_id: String,
    pub revision: String,
    pub current_source_id: Option<String>,
    pub document_state: DesktopDocumentState,
    pub deletion_epoch: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTranscriptRepresentation {
    Raw,
    Effective,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEffectiveOrigin {
    Raw,
    Automatic,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptCursor {
    pub library_scope: String,
    pub meeting_id: String,
    pub revision: String,
    pub current_source_id: Option<String>,
    pub source_id: Option<String>,
    pub representation: DesktopTranscriptRepresentation,
    pub segment_index: u64,
    pub byte_offset: u64,
    pub query_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptRequest {
    pub meeting_id: String,
    pub source_id: Option<String>,
    /// Revision read from the meeting header, never fetched implicitly on write
    /// or replaced between pages of a content snapshot.
    pub expected_revision: String,
    pub representation: DesktopTranscriptRepresentation,
    pub cursor: Option<DesktopTranscriptCursor>,
    #[serde(default = "default_limit")]
    pub limit: u16,
    #[serde(default = "default_bytes")]
    pub max_bytes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptFragment {
    pub segment_index: u64,
    pub segment_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    /// Exact UTF-8 bytes of the selected representation. Raw is never cleaned.
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub has_clean_text: bool,
    pub clean_text_manual: bool,
    /// Describes the effective overlay even when reading raw text. A manual
    /// empty string remains an effective value for manual and automatic overlays.
    pub effective_origin: DesktopEffectiveOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptPage {
    pub library_scope: String,
    pub state: DesktopTranscriptState,
    pub source_id: Option<String>,
    pub representation: DesktopTranscriptRepresentation,
    pub items: Vec<DesktopTranscriptFragment>,
    pub next: Option<DesktopTranscriptCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSummaryState {
    pub meeting_id: String,
    pub revision: String,
    pub document_state: DesktopDocumentState,
    pub deletion_epoch: i64,
    pub automatic_generation_suppressed: bool,
    pub origin: String,
    pub auto_replace: bool,
    pub source_label: Option<String>,
    pub source_revision: Option<String>,
    pub source_missing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSummaryField {
    Markdown,
    StructuredJson,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSummaryCursor {
    pub library_scope: String,
    pub meeting_id: String,
    pub revision: String,
    pub field: DesktopSummaryField,
    pub byte_offset: u64,
    pub query_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSummaryRequest {
    pub meeting_id: String,
    pub expected_revision: String,
    pub field: DesktopSummaryField,
    pub cursor: Option<DesktopSummaryCursor>,
    #[serde(default = "default_bytes")]
    pub max_bytes: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSummaryPage {
    pub library_scope: String,
    pub state: DesktopSummaryState,
    pub field: DesktopSummaryField,
    /// StructuredJson contains a UTF-8 fragment of the stored JSON text, not a
    /// parsed document. Consumers decide how much content to collect and parse.
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub next: Option<DesktopSummaryCursor>,
}

fn default_limit() -> u16 {
    20
}
fn default_bytes() -> u32 {
    MAX_DESKTOP_DATA_BYTES
}
fn budget_valid(max_bytes: u32) -> bool {
    (1024..=65_536).contains(&max_bytes)
}
fn safe_number(value: i64) -> bool {
    value >= 0 && value as u64 <= MAX_CONTENT_NUMBER
}
fn bounded_metadata(value: &str, max: usize) -> bool {
    value.len() <= max && !value.contains('\0')
}
fn query_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl DesktopTranscriptState {
    pub fn valid(&self) -> bool {
        desktop_id(&self.meeting_id)
            && desktop_revision(&self.revision)
            && safe_number(self.deletion_epoch)
            && self.current_source_id.as_deref().is_none_or(desktop_id)
            && (self.document_state == DesktopDocumentState::Present)
                == self.current_source_id.is_some()
    }
}
impl DesktopSummaryState {
    pub fn valid(&self) -> bool {
        desktop_id(&self.meeting_id)
            && desktop_revision(&self.revision)
            && safe_number(self.deletion_epoch)
            && !self.origin.is_empty()
            && bounded_metadata(&self.origin, 64)
            && self
                .source_label
                .as_deref()
                .is_none_or(|label| bounded_metadata(label, 256))
            && self.source_revision.as_deref().is_none_or(desktop_revision)
    }
}
impl DesktopTranscriptCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && desktop_id(&self.meeting_id)
            && desktop_revision(&self.revision)
            && self.current_source_id.as_deref().is_none_or(desktop_id)
            && self.source_id.as_deref().is_some_and(desktop_id)
            && self.segment_index <= MAX_CONTENT_NUMBER
            && self.byte_offset <= MAX_CONTENT_NUMBER
            && (self.segment_index > 0 || self.byte_offset > 0)
            && query_hash(&self.query_hash)
    }
}
impl DesktopSummaryCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && desktop_id(&self.meeting_id)
            && desktop_revision(&self.revision)
            && self.byte_offset > 0
            && self.byte_offset <= MAX_CONTENT_NUMBER
            && query_hash(&self.query_hash)
    }
}
impl DesktopTranscriptRequest {
    pub fn valid(&self) -> bool {
        desktop_id(&self.meeting_id)
            && desktop_revision(&self.expected_revision)
            && self.source_id.as_deref().is_none_or(desktop_id)
            && (1..=100).contains(&self.limit)
            && budget_valid(self.max_bytes)
            && self.cursor.as_ref().is_none_or(|cursor| {
                cursor.valid()
                    && cursor.meeting_id == self.meeting_id
                    && cursor.revision == self.expected_revision
                    && cursor.representation == self.representation
                    && self
                        .source_id
                        .as_ref()
                        .is_none_or(|source| cursor.source_id.as_ref() == Some(source))
            })
    }
}
impl DesktopSummaryRequest {
    pub fn valid(&self) -> bool {
        desktop_id(&self.meeting_id)
            && desktop_revision(&self.expected_revision)
            && budget_valid(self.max_bytes)
            && self.cursor.as_ref().is_none_or(|cursor| {
                cursor.valid()
                    && cursor.meeting_id == self.meeting_id
                    && cursor.revision == self.expected_revision
                    && cursor.field == self.field
            })
    }
}
impl DesktopTranscriptFragment {
    pub fn end_offset(&self) -> Option<u64> {
        self.byte_offset.checked_add(self.text.len() as u64)
    }
    pub fn valid(&self) -> bool {
        self.segment_index <= MAX_CONTENT_NUMBER
            && safe_number(self.segment_id)
            && safe_number(self.start_ms)
            && safe_number(self.end_ms)
            && self.end_ms >= self.start_ms
            && self
                .speaker
                .as_deref()
                .is_none_or(|speaker| bounded_metadata(speaker, 256))
            && self.total_bytes <= MAX_CONTENT_NUMBER
            && self.text.len() <= 65_536
            && self.end_offset().is_some_and(|end| end <= self.total_bytes)
            && ((self.total_bytes == 0 && self.byte_offset == 0 && self.text.is_empty())
                || (self.byte_offset < self.total_bytes && !self.text.is_empty()))
            && match self.effective_origin {
                DesktopEffectiveOrigin::Raw => !self.has_clean_text && !self.clean_text_manual,
                DesktopEffectiveOrigin::Automatic => self.has_clean_text && !self.clean_text_manual,
                DesktopEffectiveOrigin::Manual => self.has_clean_text && self.clean_text_manual,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_content_inputs_reject_generic_arguments_and_mismatched_cursors() {
        let json = serde_json::json!({"meeting_id":"legacy-id","source_id":null,
            "expected_revision":"r1","representation":"raw","cursor":null,
            "limit":1,"max_bytes":1024});
        let mut request: DesktopTranscriptRequest = serde_json::from_value(json.clone()).unwrap();
        assert!(request.valid());
        for key in ["actor_pid", "principal", "path", "sql", "settings"] {
            let mut bad = json.clone();
            bad[key] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<DesktopTranscriptRequest>(bad).is_err());
        }
        request.cursor = Some(DesktopTranscriptCursor {
            library_scope: "a".repeat(32),
            meeting_id: "legacy-id".into(),
            revision: "r2".into(),
            current_source_id: Some("legacy-source".into()),
            source_id: Some("legacy-source".into()),
            representation: DesktopTranscriptRepresentation::Raw,
            segment_index: 0,
            byte_offset: 4,
            query_hash: "b".repeat(64),
        });
        assert!(!request.valid());
        request.cursor.as_mut().unwrap().revision = "r1".into();
        assert!(request.valid());
        request.max_bytes = 1023;
        assert!(!request.valid());
    }

    #[test]
    fn manual_empty_overlay_and_raw_legacy_bytes_are_explicit() {
        let mut fragment = DesktopTranscriptFragment {
            segment_index: 0,
            segment_id: 1,
            start_ms: 0,
            end_ms: 1,
            speaker: None,
            text: String::new(),
            byte_offset: 0,
            total_bytes: 0,
            has_clean_text: true,
            clean_text_manual: true,
            effective_origin: DesktopEffectiveOrigin::Manual,
        };
        assert!(fragment.valid());
        fragment.text = "原\0文".into();
        fragment.total_bytes = fragment.text.len() as u64;
        assert!(fragment.valid());
        fragment.effective_origin = DesktopEffectiveOrigin::Raw;
        assert!(!fragment.valid());
        fragment.clean_text_manual = false;
        assert!(!fragment.valid());
        fragment.effective_origin = DesktopEffectiveOrigin::Automatic;
        assert!(fragment.valid());
        fragment.text.clear();
        fragment.total_bytes = 0;
        assert!(fragment.valid());
        fragment.effective_origin = DesktopEffectiveOrigin::Raw;
        assert!(!fragment.valid());
        fragment.has_clean_text = false;
        assert!(fragment.valid());
        fragment.text = "raw".into();
        fragment.total_bytes = fragment.text.len() as u64;
        fragment.byte_offset = fragment.total_bytes;
        assert!(!fragment.valid());
    }
}

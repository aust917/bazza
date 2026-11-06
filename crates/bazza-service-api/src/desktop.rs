//! Bounded private desktop projections. These are not public MCP tool DTOs.
//! No arbitrary setting key, filesystem path, SQL or caller authority is accepted.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_DESKTOP_REPLY_BYTES: usize = 65_536;
pub const MAX_DESKTOP_DATA_BYTES: u32 = 65_024;
pub const MAX_DESKTOP_ITEMS: u16 = 100;
pub const FIXTURE_DESKTOP_CAPABILITY: &str = "experimental_fixture_desktop_repository";

/// Template IDs are bounded SQL identities, not recording-directory names.
pub fn desktop_change_id(kind: &str, value: &str) -> bool {
    if kind == "template" {
        !value.is_empty() && value.len() <= 512 && !value.contains('\0')
    } else {
        desktop_id(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopEnvelope<T> {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub data: T,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DesktopLocale {
    Ru,
    En,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreferences {
    pub revision: String,
    pub ui_locale: DesktopLocale,
    pub close_to_tray: bool,
    pub hotkey: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreferencesPatch {
    pub ui_locale: Option<DesktopLocale>,
    pub close_to_tray: Option<bool>,
    pub hotkey: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopBootstrap {
    pub preferences: DesktopPreferences,
    pub change_sequence: i64,
    pub cursor_scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateDesktopPreferencesRequest {
    pub expected_revision: String,
    pub idempotency_key: String,
    pub patch: DesktopPreferencesPatch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMedia {
    pub audio: bool,
    pub video: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeeting {
    pub id: String,
    pub title: String,
    pub app: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: i64,
    pub has_video: bool,
    pub status: String,
    pub folder_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub error: Option<String>,
    pub error_truncated: bool,
    pub title_auto: bool,
    pub participants: Vec<String>,
    pub calendar_uid: Option<String>,
    pub content_kind: String,
    pub metadata_revision: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub transcript_state: String,
    pub summary_revision: String,
    pub summary_state: String,
    pub media: DesktopMedia,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum DesktopFolderFilter {
    #[default]
    All,
    Unfiled,
    Subtree {
        folder_id: String,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingFilter {
    #[serde(default)]
    pub folder: DesktopFolderFilter,
    pub tag_id: Option<String>,
    pub content_kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopCursor {
    pub library_scope: String,
    pub snapshot_sequence: i64,
    pub query_hash: String,
    pub offset: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingRequest {
    #[serde(default)]
    pub filter: DesktopMeetingFilter,
    pub cursor: Option<DesktopCursor>,
    #[serde(default = "default_limit")]
    pub limit: u16,
    #[serde(default = "default_bytes")]
    pub max_bytes: u32,
}
impl Default for DesktopMeetingRequest {
    fn default() -> Self {
        Self {
            filter: Default::default(),
            cursor: None,
            limit: default_limit(),
            max_bytes: default_bytes(),
        }
    }
}
fn default_limit() -> u16 {
    20
}
fn default_bytes() -> u32 {
    MAX_DESKTOP_DATA_BYTES
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingPage {
    pub items: Vec<DesktopMeeting>,
    pub next: Option<DesktopCursor>,
    pub snapshot_sequence: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingLookup {
    pub meeting_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopMeetingDecorations {
    pub meeting_id: String,
    pub highlights: Vec<u64>,
    pub speakers: Vec<bazza_domain::MeetingSpeaker>,
    pub speaker_stats: Vec<bazza_domain::SpeakerStat>,
}

impl DesktopMeetingDecorations {
    pub fn valid(&self, meeting_id: &str) -> bool {
        const MAX_SAFE: u64 = 9_007_199_254_740_991;
        let safe_number = |n| u64::try_from(n).is_ok_and(|n| n <= MAX_SAFE);
        self.meeting_id == meeting_id
            && self.highlights.iter().all(|n| *n <= MAX_SAFE)
            && self.speakers.iter().all(|row| {
                row.meeting_id == meeting_id && row.suggested_score.is_none_or(f32::is_finite)
            })
            && self.speaker_stats.iter().all(|row| {
                [row.segments, row.ms, row.first_ms]
                    .into_iter()
                    .all(safe_number)
            })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameDesktopMeetingRequest {
    pub meeting_id: String,
    pub expected_revision: String,
    pub idempotency_key: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteDesktopMeetingRequest {
    pub meeting_id: String,
    pub expected_lifecycle_revision: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopChangeCursor {
    pub library_scope: String,
    pub after: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopChangesRequest {
    pub cursor: Option<DesktopChangeCursor>,
    #[serde(default = "default_limit")]
    pub limit: u16,
    #[serde(default = "default_bytes")]
    pub max_bytes: u32,
}
impl Default for DesktopChangesRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: default_limit(),
            max_bytes: default_bytes(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopChange {
    pub sequence: i64,
    pub entity_kind: String,
    pub entity_id: String,
    pub revision: String,
    pub actor: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopChangePage {
    pub items: Vec<DesktopChange>,
    pub next: DesktopChangeCursor,
    pub has_more: bool,
    pub current_sequence: i64,
}

pub type DesktopMutationOutcome = crate::worker::WorkerMutationOutcome;

pub fn desktop_body_fits<T: Serialize>(value: &T, limit: usize) -> bool {
    struct Counter {
        used: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| std::io::Error::other("desktop reply budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { used: 0, limit }, value).is_ok()
}
pub fn desktop_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
pub fn desktop_revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
}
fn text(value: &str, min: usize, max: usize) -> bool {
    value.len() >= min && value.len() <= max && !value.contains('\0')
}
pub fn desktop_scope(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn page_limits(limit: u16, max_bytes: u32) -> bool {
    (1..=MAX_DESKTOP_ITEMS).contains(&limit) && (1024..=65_536).contains(&max_bytes)
}

impl DesktopPreferences {
    pub fn valid(&self) -> bool {
        desktop_revision(&self.revision) && text(&self.hotkey, 1, 256)
    }
}
impl DesktopBootstrap {
    pub fn valid(&self) -> bool {
        self.preferences.valid() && self.change_sequence >= 0 && desktop_scope(&self.cursor_scope)
    }
}
impl UpdateDesktopPreferencesRequest {
    pub fn valid(&self) -> bool {
        desktop_revision(&self.expected_revision)
            && desktop_revision(&self.idempotency_key)
            && (self.patch.ui_locale.is_some()
                || self.patch.close_to_tray.is_some()
                || self.patch.hotkey.is_some())
            && self
                .patch
                .hotkey
                .as_ref()
                .is_none_or(|hotkey| text(hotkey, 1, 256))
    }
}
impl RenameDesktopMeetingRequest {
    pub fn valid(&self) -> bool {
        desktop_id(&self.meeting_id)
            && desktop_revision(&self.expected_revision)
            && desktop_revision(&self.idempotency_key)
            && text(&self.title, 1, 512)
    }
}
impl DeleteDesktopMeetingRequest {
    pub fn valid(&self) -> bool {
        Uuid::parse_str(&self.meeting_id).is_ok_and(|id| id.to_string() == self.meeting_id)
            && desktop_revision(&self.expected_lifecycle_revision)
            && desktop_revision(&self.idempotency_key)
    }

    /// The receipt commits logical deletion and a queued purge, not the
    /// completion of filesystem work.
    pub fn accepts(&self, outcome: &DesktopMutationOutcome) -> bool {
        outcome.entity_id == self.meeting_id
            && desktop_revision(&outcome.revision)
            && outcome.revision != self.expected_lifecycle_revision
            && (1..=9_007_199_254_740_991).contains(&outcome.change_sequence)
            && outcome.job_id.as_deref().is_some_and(|value| {
                Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
            })
    }
}
impl DesktopCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && self.snapshot_sequence >= 0
            && hash(&self.query_hash)
            && self.offset <= 1_000_000
    }
}
impl DesktopMeetingRequest {
    pub fn valid(&self) -> bool {
        page_limits(self.limit, self.max_bytes)
            && self.cursor.as_ref().is_none_or(DesktopCursor::valid)
            && match &self.filter.folder {
                DesktopFolderFilter::Subtree { folder_id } => desktop_id(folder_id),
                _ => true,
            }
            && self.filter.tag_id.as_ref().is_none_or(|id| desktop_id(id))
            && self
                .filter
                .content_kind
                .as_deref()
                .is_none_or(|kind| matches!(kind, "recorded" | "text"))
    }
}
impl DesktopChangesRequest {
    pub fn valid(&self) -> bool {
        page_limits(self.limit, self.max_bytes)
            && self
                .cursor
                .as_ref()
                .is_none_or(|c| desktop_scope(&c.library_scope) && c.after >= 0)
    }
}
impl DesktopMeeting {
    pub fn valid(&self) -> bool {
        desktop_id(&self.id)
            && text(&self.title, 1, 512)
            && self.app.as_ref().is_none_or(|v| text(v, 0, 512))
            && self.duration_ms >= 0
            && self.duration_ms <= 9_007_199_254_740_991
            && matches!(
                self.status.as_str(),
                "recording" | "processing" | "ready" | "failed"
            )
            && self.folder_id.as_ref().is_none_or(|id| desktop_id(id))
            && self.error.as_ref().is_none_or(|v| text(v, 0, 2048))
            && self.participants.len() <= 64
            && self.participants.iter().all(|v| text(v, 0, 256))
            && self.calendar_uid.as_ref().is_none_or(|v| text(v, 0, 512))
            && matches!(self.content_kind.as_str(), "recorded" | "text")
            && [
                &self.metadata_revision,
                &self.lifecycle_revision,
                &self.transcript_revision,
                &self.summary_revision,
            ]
            .iter()
            .all(|v| desktop_revision(v))
            && [&self.transcript_state, &self.summary_state]
                .iter()
                .all(|v| matches!(v.as_str(), "absent" | "present" | "deleted"))
            && (!self.media.video || self.has_video)
            && !(self.content_kind == "text" && (self.media.audio || self.media.video))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Request;
    #[test]
    fn delete_receipt_requires_the_selected_meeting_new_revision_and_purge_job() {
        let mut request = DeleteDesktopMeetingRequest {
            meeting_id: Uuid::new_v4().to_string(),
            expected_lifecycle_revision: "observed".into(),
            idempotency_key: "operation".into(),
        };
        assert!(request.valid());
        let mut outcome = DesktopMutationOutcome {
            entity_id: request.meeting_id.clone(),
            revision: "committed".into(),
            change_sequence: 1,
            job_id: Some(Uuid::new_v4().to_string()),
            dedup_until: Utc::now(),
        };
        assert!(request.accepts(&outcome));
        outcome.job_id = None;
        assert!(!request.accepts(&outcome));
        outcome.job_id = Some(Uuid::new_v4().to_string());
        outcome.revision = request.expected_lifecycle_revision.clone();
        assert!(!request.accepts(&outcome));
        outcome.revision = "committed".into();
        outcome.entity_id = Uuid::new_v4().to_string();
        assert!(!request.accepts(&outcome));
        let mut value = serde_json::to_value(&request).unwrap();
        value["path"] = "caller-chosen".into();
        assert!(serde_json::from_value::<DeleteDesktopMeetingRequest>(value).is_err());
        request.meeting_id = "../unsafe".into();
        assert!(!request.valid());
    }
    #[test]
    fn desktop_wire_has_no_generic_settings_or_caller_authority() {
        let request = UpdateDesktopPreferencesRequest {
            expected_revision: "revision".into(),
            idempotency_key: "key".into(),
            patch: DesktopPreferencesPatch {
                ui_locale: Some(DesktopLocale::Ru),
                ..Default::default()
            },
        };
        assert!(request.valid());
        for field in [
            "capture_system",
            "stt_provider",
            "llm_api_key",
            "key",
            "sql",
            "path",
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            value["patch"][field] = true.into();
            assert!(serde_json::from_value::<UpdateDesktopPreferencesRequest>(value).is_err());
        }
        let mut value = serde_json::to_value(Request::DesktopPreferences).unwrap();
        value["principal"] = "admin".into();
        assert!(serde_json::from_value::<Request>(value).is_err());
    }
    #[test]
    fn desktop_budgets_and_cursor_limits_are_independent_of_public_mcp() {
        assert!(DesktopMeetingRequest::default().valid());
        assert!(!DesktopMeetingRequest {
            limit: 101,
            ..Default::default()
        }
        .valid());
        assert!(!DesktopChangesRequest {
            max_bytes: 65_537,
            ..Default::default()
        }
        .valid());
        assert!(!desktop_body_fits(
            &"я".repeat(40_000),
            MAX_DESKTOP_REPLY_BYTES
        ));
        assert!(!desktop_id("../library"));
        assert!(!desktop_revision("bad revision"));
    }
}

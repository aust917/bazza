// Store-local mirror of the private native template contract.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

pub const FIXTURE_TEMPLATES_CAPABILITY: &str = "experimental_fixture_desktop_templates";
pub const MAX_TEMPLATE_ID_BYTES: usize = 512;
pub const MAX_TEMPLATE_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_TEMPLATE_APPEND_BYTES: usize = 32 * 1024;
pub const MAX_TEMPLATE_DATA_BYTES: usize = 65_024;
pub const MAX_TEMPLATE_CHUNKS: u32 = 4096;
const MAX_SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemplateDocumentState {
    Absent,
    Present,
    Deleted,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemplateField {
    Name,
    Description,
    Body,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemplateHeader {
    pub id: String,
    pub revision: String,
    pub generation: String,
    pub state: TemplateDocumentState,
    pub builtin: bool,
    pub name_preview: String,
    pub description_preview: String,
    pub name_bytes: u64,
    pub description_bytes: u64,
    pub body_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemplateListCursor {
    pub library_scope: String,
    pub catalog_revision: String,
    pub query_hash: String,
    pub offset: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateListRequest {
    pub cursor: Option<TemplateListCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateListPage {
    pub library_scope: String,
    pub catalog_revision: String,
    pub items: Vec<TemplateHeader>,
    pub next: Option<TemplateListCursor>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemplateReadCursor {
    pub library_scope: String,
    pub template_id: String,
    pub revision: String,
    pub generation: String,
    pub field: TemplateField,
    pub byte_offset: u64,
    pub max_bytes: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateReadRequest {
    pub template_id: String,
    pub expected_revision: String,
    pub field: TemplateField,
    pub cursor: Option<TemplateReadCursor>,
    pub max_bytes: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateFieldPage {
    pub library_scope: String,
    pub header: TemplateHeader,
    pub field: TemplateField,
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub next: Option<TemplateReadCursor>,
}
impl std::fmt::Debug for TemplateHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateHeader")
            .field("state", &self.state)
            .field("builtin", &self.builtin)
            .field("body_bytes", &self.body_bytes)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for TemplateFieldPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateFieldPage")
            .field("field", &self.field)
            .field("byte_offset", &self.byte_offset)
            .field("total_bytes", &self.total_bytes)
            .field("fragment_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemplateWriteTarget {
    New {
        requested_id: String,
    },
    Existing {
        template_id: String,
        expected_revision: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateBeginWrite {
    pub upload_id: Uuid,
    pub target: TemplateWriteTarget,
    pub name: String,
    pub description: String,
    pub total_bytes: u64,
    pub sha256: String,
    pub idempotency_key: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateAppendWrite {
    pub upload_id: Uuid,
    pub sequence: u32,
    pub byte_offset: u64,
    pub text: String,
    pub sha256: String,
    pub idempotency_key: String,
}
impl std::fmt::Debug for TemplateBeginWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateBeginWrite")
            .field("upload_id", &self.upload_id)
            .field("total_bytes", &self.total_bytes)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for TemplateAppendWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateAppendWrite")
            .field("upload_id", &self.upload_id)
            .field("sequence", &self.sequence)
            .field("byte_offset", &self.byte_offset)
            .field("text_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateUploadRequest {
    pub upload_id: Uuid,
    pub idempotency_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateDeleteRequest {
    pub template_id: String,
    pub expected_revision: String,
    pub idempotency_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateSeedRequest {
    pub expected_catalog_revision: String,
    pub idempotency_key: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemplateEffect {
    Saved,
    Deleted,
    BuiltinProtectedNoOp,
    AlreadyAbsent,
    SeededMissing,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemplateReceipt {
    pub upload_id: Option<Uuid>,
    pub template_id: Option<String>,
    pub revision: String,
    pub generation: Option<String>,
    pub effect: TemplateEffect,
    pub change_sequence: Option<i64>,
    pub inserted_ids: Vec<String>,
    pub dedup_until: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemplateUploadStatus {
    Open,
    Committed,
    Aborted,
    Expired,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemplateUploadState {
    pub upload_id: Uuid,
    pub template_id: String,
    pub status: TemplateUploadStatus,
    pub next_sequence: u32,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub expires_at: DateTime<Utc>,
    pub receipt: Option<TemplateReceipt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopTemplatesRequest {
    List(TemplateListRequest),
    Describe { template_id: String },
    ReadField(TemplateReadRequest),
    BeginWrite(TemplateBeginWrite),
    AppendWrite(TemplateAppendWrite),
    CommitWrite(TemplateUploadRequest),
    AbortWrite(TemplateUploadRequest),
    Delete(TemplateDeleteRequest),
    SeedMissingBuiltins(TemplateSeedRequest),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "result",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopTemplatesReply {
    List(TemplateListPage),
    Describe(TemplateHeader),
    ReadField(TemplateFieldPage),
    Upload(TemplateUploadState),
    Mutation(TemplateReceipt),
}

pub fn template_body_fits<T: Serialize>(value: &T, max: usize) -> bool {
    struct Count {
        used: usize,
        max: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|n| *n <= self.max)
                .ok_or_else(|| std::io::Error::other("template byte budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count { used: 0, max }, value).is_ok()
}
pub fn template_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEMPLATE_ID_BYTES && !value.contains('\0')
}
fn key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn token(value: &str) -> bool {
    key(value)
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn scope(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn budget(value: u32) -> bool {
    (2048..=MAX_TEMPLATE_DATA_BYTES as u32).contains(&value)
}

impl DesktopTemplatesRequest {
    pub fn valid(&self) -> bool {
        let valid = match self {
            Self::List(r) => {
                (1..=100).contains(&r.limit)
                    && budget(r.max_bytes)
                    && r.cursor.as_ref().is_none_or(|c| {
                        scope(&c.library_scope)
                            && token(&c.catalog_revision)
                            && hash(&c.query_hash)
                            && c.offset <= MAX_SAFE
                    })
            }
            Self::Describe { template_id: id } => template_id(id),
            Self::ReadField(r) => {
                template_id(&r.template_id)
                    && token(&r.expected_revision)
                    && budget(r.max_bytes)
                    && r.cursor.as_ref().is_none_or(|c| {
                        scope(&c.library_scope)
                            && c.template_id == r.template_id
                            && c.revision == r.expected_revision
                            && token(&c.generation)
                            && c.field == r.field
                            && c.max_bytes == r.max_bytes
                            && c.byte_offset <= MAX_SAFE
                    })
            }
            Self::BeginWrite(r) => {
                !r.upload_id.is_nil()
                    && key(&r.idempotency_key)
                    && !r.name.trim().is_empty()
                    && r.name.len() <= 512
                    && r.description.len() <= 8192
                    && r.total_bytes <= MAX_TEMPLATE_BODY_BYTES
                    && hash(&r.sha256)
                    && match &r.target {
                        TemplateWriteTarget::New { requested_id } => {
                            requested_id.len() <= 512 && !requested_id.contains('\0')
                        }
                        TemplateWriteTarget::Existing {
                            template_id: id,
                            expected_revision,
                        } => template_id(id) && token(expected_revision),
                    }
            }
            Self::AppendWrite(r) => {
                !r.upload_id.is_nil()
                    && key(&r.idempotency_key)
                    && (1..=MAX_TEMPLATE_CHUNKS).contains(&r.sequence)
                    && !r.text.is_empty()
                    && r.byte_offset <= MAX_TEMPLATE_BODY_BYTES
                    && hash(&r.sha256)
            }
            Self::CommitWrite(r) | Self::AbortWrite(r) => {
                !r.upload_id.is_nil() && key(&r.idempotency_key)
            }
            Self::Delete(r) => {
                template_id(&r.template_id)
                    && token(&r.expected_revision)
                    && key(&r.idempotency_key)
            }
            Self::SeedMissingBuiltins(r) => {
                token(&r.expected_catalog_revision) && key(&r.idempotency_key)
            }
        };
        valid
            && template_body_fits(
                self,
                if matches!(self, Self::AppendWrite(_)) {
                    MAX_TEMPLATE_APPEND_BYTES
                } else {
                    MAX_TEMPLATE_DATA_BYTES
                },
            )
    }
}
impl TemplateHeader {
    pub fn valid(&self) -> bool {
        template_id(&self.id)
            && token(&self.revision)
            && token(&self.generation)
            && self.name_bytes <= MAX_SAFE
            && self.description_bytes <= MAX_SAFE
            && self.body_bytes <= MAX_SAFE
            && self.name_preview.len() <= 256
            && self.name_preview.len() as u64 <= self.name_bytes
            && self.description_preview.len() <= 512
            && self.description_preview.len() as u64 <= self.description_bytes
            && (self.state == TemplateDocumentState::Present
                || (self.name_bytes == 0
                    && self.description_bytes == 0
                    && self.body_bytes == 0
                    && self.name_preview.is_empty()
                    && self.description_preview.is_empty()))
    }
}
impl TemplateReceipt {
    pub fn valid(&self) -> bool {
        token(&self.revision)
            && self.dedup_until.timestamp_millis() > 0
            && self.generation.as_ref().is_none_or(|g| token(g))
            && self.template_id.as_ref().is_none_or(|id| template_id(id))
            && self
                .change_sequence
                .is_none_or(|s| (1..=MAX_SAFE as i64).contains(&s))
            && self.inserted_ids.len() <= 6
            && self.inserted_ids.iter().all(|id| template_id(id))
            && self.inserted_ids.iter().collect::<HashSet<_>>().len() == self.inserted_ids.len()
            && match self.effect {
                TemplateEffect::Saved => {
                    self.upload_id.is_some_and(|id| !id.is_nil())
                        && self.template_id.is_some()
                        && self.generation.is_some()
                        && self.change_sequence.is_some()
                        && self.inserted_ids.is_empty()
                }
                TemplateEffect::Deleted => {
                    self.upload_id.is_none()
                        && self.template_id.is_some()
                        && self.generation.is_some()
                        && self.change_sequence.is_some()
                        && self.inserted_ids.is_empty()
                }
                TemplateEffect::BuiltinProtectedNoOp | TemplateEffect::AlreadyAbsent => {
                    self.upload_id.is_none()
                        && self.template_id.is_some()
                        && self.generation.is_some()
                        && self.change_sequence.is_none()
                        && self.inserted_ids.is_empty()
                }
                TemplateEffect::SeededMissing => {
                    self.upload_id.is_none()
                        && self.template_id.is_none()
                        && self.generation.is_none()
                        && (self.change_sequence.is_some() != self.inserted_ids.is_empty())
                }
            }
    }
}
impl TemplateUploadState {
    pub fn valid(&self) -> bool {
        !self.upload_id.is_nil()
            && template_id(&self.template_id)
            && self.expires_at.timestamp_millis() > 0
            && (1..=MAX_TEMPLATE_CHUNKS + 1).contains(&self.next_sequence)
            && self.received_bytes <= self.total_bytes
            && self.total_bytes <= MAX_TEMPLATE_BODY_BYTES
            && if self.status == TemplateUploadStatus::Committed {
                self.received_bytes == self.total_bytes
                    && self.receipt.as_ref().is_some_and(|r| {
                        r.valid()
                            && r.effect == TemplateEffect::Saved
                            && r.upload_id == Some(self.upload_id)
                            && r.template_id.as_ref() == Some(&self.template_id)
                    })
            } else {
                self.receipt.is_none()
            }
    }
}
impl DesktopTemplatesReply {
    pub fn valid_for(&self, request: &DesktopTemplatesRequest) -> bool {
        if !template_body_fits(self, MAX_TEMPLATE_DATA_BYTES) {
            return false;
        }
        match (self, request) {
            (Self::List(p), DesktopTemplatesRequest::List(r)) => {
                let offset = r.cursor.as_ref().map_or(0, |c| c.offset);
                let mut ids = HashSet::new();
                scope(&p.library_scope)
                    && token(&p.catalog_revision)
                    && p.items.len() <= r.limit as usize
                    && p.items.iter().all(|h| {
                        h.valid() && h.state == TemplateDocumentState::Present && ids.insert(&h.id)
                    })
                    && r.cursor.as_ref().is_none_or(|c| {
                        c.library_scope == p.library_scope
                            && c.catalog_revision == p.catalog_revision
                    })
                    && p.next.as_ref().is_none_or(|c| {
                        !p.items.is_empty()
                            && c.library_scope == p.library_scope
                            && c.catalog_revision == p.catalog_revision
                            && hash(&c.query_hash)
                            && r.cursor
                                .as_ref()
                                .is_none_or(|old| old.query_hash == c.query_hash)
                            && c.offset == offset + p.items.len() as u64
                    })
                    && template_body_fits(self, r.max_bytes as usize)
            }
            (Self::Describe(h), DesktopTemplatesRequest::Describe { template_id: id }) => {
                h.valid() && h.id == *id
            }
            (Self::ReadField(p), DesktopTemplatesRequest::ReadField(r)) => {
                let offset = r.cursor.as_ref().map_or(0, |c| c.byte_offset);
                let total = match p.field {
                    TemplateField::Name => p.header.name_bytes,
                    TemplateField::Description => p.header.description_bytes,
                    TemplateField::Body => p.header.body_bytes,
                };
                let end = p.byte_offset.saturating_add(p.text.len() as u64);
                scope(&p.library_scope)
                    && p.header.valid()
                    && p.header.state == TemplateDocumentState::Present
                    && p.header.id == r.template_id
                    && p.header.revision == r.expected_revision
                    && p.field == r.field
                    && p.byte_offset == offset
                    && p.total_bytes == total
                    && end <= total
                    && r.cursor.as_ref().is_none_or(|c| {
                        c.library_scope == p.library_scope && c.generation == p.header.generation
                    })
                    && match &p.next {
                        Some(c) => {
                            !p.text.is_empty()
                                && end < total
                                && c.library_scope == p.library_scope
                                && c.template_id == p.header.id
                                && c.revision == p.header.revision
                                && c.generation == p.header.generation
                                && c.field == p.field
                                && c.byte_offset == end
                                && c.max_bytes == r.max_bytes
                        }
                        None => end == total,
                    }
                    && template_body_fits(self, r.max_bytes as usize)
            }
            (Self::Upload(p), DesktopTemplatesRequest::BeginWrite(r)) => {
                p.valid()
                    && p.upload_id == r.upload_id
                    && p.total_bytes == r.total_bytes
                    && match &r.target {
                        TemplateWriteTarget::New { requested_id }
                            if requested_id.trim().is_empty() =>
                        {
                            Uuid::parse_str(&p.template_id)
                                .is_ok_and(|id| !id.is_nil() && id.to_string() == p.template_id)
                        }
                        TemplateWriteTarget::New { requested_id } => p.template_id == *requested_id,
                        TemplateWriteTarget::Existing { template_id, .. } => {
                            p.template_id == *template_id
                        }
                    }
            }
            (Self::Upload(p), DesktopTemplatesRequest::AppendWrite(r)) => {
                p.valid()
                    && p.upload_id == r.upload_id
                    && (p.status != TemplateUploadStatus::Open
                        || (p.next_sequence > r.sequence
                            && p.received_bytes >= r.byte_offset + r.text.len() as u64))
            }
            (Self::Upload(p), DesktopTemplatesRequest::AbortWrite(r)) => {
                p.valid() && p.upload_id == r.upload_id && p.status != TemplateUploadStatus::Open
            }
            (Self::Mutation(p), DesktopTemplatesRequest::CommitWrite(r)) => {
                p.valid() && p.effect == TemplateEffect::Saved && p.upload_id == Some(r.upload_id)
            }
            (Self::Upload(p), DesktopTemplatesRequest::CommitWrite(r)) => {
                p.valid()
                    && p.upload_id == r.upload_id
                    && matches!(
                        p.status,
                        TemplateUploadStatus::Aborted | TemplateUploadStatus::Expired
                    )
            }
            (Self::Mutation(p), DesktopTemplatesRequest::Delete(r)) => {
                p.valid()
                    && p.template_id.as_ref() == Some(&r.template_id)
                    && match p.effect {
                        TemplateEffect::Deleted => p.revision != r.expected_revision,
                        TemplateEffect::BuiltinProtectedNoOp | TemplateEffect::AlreadyAbsent => {
                            p.revision == r.expected_revision
                        }
                        _ => false,
                    }
                    && matches!(
                        p.effect,
                        TemplateEffect::Deleted
                            | TemplateEffect::BuiltinProtectedNoOp
                            | TemplateEffect::AlreadyAbsent
                    )
            }
            (Self::Mutation(p), DesktopTemplatesRequest::SeedMissingBuiltins(r)) => {
                p.valid()
                    && p.effect == TemplateEffect::SeededMissing
                    && (if p.inserted_ids.is_empty() {
                        p.revision == r.expected_catalog_revision
                    } else {
                        p.revision != r.expected_catalog_revision
                    })
            }
            _ => false,
        }
    }
}

//! Private bounded chat reads. No send, completion, cancellation or mutation
//! authority is exposed; stored text and citation JSON remain opaque UTF-8.
use crate::desktop::{desktop_body_fits, desktop_revision, desktop_scope};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const FIXTURE_CHATS_CAPABILITY: &str = "experimental_fixture_desktop_chats";
pub const MAX_CHAT_DATA_BYTES: usize = 65_024;
pub const MAX_CHAT_REQUEST_BYTES: usize = 65_024;
pub const MAX_CHAT_ITEMS: u16 = 100;
pub const MAX_CHAT_ID_BYTES: usize = 512;
pub const MAX_CHAT_PREVIEW_BYTES: usize = 256;
pub const MAX_CHAT_ROLE_PREVIEW_BYTES: usize = 128;
const MAX_SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopChatScope {
    AllCatalog,
    Exact {
        kind: String,
        scope_id: Option<String>,
    },
    LikeChat {
        chat_id: String,
        generation: String,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatHeader {
    pub id: String,
    /// Original SQLite row identity, not a numerically monotonic page ordinal.
    pub rowid: String,
    pub generation: String,
    pub metadata_revision: String,
    pub messages_revision: String,
    pub erasure_revision: String,
    pub title_preview: String,
    pub scope_kind_preview: String,
    pub scope_id_preview: Option<String>,
    pub title_bytes: u64,
    pub scope_kind_bytes: u64,
    pub scope_id_bytes: Option<u64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatMessageHeader {
    pub id: String,
    pub rowid: String,
    pub chat_id: String,
    pub generation: String,
    pub revision: String,
    pub role_preview: String,
    pub role_bytes: u64,
    pub content_bytes: u64,
    pub citations_bytes: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatListCursor {
    pub library_scope: String,
    pub catalog_revision: String,
    pub query_hash: String,
    pub after_rowid: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatMessagesCursor {
    pub library_scope: String,
    pub chat_id: String,
    pub generation: String,
    pub messages_revision: String,
    pub query_hash: String,
    pub after_rowid: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatListPage {
    pub library_scope: String,
    pub catalog_revision: String,
    pub items: Vec<DesktopChatHeader>,
    pub next: Option<DesktopChatListCursor>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatMessagesPage {
    pub library_scope: String,
    pub chat_id: String,
    pub generation: String,
    pub messages_revision: String,
    pub items: Vec<DesktopChatMessageHeader>,
    pub next: Option<DesktopChatMessagesCursor>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopChatField {
    Title,
    ScopeKind,
    ScopeId,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopChatMessageField {
    Role,
    Content,
    CitationsStoredJson,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatFieldCursor {
    pub library_scope: String,
    pub chat_id: String,
    pub generation: String,
    pub metadata_revision: String,
    pub field: DesktopChatField,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub present: bool,
    pub max_bytes: u32,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatMessageFieldCursor {
    pub library_scope: String,
    pub chat_id: String,
    pub message_id: String,
    pub chat_generation: String,
    pub generation: String,
    pub revision: String,
    pub field: DesktopChatMessageField,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub max_bytes: u32,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatFieldPage {
    pub library_scope: String,
    pub chat_id: String,
    pub generation: String,
    pub metadata_revision: String,
    pub field: DesktopChatField,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub present: bool,
    pub text: String,
    pub next: Option<DesktopChatFieldCursor>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatMessageFieldPage {
    pub library_scope: String,
    pub chat_id: String,
    pub message_id: String,
    pub chat_generation: String,
    pub generation: String,
    pub revision: String,
    pub field: DesktopChatMessageField,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub text: String,
    pub next: Option<DesktopChatMessageFieldCursor>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopChatsRequest {
    List {
        scope: DesktopChatScope,
        limit: u16,
        max_bytes: u32,
        cursor: Option<DesktopChatListCursor>,
    },
    Describe {
        chat_id: String,
    },
    Messages {
        chat_id: String,
        expected_generation: String,
        expected_messages_revision: String,
        limit: u16,
        max_bytes: u32,
        cursor: Option<DesktopChatMessagesCursor>,
    },
    ReadChatField {
        chat_id: String,
        expected_generation: String,
        expected_metadata_revision: String,
        field: DesktopChatField,
        cursor: Option<DesktopChatFieldCursor>,
        max_bytes: u32,
    },
    ReadMessageField {
        chat_id: String,
        message_id: String,
        expected_chat_generation: String,
        expected_generation: String,
        expected_revision: String,
        field: DesktopChatMessageField,
        cursor: Option<DesktopChatMessageFieldCursor>,
        max_bytes: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopChatsReply {
    List(DesktopChatListPage),
    Header(DesktopChatHeader),
    Messages(DesktopChatMessagesPage),
    ChatField(DesktopChatFieldPage),
    MessageField(DesktopChatMessageFieldPage),
}

macro_rules! redacted_debug {
    ($($name:ty),+ $(,)?) => {$(
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    )+};
}
redacted_debug!(
    DesktopChatScope,
    DesktopChatHeader,
    DesktopChatMessageHeader,
    DesktopChatFieldPage,
    DesktopChatMessageFieldPage,
    DesktopChatsRequest,
    DesktopChatListCursor,
    DesktopChatMessagesCursor,
    DesktopChatListPage,
    DesktopChatMessagesPage,
    DesktopChatFieldCursor,
    DesktopChatMessageFieldCursor,
);

/// Bounded SQL identity, deliberately independent of filesystem path grammar.
pub fn chat_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_CHAT_ID_BYTES && !value.contains('\0')
}

/// Canonical signed SQLite rowid. Zero/negative values are valid legacy anchors.
pub fn chat_rowid(value: &str) -> bool {
    value
        .parse::<i64>()
        .is_ok_and(|rowid| rowid.to_string() == value)
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn budget(value: u32) -> bool {
    (1024..=MAX_CHAT_DATA_BYTES as u32).contains(&value)
}
fn preview(value: &str, total: u64, max: usize) -> bool {
    total <= MAX_SAFE && value.len() <= max && value.len() as u64 <= total
}

impl DesktopChatScope {
    pub fn valid(&self) -> bool {
        match self {
            Self::AllCatalog => true,
            Self::Exact { kind, scope_id } => {
                kind.len() <= 128 && scope_id.as_ref().is_none_or(|id| id.len() <= 8192)
            }
            Self::LikeChat {
                chat_id: id,
                generation,
            } => chat_id(id) && desktop_revision(generation),
        }
    }
}

impl DesktopChatHeader {
    pub fn valid(&self) -> bool {
        chat_id(&self.id)
            && chat_rowid(&self.rowid)
            && desktop_revision(&self.generation)
            && desktop_revision(&self.metadata_revision)
            && desktop_revision(&self.messages_revision)
            && desktop_revision(&self.erasure_revision)
            && preview(
                &self.title_preview,
                self.title_bytes,
                MAX_CHAT_PREVIEW_BYTES,
            )
            && preview(
                &self.scope_kind_preview,
                self.scope_kind_bytes,
                MAX_CHAT_PREVIEW_BYTES,
            )
            && match (&self.scope_id_preview, self.scope_id_bytes) {
                (None, None) => true,
                (Some(value), Some(total)) => preview(value, total, MAX_CHAT_PREVIEW_BYTES),
                _ => false,
            }
    }

    fn matches_scope(&self, scope: &DesktopChatScope) -> bool {
        match scope {
            DesktopChatScope::AllCatalog => true,
            DesktopChatScope::LikeChat {
                chat_id,
                generation,
            } => self.id != *chat_id || self.generation == *generation,
            DesktopChatScope::Exact { kind, scope_id } => {
                self.scope_kind_bytes == kind.len() as u64
                    && self.scope_kind_preview == *kind
                    && match (scope_id, &self.scope_id_preview, self.scope_id_bytes) {
                        (None, None, None) => true,
                        (Some(value), Some(prefix), Some(total)) => {
                            total == value.len() as u64 && value.starts_with(prefix)
                        }
                        _ => false,
                    }
            }
        }
    }
}

impl DesktopChatMessageHeader {
    pub fn valid(&self) -> bool {
        chat_id(&self.id)
            && chat_id(&self.chat_id)
            && chat_rowid(&self.rowid)
            && desktop_revision(&self.generation)
            && desktop_revision(&self.revision)
            && preview(
                &self.role_preview,
                self.role_bytes,
                MAX_CHAT_ROLE_PREVIEW_BYTES,
            )
            && self.content_bytes <= MAX_SAFE
            && self.citations_bytes <= MAX_SAFE
    }
}

impl DesktopChatListCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && desktop_revision(&self.catalog_revision)
            && hash(&self.query_hash)
            && chat_rowid(&self.after_rowid)
    }
}
impl DesktopChatMessagesCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && chat_id(&self.chat_id)
            && desktop_revision(&self.generation)
            && desktop_revision(&self.messages_revision)
            && hash(&self.query_hash)
            && chat_rowid(&self.after_rowid)
    }
}
impl DesktopChatFieldCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && chat_id(&self.chat_id)
            && desktop_revision(&self.generation)
            && desktop_revision(&self.metadata_revision)
            && self.present
            && self.byte_offset > 0
            && self.byte_offset < self.total_bytes
            && self.total_bytes <= MAX_SAFE
            && budget(self.max_bytes)
    }
}
impl DesktopChatMessageFieldCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && chat_id(&self.chat_id)
            && chat_id(&self.message_id)
            && desktop_revision(&self.chat_generation)
            && desktop_revision(&self.generation)
            && desktop_revision(&self.revision)
            && self.byte_offset > 0
            && self.byte_offset < self.total_bytes
            && self.total_bytes <= MAX_SAFE
            && budget(self.max_bytes)
    }
}

impl DesktopChatsRequest {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::List { .. } => "list",
            Self::Describe { .. } => "describe",
            Self::Messages { .. } => "messages",
            Self::ReadChatField { .. } => "read_chat_field",
            Self::ReadMessageField { .. } => "read_message_field",
        }
    }
    pub fn max_bytes(&self) -> usize {
        match self {
            Self::Describe { .. } => MAX_CHAT_DATA_BYTES,
            Self::List { max_bytes, .. }
            | Self::Messages { max_bytes, .. }
            | Self::ReadChatField { max_bytes, .. }
            | Self::ReadMessageField { max_bytes, .. } => *max_bytes as usize,
        }
    }
    pub fn valid(&self) -> bool {
        let valid = match self {
            Self::List {
                scope,
                limit,
                max_bytes,
                cursor,
            } => {
                scope.valid()
                    && (1..=MAX_CHAT_ITEMS).contains(limit)
                    && budget(*max_bytes)
                    && cursor.as_ref().is_none_or(DesktopChatListCursor::valid)
            }
            Self::Describe { chat_id: id } => chat_id(id),
            Self::Messages {
                chat_id: id,
                expected_generation,
                expected_messages_revision,
                limit,
                max_bytes,
                cursor,
            } => {
                chat_id(id)
                    && desktop_revision(expected_generation)
                    && desktop_revision(expected_messages_revision)
                    && (1..=MAX_CHAT_ITEMS).contains(limit)
                    && budget(*max_bytes)
                    && cursor.as_ref().is_none_or(|c| {
                        c.valid()
                            && c.chat_id == *id
                            && c.generation == *expected_generation
                            && c.messages_revision == *expected_messages_revision
                    })
            }
            Self::ReadChatField {
                chat_id: id,
                expected_generation,
                expected_metadata_revision,
                field,
                cursor,
                max_bytes,
            } => {
                chat_id(id)
                    && desktop_revision(expected_generation)
                    && desktop_revision(expected_metadata_revision)
                    && budget(*max_bytes)
                    && cursor.as_ref().is_none_or(|c| {
                        c.valid()
                            && c.chat_id == *id
                            && c.generation == *expected_generation
                            && c.metadata_revision == *expected_metadata_revision
                            && c.field == *field
                            && c.max_bytes == *max_bytes
                    })
            }
            Self::ReadMessageField {
                chat_id: id,
                message_id,
                expected_chat_generation,
                expected_generation,
                expected_revision,
                field,
                cursor,
                max_bytes,
            } => {
                chat_id(id)
                    && chat_id(message_id)
                    && desktop_revision(expected_chat_generation)
                    && desktop_revision(expected_generation)
                    && desktop_revision(expected_revision)
                    && budget(*max_bytes)
                    && cursor.as_ref().is_none_or(|c| {
                        c.valid()
                            && c.chat_id == *id
                            && c.message_id == *message_id
                            && c.chat_generation == *expected_chat_generation
                            && c.generation == *expected_generation
                            && c.revision == *expected_revision
                            && c.field == *field
                            && c.max_bytes == *max_bytes
                    })
            }
        };
        valid && desktop_body_fits(self, MAX_CHAT_REQUEST_BYTES)
    }
}

/// Every nonempty field page advances. An empty final fragment exists only for
/// a truly empty/absent field at offset zero, never at a nonzero phantom offset.
fn fragment_end(offset: u64, total: u64, text: &str) -> Option<u64> {
    if total > MAX_SAFE {
        return None;
    }
    let end = offset.checked_add(text.len() as u64)?;
    if (total == 0 && offset == 0 && text.is_empty())
        || (!text.is_empty() && offset < total && end <= total)
    {
        Some(end)
    } else {
        None
    }
}

impl DesktopChatsReply {
    pub fn valid_for(&self, request: &DesktopChatsRequest) -> bool {
        if !request.valid() || !desktop_body_fits(self, request.max_bytes()) {
            return false;
        }
        match (self, request) {
            (Self::Header(header), DesktopChatsRequest::Describe { chat_id }) => {
                header.valid() && header.id == *chat_id
            }
            (
                Self::List(page),
                DesktopChatsRequest::List {
                    scope,
                    limit,
                    cursor,
                    ..
                },
            ) => {
                let mut ids = HashSet::new();
                let mut rows = HashSet::new();
                desktop_scope(&page.library_scope)
                    && desktop_revision(&page.catalog_revision)
                    && page.items.len() <= *limit as usize
                    && (cursor.is_none() || !page.items.is_empty())
                    && page.items.iter().all(|h| {
                        h.valid()
                            && h.matches_scope(scope)
                            && ids.insert(&h.id)
                            && rows.insert(&h.rowid)
                            && cursor.as_ref().is_none_or(|c| c.after_rowid != h.rowid)
                    })
                    && cursor.as_ref().is_none_or(|c| {
                        c.library_scope == page.library_scope
                            && c.catalog_revision == page.catalog_revision
                    })
                    && page.next.as_ref().is_none_or(|c| {
                        c.valid()
                            && c.library_scope == page.library_scope
                            && c.catalog_revision == page.catalog_revision
                            && page.items.last().is_some_and(|h| h.rowid == c.after_rowid)
                            && cursor.as_ref().is_none_or(|old| {
                                old.query_hash == c.query_hash && old.after_rowid != c.after_rowid
                            })
                    })
            }
            (
                Self::Messages(page),
                DesktopChatsRequest::Messages {
                    chat_id,
                    expected_generation,
                    expected_messages_revision,
                    limit,
                    cursor,
                    ..
                },
            ) => {
                let mut ids = HashSet::new();
                let mut rows = HashSet::new();
                desktop_scope(&page.library_scope)
                    && page.chat_id == *chat_id
                    && page.generation == *expected_generation
                    && page.messages_revision == *expected_messages_revision
                    && page.items.len() <= *limit as usize
                    && (cursor.is_none() || !page.items.is_empty())
                    && page.items.iter().all(|h| {
                        h.valid()
                            && h.chat_id == *chat_id
                            && ids.insert(&h.id)
                            && rows.insert(&h.rowid)
                            && cursor.as_ref().is_none_or(|c| c.after_rowid != h.rowid)
                    })
                    && cursor
                        .as_ref()
                        .is_none_or(|c| c.library_scope == page.library_scope)
                    && page.next.as_ref().is_none_or(|c| {
                        c.valid()
                            && c.library_scope == page.library_scope
                            && c.chat_id == page.chat_id
                            && c.generation == page.generation
                            && c.messages_revision == page.messages_revision
                            && page.items.last().is_some_and(|h| h.rowid == c.after_rowid)
                            && cursor.as_ref().is_none_or(|old| {
                                old.query_hash == c.query_hash && old.after_rowid != c.after_rowid
                            })
                    })
            }
            (
                Self::ChatField(page),
                DesktopChatsRequest::ReadChatField {
                    chat_id,
                    expected_generation,
                    expected_metadata_revision,
                    field,
                    cursor,
                    max_bytes,
                },
            ) => {
                let Some(end) = fragment_end(page.byte_offset, page.total_bytes, &page.text) else {
                    return false;
                };
                desktop_scope(&page.library_scope)
                    && page.chat_id == *chat_id
                    && page.generation == *expected_generation
                    && page.metadata_revision == *expected_metadata_revision
                    && page.field == *field
                    && page.byte_offset == cursor.as_ref().map_or(0, |c| c.byte_offset)
                    && (page.present
                        || (page.field == DesktopChatField::ScopeId && page.total_bytes == 0))
                    && cursor.as_ref().is_none_or(|c| {
                        c.library_scope == page.library_scope
                            && c.total_bytes == page.total_bytes
                            && c.present == page.present
                    })
                    && match &page.next {
                        Some(c) => {
                            c.valid()
                                && end < page.total_bytes
                                && c.byte_offset == end
                                && c.library_scope == page.library_scope
                                && c.chat_id == page.chat_id
                                && c.generation == page.generation
                                && c.metadata_revision == page.metadata_revision
                                && c.field == page.field
                                && c.total_bytes == page.total_bytes
                                && c.present == page.present
                                && c.max_bytes == *max_bytes
                        }
                        None => end == page.total_bytes,
                    }
            }
            (
                Self::MessageField(page),
                DesktopChatsRequest::ReadMessageField {
                    chat_id,
                    message_id,
                    expected_chat_generation,
                    expected_generation,
                    expected_revision,
                    field,
                    cursor,
                    max_bytes,
                },
            ) => {
                let Some(end) = fragment_end(page.byte_offset, page.total_bytes, &page.text) else {
                    return false;
                };
                desktop_scope(&page.library_scope)
                    && page.chat_id == *chat_id
                    && page.message_id == *message_id
                    && page.chat_generation == *expected_chat_generation
                    && page.generation == *expected_generation
                    && page.revision == *expected_revision
                    && page.field == *field
                    && page.byte_offset == cursor.as_ref().map_or(0, |c| c.byte_offset)
                    && cursor.as_ref().is_none_or(|c| {
                        c.library_scope == page.library_scope && c.total_bytes == page.total_bytes
                    })
                    && match &page.next {
                        Some(c) => {
                            c.valid()
                                && end < page.total_bytes
                                && c.byte_offset == end
                                && c.library_scope == page.library_scope
                                && c.chat_id == page.chat_id
                                && c.message_id == page.message_id
                                && c.chat_generation == page.chat_generation
                                && c.generation == page.generation
                                && c.revision == page.revision
                                && c.field == page.field
                                && c.total_bytes == page.total_bytes
                                && c.max_bytes == *max_bytes
                        }
                        None => end == page.total_bytes,
                    }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "chats_tests.rs"]
mod tests;

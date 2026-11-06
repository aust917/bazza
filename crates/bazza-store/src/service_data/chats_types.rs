// Store-local mirror of the private chat read contract; no API dependency.
// Stored text and citation JSON remain opaque UTF-8.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const FIXTURE_CHATS_CAPABILITY: &str = "experimental_fixture_desktop_chats";
pub const MAX_CHAT_DATA_BYTES: usize = 65_024;
pub const MAX_CHAT_REQUEST_BYTES: usize = 65_024;
pub const MAX_CHAT_ITEMS: u16 = 100;
pub const MAX_CHAT_ID_BYTES: usize = 512;
pub const MAX_CHAT_PREVIEW_BYTES: usize = 256;
pub const MAX_CHAT_ROLE_PREVIEW_BYTES: usize = 128;

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatListCursor {
    pub library_scope: String,
    pub catalog_revision: String,
    pub query_hash: String,
    pub after_rowid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatMessagesCursor {
    pub library_scope: String,
    pub chat_id: String,
    pub generation: String,
    pub messages_revision: String,
    pub query_hash: String,
    pub after_rowid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesktopChatListPage {
    pub library_scope: String,
    pub catalog_revision: String,
    pub items: Vec<DesktopChatHeader>,
    pub next: Option<DesktopChatListCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    DesktopChatsRequest
);

//! Closed private GUI folders/tags boundary. No share tokens, voice vectors,
//! generic settings, SQL or file operations are represented here.
use crate::desktop::*;
use serde::{Deserialize, Serialize};

pub const FIXTURE_LIBRARY_CAPABILITY: &str = "experimental_fixture_desktop_library";

// Template identities are SQL values, including legacy Unicode/custom IDs.
// They are never recording paths and do not use the folder/meeting ID grammar.
fn template_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.contains('\0')
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopLibraryOperation {
    FolderCreate,
    FolderRename,
    FolderMove,
    FolderDelete,
    FolderSetTemplate,
    FolderUpdate,
    TagCreate,
    TagRename,
    TagDelete,
    MeetingMove,
    MeetingTag,
    MeetingUntag,
    MeetingTagNamed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopLibraryCursor {
    pub library_scope: String,
    pub snapshot_sequence: i64,
    pub query_hash: String,
    pub offset: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopLibraryPageRequest {
    pub cursor: Option<DesktopLibraryCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopLibraryItem {
    Folder {
        id: String,
        name: String,
        parent_id: Option<String>,
        default_template_id: Option<String>,
        meeting_count: i64,
        revision: String,
    },
    Tag {
        id: String,
        name: String,
        meeting_count: i64,
        revision: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopLibraryPage {
    pub library_scope: String,
    pub snapshot_sequence: i64,
    pub items: Vec<DesktopLibraryItem>,
    pub next: Option<DesktopLibraryCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopLibraryRequest {
    List {
        cursor: Option<DesktopLibraryCursor>,
        limit: u16,
        max_bytes: u32,
    },
    FolderCreate {
        name: String,
        parent_id: Option<String>,
        idempotency_key: String,
    },
    FolderRename {
        folder_id: String,
        name: String,
        expected_revision: String,
        idempotency_key: String,
    },
    FolderMove {
        folder_id: String,
        parent_id: Option<String>,
        expected_revision: String,
        idempotency_key: String,
    },
    FolderDelete {
        folder_id: String,
        expected_revision: String,
        idempotency_key: String,
    },
    FolderSetTemplate {
        folder_id: String,
        template_id: Option<String>,
        expected_revision: String,
        idempotency_key: String,
    },
    FolderUpdate {
        folder_id: String,
        name: String,
        parent_id: Option<String>,
        default_template_id: Option<String>,
        expected_revision: String,
        idempotency_key: String,
    },
    TagCreate {
        name: String,
        idempotency_key: String,
    },
    TagRename {
        tag_id: String,
        name: String,
        expected_revision: String,
        idempotency_key: String,
    },
    TagDelete {
        tag_id: String,
        expected_revision: String,
        idempotency_key: String,
    },
    MeetingMove {
        meeting_id: String,
        folder_id: Option<String>,
        expected_revision: String,
        idempotency_key: String,
    },
    MeetingTag {
        meeting_id: String,
        tag_id: String,
        expected_revision: String,
        idempotency_key: String,
    },
    MeetingUntag {
        meeting_id: String,
        tag_id: String,
        expected_revision: String,
        idempotency_key: String,
    },
    MeetingTagNamed {
        meeting_id: String,
        name: String,
        expected_revision: String,
        idempotency_key: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopLibraryMutationReply {
    pub operation: DesktopLibraryOperation,
    pub outcome: DesktopMutationOutcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopLibraryReply {
    Page(DesktopLibraryPage),
    Mutation(DesktopLibraryMutationReply),
}

impl DesktopLibraryRequest {
    pub fn operation(&self) -> Option<DesktopLibraryOperation> {
        match self {
            Self::List { .. } => None,
            Self::FolderCreate { .. } => Some(DesktopLibraryOperation::FolderCreate),
            Self::FolderRename { .. } => Some(DesktopLibraryOperation::FolderRename),
            Self::FolderMove { .. } => Some(DesktopLibraryOperation::FolderMove),
            Self::FolderDelete { .. } => Some(DesktopLibraryOperation::FolderDelete),
            Self::FolderSetTemplate { .. } => Some(DesktopLibraryOperation::FolderSetTemplate),
            Self::FolderUpdate { .. } => Some(DesktopLibraryOperation::FolderUpdate),
            Self::TagCreate { .. } => Some(DesktopLibraryOperation::TagCreate),
            Self::TagRename { .. } => Some(DesktopLibraryOperation::TagRename),
            Self::TagDelete { .. } => Some(DesktopLibraryOperation::TagDelete),
            Self::MeetingMove { .. } => Some(DesktopLibraryOperation::MeetingMove),
            Self::MeetingTag { .. } => Some(DesktopLibraryOperation::MeetingTag),
            Self::MeetingUntag { .. } => Some(DesktopLibraryOperation::MeetingUntag),
            Self::MeetingTagNamed { .. } => Some(DesktopLibraryOperation::MeetingTagNamed),
        }
    }
    pub fn idempotency_key(&self) -> Option<&str> {
        match self {
            Self::List { .. } => None,
            Self::FolderCreate {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::FolderRename {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::FolderMove {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::FolderDelete {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::FolderSetTemplate {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::FolderUpdate {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::TagCreate {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::TagRename {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::TagDelete {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::MeetingMove {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::MeetingTag {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::MeetingUntag {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::MeetingTagNamed {
                idempotency_key, ..
            } => Some(idempotency_key),
        }
    }
}

const MAX_SAFE: i64 = 9_007_199_254_740_991;
fn name_valid(value: &str, allow_empty: bool) -> bool {
    value.len() <= 512 && !value.contains('\0') && (allow_empty || !value.trim().is_empty())
}
fn key_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl DesktopLibraryRequest {
    pub fn target_id(&self) -> Option<&str> {
        match self {
            Self::List { .. } | Self::FolderCreate { .. } | Self::TagCreate { .. } => None,
            Self::FolderRename { folder_id, .. } => Some(folder_id),
            Self::FolderMove { folder_id, .. } => Some(folder_id),
            Self::FolderDelete { folder_id, .. } => Some(folder_id),
            Self::FolderSetTemplate { folder_id, .. } => Some(folder_id),
            Self::FolderUpdate { folder_id, .. } => Some(folder_id),
            Self::TagRename { tag_id, .. } => Some(tag_id),
            Self::TagDelete { tag_id, .. } => Some(tag_id),
            Self::MeetingMove { meeting_id, .. } => Some(meeting_id),
            Self::MeetingTag { meeting_id, .. } => Some(meeting_id),
            Self::MeetingUntag { meeting_id, .. } => Some(meeting_id),
            Self::MeetingTagNamed { meeting_id, .. } => Some(meeting_id),
        }
    }
    pub fn valid(&self) -> bool {
        match self {
            Self::List {
                cursor,
                limit,
                max_bytes,
            } => {
                (1..=100).contains(limit)
                    && (1024..=MAX_DESKTOP_DATA_BYTES).contains(max_bytes)
                    && cursor.as_ref().is_none_or(DesktopLibraryCursor::valid)
            }
            Self::FolderCreate {
                name,
                parent_id,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && name_valid(name, false)
                    && parent_id.as_ref().is_none_or(|id| desktop_id(id))
            }
            Self::FolderRename {
                folder_id,
                name,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(folder_id)
                    && name_valid(name, false)
                    && desktop_revision(expected_revision)
            }
            Self::FolderMove {
                folder_id,
                parent_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(folder_id)
                    && parent_id.as_ref().is_none_or(|id| desktop_id(id))
                    && desktop_revision(expected_revision)
            }
            Self::FolderDelete {
                folder_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(folder_id)
                    && desktop_revision(expected_revision)
            }
            Self::FolderSetTemplate {
                folder_id,
                template_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(folder_id)
                    && template_id.as_ref().is_none_or(|id| self::template_id(id))
                    && desktop_revision(expected_revision)
            }
            Self::FolderUpdate {
                folder_id,
                name,
                parent_id,
                default_template_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(folder_id)
                    && name_valid(name, false)
                    && parent_id.as_ref().is_none_or(|id| desktop_id(id))
                    && default_template_id
                        .as_ref()
                        .is_none_or(|id| template_id(id))
                    && desktop_revision(expected_revision)
            }
            Self::TagCreate {
                name,
                idempotency_key,
            } => key_valid(idempotency_key) && name_valid(name, false),
            Self::TagRename {
                tag_id,
                name,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(tag_id)
                    && name_valid(name, false)
                    && desktop_revision(expected_revision)
            }
            Self::TagDelete {
                tag_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(tag_id)
                    && desktop_revision(expected_revision)
            }
            Self::MeetingMove {
                meeting_id,
                folder_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(meeting_id)
                    && folder_id.as_ref().is_none_or(|id| desktop_id(id))
                    && desktop_revision(expected_revision)
            }
            Self::MeetingTag {
                meeting_id,
                tag_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(meeting_id)
                    && desktop_id(tag_id)
                    && desktop_revision(expected_revision)
            }
            Self::MeetingUntag {
                meeting_id,
                tag_id,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(meeting_id)
                    && desktop_id(tag_id)
                    && desktop_revision(expected_revision)
            }
            Self::MeetingTagNamed {
                meeting_id,
                name,
                expected_revision,
                idempotency_key,
            } => {
                key_valid(idempotency_key)
                    && desktop_id(meeting_id)
                    && name_valid(name, false)
                    && desktop_revision(expected_revision)
            }
        }
    }
}
impl DesktopLibraryCursor {
    pub fn valid(&self) -> bool {
        desktop_scope(&self.library_scope)
            && (0..=MAX_SAFE).contains(&self.snapshot_sequence)
            && hash_valid(&self.query_hash)
            && self.offset <= MAX_SAFE as u64
    }
}
impl DesktopLibraryPageRequest {
    pub fn valid(&self) -> bool {
        (1..=100).contains(&self.limit)
            && (1024..=MAX_DESKTOP_DATA_BYTES).contains(&self.max_bytes)
            && self.cursor.as_ref().is_none_or(DesktopLibraryCursor::valid)
    }
}
impl DesktopLibraryItem {
    pub fn key(&self) -> (u8, &str, &str) {
        match self {
            Self::Folder { id, name, .. } => (0, name, id),
            Self::Tag { id, name, .. } => (1, name, id),
        }
    }
    pub fn valid(&self) -> bool {
        match self {
            Self::Folder {
                id,
                name,
                parent_id,
                default_template_id,
                meeting_count,
                revision,
            } => {
                desktop_id(id)
                    && name_valid(name, true)
                    && desktop_revision(revision)
                    && parent_id.as_ref().is_none_or(|id| desktop_id(id))
                    && default_template_id
                        .as_ref()
                        .is_none_or(|id| template_id(id))
                    && (0..=MAX_SAFE).contains(meeting_count)
            }
            Self::Tag {
                id,
                name,
                meeting_count,
                revision,
            } => {
                desktop_id(id)
                    && name_valid(name, true)
                    && desktop_revision(revision)
                    && (0..=MAX_SAFE).contains(meeting_count)
            }
        }
    }
}
impl DesktopLibraryPage {
    pub fn valid_for(&self, request: &DesktopLibraryPageRequest) -> bool {
        #[derive(Serialize)]
        struct Body<'a> {
            kind: &'static str,
            data: &'a DesktopLibraryPage,
        }
        if !request.valid()
            || !desktop_scope(&self.library_scope)
            || !(0..=MAX_SAFE).contains(&self.snapshot_sequence)
            || self.items.len() > usize::from(request.limit)
            || !self.items.iter().all(DesktopLibraryItem::valid)
            || !self
                .items
                .windows(2)
                .all(|items| items[0].key() < items[1].key())
            || !desktop_body_fits(
                &Body {
                    kind: "page",
                    data: self,
                },
                request.max_bytes as usize,
            )
        {
            return false;
        }
        let offset = request.cursor.as_ref().map_or(0, |cursor| cursor.offset);
        let mut seen = std::collections::HashSet::new();
        if self
            .items
            .iter()
            .any(|item| !seen.insert((item.key().0, item.key().2)))
            || (self.items.is_empty() && offset != 0)
        {
            return false;
        }
        if request.cursor.as_ref().is_some_and(|cursor| {
            cursor.library_scope != self.library_scope
                || cursor.snapshot_sequence != self.snapshot_sequence
        }) {
            return false;
        }
        self.next.as_ref().is_none_or(|next| {
            next.valid()
                && !self.items.is_empty()
                && next.library_scope == self.library_scope
                && next.snapshot_sequence == self.snapshot_sequence
                && Some(next.offset) == offset.checked_add(self.items.len() as u64)
                && request
                    .cursor
                    .as_ref()
                    .is_none_or(|cursor| cursor.query_hash == next.query_hash)
        })
    }
}

impl DesktopLibraryReply {
    pub fn valid_for(&self, request: &DesktopLibraryRequest) -> bool {
        if !request.valid() {
            return false;
        }
        match (self, request) {
            (
                Self::Page(page),
                DesktopLibraryRequest::List {
                    cursor,
                    limit,
                    max_bytes,
                },
            ) => page.valid_for(&DesktopLibraryPageRequest {
                cursor: cursor.clone(),
                limit: *limit,
                max_bytes: *max_bytes,
            }),
            (Self::Mutation(reply), _) => {
                request.valid()
                    && request.operation() == Some(reply.operation)
                    && desktop_id(&reply.outcome.entity_id)
                    && request
                        .target_id()
                        .is_none_or(|id| id == reply.outcome.entity_id)
                    && desktop_revision(&reply.outcome.revision)
                    && (1..=MAX_SAFE).contains(&reply.outcome.change_sequence)
                    && reply.outcome.job_id.is_none()
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn template_assignment_and_projection_preserve_custom_unicode_sql_ids() {
        let id = format!("  Клиент/{}\" ", "🙂".repeat(40));
        let mut request = DesktopLibraryRequest::FolderSetTemplate {
            folder_id: "folder".into(),
            template_id: Some(id.clone()),
            expected_revision: "revision-1".into(),
            idempotency_key: "assign-template".into(),
        };
        assert!(request.valid());
        let mut item = folder("folder", "Project");
        if let DesktopLibraryItem::Folder {
            default_template_id,
            ..
        } = &mut item
        {
            *default_template_id = Some(id);
        }
        assert!(item.valid());
        if let DesktopLibraryRequest::FolderSetTemplate { template_id, .. } = &mut request {
            *template_id = Some("bad\0id".into());
        }
        assert!(!request.valid());
    }

    fn rename() -> DesktopLibraryRequest {
        DesktopLibraryRequest::FolderRename {
            folder_id: "folder".into(),
            name: "Renamed".into(),
            expected_revision: "revision-old".into(),
            idempotency_key: "key".into(),
        }
    }
    fn folder(id: &str, name: &str) -> DesktopLibraryItem {
        DesktopLibraryItem::Folder {
            id: id.into(),
            name: name.into(),
            parent_id: None,
            default_template_id: None,
            meeting_count: 2,
            revision: "revision".into(),
        }
    }
    fn mutation(operation: DesktopLibraryOperation) -> DesktopLibraryReply {
        DesktopLibraryReply::Mutation(DesktopLibraryMutationReply {
            operation,
            outcome: DesktopMutationOutcome {
                entity_id: "folder".into(),
                revision: "revision-new".into(),
                change_sequence: 7,
                job_id: None,
                dedup_until: "2099-01-01T00:00:00Z".parse().unwrap(),
            },
        })
    }

    #[test]
    fn closed_operations_reject_unknown_fields_names_and_unsafe_ids() {
        let mut request = serde_json::to_value(rename()).unwrap();
        request["principal"] = "untrusted".into();
        assert!(serde_json::from_value::<DesktopLibraryRequest>(request).is_err());
        assert!(serde_json::from_value::<DesktopLibraryRequest>(
            json!({"op":"execute_sql","sql":"SELECT 1"})
        )
        .is_err());
        let request = DesktopLibraryRequest::FolderUpdate {
            folder_id: "folder".into(),
            name: "Name".into(),
            parent_id: None,
            default_template_id: Some("custom".into()),
            expected_revision: "old".into(),
            idempotency_key: "update".into(),
        };
        assert!(request.valid());
        let mut request = serde_json::to_value(request).unwrap();
        request["parent_id"] = "../outside".into();
        assert!(!serde_json::from_value::<DesktopLibraryRequest>(request)
            .unwrap()
            .valid());
        let mut request = serde_json::to_value(rename()).unwrap();
        request["name"] = " \n\t ".into();
        assert!(!serde_json::from_value::<DesktopLibraryRequest>(request)
            .unwrap()
            .valid());
    }

    #[test]
    fn mutation_validation_binds_suboperation_target_and_no_job_result() {
        assert!(mutation(DesktopLibraryOperation::FolderRename).valid_for(&rename()));
        assert!(!mutation(DesktopLibraryOperation::TagRename).valid_for(&rename()));
        let mut reply = mutation(DesktopLibraryOperation::FolderRename);
        if let DesktopLibraryReply::Mutation(reply) = &mut reply {
            reply.outcome.entity_id = "different".into();
        }
        assert!(!reply.valid_for(&rename()));
        let mut reply = mutation(DesktopLibraryOperation::FolderRename);
        if let DesktopLibraryReply::Mutation(reply) = &mut reply {
            reply.outcome.job_id = Some("unexpected-job".into());
        }
        assert!(!reply.valid_for(&rename()));
    }

    #[test]
    fn whole_page_budget_and_total_items_order_and_ids_are_validated() {
        let request = DesktopLibraryPageRequest {
            cursor: None,
            limit: 100,
            max_bytes: 1024,
        };
        let mut page = DesktopLibraryPage {
            library_scope: "a".repeat(32),
            snapshot_sequence: 7,
            items: vec![folder("folder", "Name")],
            next: None,
        };
        assert!(page.valid_for(&request));
        page.items = vec![folder("folder", &format!("z{}", "\u{1}".repeat(500)))];
        assert!(!page.valid_for(&request));
        page.items = vec![folder("same", "A"), folder("same", "B")];
        assert!(!page.valid_for(&request));
        page.items = vec![folder("b", "B"), folder("a", "A")];
        assert!(!page.valid_for(&request));
        page.items = vec![folder("a", "A"), folder("b", "B")];
        assert!(!page.valid_for(&DesktopLibraryPageRequest {
            limit: 1,
            ..request
        }));
    }

    #[test]
    fn continuation_validation_binds_scope_sequence_query_and_advancing_offset() {
        let cursor = DesktopLibraryCursor {
            library_scope: "a".repeat(32),
            snapshot_sequence: 7,
            query_hash: "b".repeat(64),
            offset: 1,
        };
        let mut request = DesktopLibraryPageRequest {
            cursor: Some(cursor.clone()),
            limit: 20,
            max_bytes: 4096,
        };
        let mut page = DesktopLibraryPage {
            library_scope: cursor.library_scope.clone(),
            snapshot_sequence: 7,
            items: vec![folder("a", "A")],
            next: Some(DesktopLibraryCursor {
                offset: 2,
                ..cursor
            }),
        };
        assert!(page.valid_for(&request));
        request.cursor.as_mut().unwrap().query_hash = "c".repeat(64);
        assert!(!page.valid_for(&request));
        request.cursor.as_mut().unwrap().query_hash = "b".repeat(64);
        page.next.as_mut().unwrap().offset = 1;
        assert!(!page.valid_for(&request));
        page.next.as_mut().unwrap().offset = 2;
        page.library_scope = "d".repeat(32);
        assert!(!page.valid_for(&request));
    }
}

//! Closed private GUI organization operations. Reads are bounded and never
//! initialize projection state. Mutations preserve unrelated capture metadata.
use super::*;
use crate::Store;
use chrono::Utc;
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureLibraryKind {
    All,
    Folders,
    Tags,
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
pub enum DesktopLibraryMutation {
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

impl DesktopLibraryMutation {
    pub fn operation(&self) -> DesktopLibraryOperation {
        match self {
            Self::FolderCreate { .. } => DesktopLibraryOperation::FolderCreate,
            Self::FolderRename { .. } => DesktopLibraryOperation::FolderRename,
            Self::FolderMove { .. } => DesktopLibraryOperation::FolderMove,
            Self::FolderDelete { .. } => DesktopLibraryOperation::FolderDelete,
            Self::FolderSetTemplate { .. } => DesktopLibraryOperation::FolderSetTemplate,
            Self::FolderUpdate { .. } => DesktopLibraryOperation::FolderUpdate,
            Self::TagCreate { .. } => DesktopLibraryOperation::TagCreate,
            Self::TagRename { .. } => DesktopLibraryOperation::TagRename,
            Self::TagDelete { .. } => DesktopLibraryOperation::TagDelete,
            Self::MeetingMove { .. } => DesktopLibraryOperation::MeetingMove,
            Self::MeetingTag { .. } => DesktopLibraryOperation::MeetingTag,
            Self::MeetingUntag { .. } => DesktopLibraryOperation::MeetingUntag,
            Self::MeetingTagNamed { .. } => DesktopLibraryOperation::MeetingTagNamed,
        }
    }
    pub fn idempotency_key(&self) -> &str {
        match self {
            Self::FolderCreate {
                idempotency_key, ..
            } => idempotency_key,
            Self::FolderRename {
                idempotency_key, ..
            } => idempotency_key,
            Self::FolderMove {
                idempotency_key, ..
            } => idempotency_key,
            Self::FolderDelete {
                idempotency_key, ..
            } => idempotency_key,
            Self::FolderSetTemplate {
                idempotency_key, ..
            } => idempotency_key,
            Self::FolderUpdate {
                idempotency_key, ..
            } => idempotency_key,
            Self::TagCreate {
                idempotency_key, ..
            } => idempotency_key,
            Self::TagRename {
                idempotency_key, ..
            } => idempotency_key,
            Self::TagDelete {
                idempotency_key, ..
            } => idempotency_key,
            Self::MeetingMove {
                idempotency_key, ..
            } => idempotency_key,
            Self::MeetingTag {
                idempotency_key, ..
            } => idempotency_key,
            Self::MeetingUntag {
                idempotency_key, ..
            } => idempotency_key,
            Self::MeetingTagNamed {
                idempotency_key, ..
            } => idempotency_key,
        }
    }
}

const LIBRARY_BYTES: u32 = 65_024;
const MAX_SAFE: i64 = 9_007_199_254_740_991;

/// Explicit bootstrap hook, never called by a read. Normal service startup
/// enrolls legacy rows once before exposing private organization operations.
pub(super) fn initialize_library_on(tx: &Transaction<'_>) -> DataResult<()> {
    tx.execute("INSERT OR IGNORE INTO service_folder_state SELECT id,lower(hex(randomblob(16))) FROM folders", [])?;
    tx.execute(
        "INSERT OR IGNORE INTO service_tag_state SELECT id,lower(hex(randomblob(16))) FROM tags",
        [],
    )?;
    Ok(())
}

fn valid_revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}
fn library_snapshot(conn: &Connection) -> DataResult<(String, i64)> {
    let scope: String = conn.query_row(
        "SELECT substr(cursor_scope,1,33) FROM service_search_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if scope.len() != 32
        || !scope
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(DataError::Invalid("library cursor scope"));
    }
    let sequence: i64 = conn.query_row("SELECT MAX(COALESCE((SELECT MAX(sequence) FROM service_changes),0),COALESCE((SELECT seq FROM sqlite_sequence WHERE name='service_changes'),0))", [], |row| row.get(0))?;
    if !(0..=MAX_SAFE).contains(&sequence) {
        return Err(DataError::Invalid("library sequence"));
    }
    Ok((scope, sequence))
}
fn body_fits<T: Serialize>(value: &T, limit: usize) -> bool {
    struct Counter(usize, usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|used| *used <= self.1)
                .ok_or_else(|| std::io::Error::other("library response budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0, limit), value).is_ok()
}
fn page_fits(page: &DesktopLibraryPage, limit: u32) -> bool {
    #[derive(Serialize)]
    struct Body<'a> {
        kind: &'static str,
        data: &'a DesktopLibraryPage,
    }
    body_fits(
        &Body {
            kind: "page",
            data: page,
        },
        limit as usize,
    )
}

// Check lengths/types inside SQLite before any legacy string enters Rust.
const LIBRARY_ROWS: &str = r#"
SELECT 0 AS kind,
CASE WHEN typeof(f.id)='text' AND length(CAST(f.id AS BLOB)) BETWEEN 1 AND 128 THEN f.id END AS id,
CASE WHEN typeof(f.name)='text' AND length(CAST(f.name AS BLOB))<=512 THEN f.name END AS name,
CASE WHEN length(CAST(f.parent_id AS BLOB))<=128 THEN f.parent_id END AS parent_id,
CASE WHEN length(CAST(f.default_template_id AS BLOB))<=512 THEN f.default_template_id END AS default_template_id,
(SELECT COUNT(*) FROM meetings m LEFT JOIN service_meeting_state s ON s.meeting_id=m.id WHERE m.folder_id=f.id AND COALESCE(s.tombstoned,0)=0) AS meeting_count,
CASE WHEN typeof(fs.revision)='text' AND length(CAST(fs.revision AS BLOB)) BETWEEN 1 AND 128 THEN fs.revision END AS revision,
((f.parent_id IS NULL OR (typeof(f.parent_id)='text' AND length(CAST(f.parent_id AS BLOB)) BETWEEN 1 AND 128)) AND
 (f.default_template_id IS NULL OR (typeof(f.default_template_id)='text' AND length(CAST(f.default_template_id AS BLOB)) BETWEEN 1 AND 512))) AS valid,
f.name AS sort_name,f.id AS sort_id
FROM folders f LEFT JOIN service_folder_state fs ON fs.folder_id=f.id
WHERE ?3 IS NULL OR ?3=0
UNION ALL
SELECT 1,
CASE WHEN typeof(t.id)='text' AND length(CAST(t.id AS BLOB)) BETWEEN 1 AND 128 THEN t.id END,
CASE WHEN typeof(t.name)='text' AND length(CAST(t.name AS BLOB))<=512 THEN t.name END,
NULL,NULL,
(SELECT COUNT(*) FROM meeting_tags mt JOIN meetings m ON m.id=mt.meeting_id LEFT JOIN service_meeting_state s ON s.meeting_id=m.id WHERE mt.tag_id=t.id AND COALESCE(s.tombstoned,0)=0),
CASE WHEN typeof(ts.revision)='text' AND length(CAST(ts.revision AS BLOB)) BETWEEN 1 AND 128 THEN ts.revision END,
1,t.name,t.id FROM tags t LEFT JOIN service_tag_state ts ON ts.tag_id=t.id
WHERE ?3 IS NULL OR ?3=1
ORDER BY kind,sort_name,sort_id LIMIT ?1 OFFSET ?2
"#;

fn library_item(row: &rusqlite::Row<'_>) -> DataResult<DesktopLibraryItem> {
    let bad = || DataError::Invalid("legacy library projection");
    let read = |index: usize| row.get::<_, String>(index).map_err(|_| bad());
    let kind: i64 = row.get(0).map_err(|_| bad())?;
    let id = read(1)?;
    let name = read(2)?;
    let revision = read(6)?;
    let count: i64 = row.get(5).map_err(|_| bad())?;
    let valid: bool = row.get(7).map_err(|_| bad())?;
    if !valid || !valid_revision(&revision) || !(0..=MAX_SAFE).contains(&count) {
        return Err(bad());
    }
    safe_id(&id)?;
    bounded_text(&name, 0, 512)?;
    match kind {
        0 => {
            let parent_id: Option<String> = row.get(3).map_err(|_| bad())?;
            let default_template_id: Option<String> = row.get(4).map_err(|_| bad())?;
            if let Some(id) = &parent_id {
                safe_id(id)?;
            }
            if let Some(id) = &default_template_id {
                bounded_text(id, 1, 512)?;
            }
            Ok(DesktopLibraryItem::Folder {
                id,
                name,
                parent_id,
                default_template_id,
                meeting_count: count,
                revision,
            })
        }
        1 => Ok(DesktopLibraryItem::Tag {
            id,
            name,
            meeting_count: count,
            revision,
        }),
        _ => Err(bad()),
    }
}

impl Store {
    pub fn service_desktop_library_page(
        &self,
        request: &DesktopLibraryPageRequest,
    ) -> DataResult<DesktopLibraryPage> {
        self.library_page(request, None)
    }

    pub fn fixture_read_library_structure(
        &self,
        kind: FixtureLibraryKind,
        request: &DesktopLibraryPageRequest,
    ) -> DataResult<DesktopLibraryPage> {
        self.library_page(request, Some(kind))
    }

    fn library_page(
        &self,
        request: &DesktopLibraryPageRequest,
        fixture_kind: Option<FixtureLibraryKind>,
    ) -> DataResult<DesktopLibraryPage> {
        if !(1..=100).contains(&request.limit)
            || !(1024..=LIBRARY_BYTES).contains(&request.max_bytes)
        {
            return Err(DataError::Invalid("library page budget"));
        }
        let (query, kind) = match fixture_kind {
            None => ("library-v1", None),
            Some(FixtureLibraryKind::All) => ("fixture-library-all-v1", None),
            Some(FixtureLibraryKind::Folders) => ("fixture-library-folders-v1", Some(0)),
            Some(FixtureLibraryKind::Tags) => ("fixture-library-tags-v1", Some(1)),
        };
        let query_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                query,
                request.limit,
                request.max_bytes
            ))?)
        );
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (scope, sequence) = library_snapshot(&tx)?;
        let offset = match &request.cursor {
            None => 0,
            Some(cursor)
                if cursor.library_scope == scope
                    && cursor.snapshot_sequence == sequence
                    && cursor.query_hash == query_hash
                    && cursor.offset <= MAX_SAFE as u64 =>
            {
                cursor.offset
            }
            Some(_) => return Err(DataError::StaleCursor),
        };
        let mut page = DesktopLibraryPage {
            library_scope: scope.clone(),
            snapshot_sequence: sequence,
            items: Vec::new(),
            next: None,
        };
        let next = |len: usize| DesktopLibraryCursor {
            library_scope: scope.clone(),
            snapshot_sequence: sequence,
            query_hash: query_hash.clone(),
            offset: offset + len as u64,
        };
        {
            let mut statement = tx.prepare(LIBRARY_ROWS)?;
            let mut rows =
                statement.query(params![i64::from(request.limit) + 1, offset as i64, kind])?;
            let mut more = false;
            while page.items.len() < usize::from(request.limit) {
                let Some(row) = rows.next()? else {
                    break;
                };
                let item = match library_item(row) {
                    Ok(item) => item,
                    Err(DataError::Invalid(_)) if !page.items.is_empty() => {
                        more = true;
                        break;
                    }
                    Err(error) => return Err(error),
                };
                page.items.push(item);
                page.next = Some(next(page.items.len()));
                if !page_fits(&page, request.max_bytes) {
                    let item = page.items.pop().expect("just pushed library item");
                    if page.items.is_empty() && rows.next()?.is_none() {
                        page.items.push(item);
                        page.next = None;
                        if page_fits(&page, request.max_bytes) {
                            break;
                        }
                    }
                    if page.items.is_empty() {
                        return Err(DataError::Invalid("library row exceeds page budget"));
                    }
                    more = true;
                    break;
                }
            }
            if page.items.len() == usize::from(request.limit) && !more {
                more = rows.next()?.is_some();
            }
            page.next = more.then(|| next(page.items.len()));
            if page.items.is_empty() && offset != 0 {
                return Err(DataError::StaleCursor);
            }
            if !page_fits(&page, request.max_bytes) {
                return Err(DataError::Invalid("library response budget"));
            }
        }
        tx.commit()?;
        Ok(page)
    }
}

fn entity_revision(conn: &Connection, kind: &str, id: &str) -> DataResult<String> {
    safe_id(id)?;
    let sql = match kind {
        "folder" => "SELECT CASE WHEN length(CAST(s.revision AS BLOB)) BETWEEN 1 AND 128 THEN s.revision END FROM folders f LEFT JOIN service_folder_state s ON s.folder_id=f.id WHERE f.id=?1",
        "tag" => "SELECT CASE WHEN length(CAST(s.revision AS BLOB)) BETWEEN 1 AND 128 THEN s.revision END FROM tags t LEFT JOIN service_tag_state s ON s.tag_id=t.id WHERE t.id=?1",
        _ => return Err(DataError::Invalid("library entity kind")),
    };
    let value: Option<Option<String>> = conn.query_row(sql, [id], |row| row.get(0)).optional()?;
    match value {
        None => Err(DataError::NotFound),
        Some(Some(value)) if valid_revision(&value) => Ok(value),
        Some(_) => Err(DataError::Invalid("library projection not initialized")),
    }
}
fn expect_entity(tx: &Transaction<'_>, kind: &str, id: &str, expected: &str) -> DataResult<()> {
    if !valid_revision(expected) {
        return Err(DataError::Invalid("library revision"));
    }
    check_revision(&entity_revision(tx, kind, id)?, expected)
}
fn set_entity_revision(tx: &Transaction<'_>, kind: &str, id: &str) -> DataResult<String> {
    let sql = match kind {
        "folder" => "UPDATE service_folder_state SET revision=?2 WHERE folder_id=?1",
        "tag" => "UPDATE service_tag_state SET revision=?2 WHERE tag_id=?1",
        _ => return Err(DataError::Invalid("library entity kind")),
    };
    let revision = revision();
    if tx.execute(sql, params![id, revision])? != 1 {
        return Err(DataError::Invalid("library projection not initialized"));
    }
    Ok(revision)
}
fn named(value: &str) -> DataResult<&str> {
    bounded_text(value, 1, 512)?;
    let value = value.trim();
    bounded_text(value, 1, 512)?;
    Ok(value)
}
fn template_exists(conn: &Connection, id: Option<&str>) -> DataResult<()> {
    if let Some(id) = id {
        bounded_text(id, 1, 512)?;
        if !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM templates WHERE id=?1)",
            [id],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(DataError::NotFound);
        }
    }
    Ok(())
}
fn parent_valid(conn: &Connection, id: &str, parent: Option<&str>) -> DataResult<()> {
    let Some(parent) = parent else {
        return Ok(());
    };
    entity_revision(conn, "folder", parent)?;
    let (self_cycle, missing, root): (bool, bool, bool) = conn.query_row(
        "WITH RECURSIVE ancestors(id) AS (SELECT ?1 UNION SELECT f.parent_id FROM folders f JOIN ancestors a ON a.id=f.id WHERE f.parent_id IS NOT NULL) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?2),EXISTS(SELECT 1 FROM ancestors a LEFT JOIN folders f ON f.id=a.id WHERE f.id IS NULL),EXISTS(SELECT 1 FROM ancestors a JOIN folders f ON f.id=a.id WHERE f.parent_id IS NULL)",
        params![parent,id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    )?;
    if self_cycle || missing || !root {
        return Err(DataError::Invalid("folder cycle or invalid ancestry"));
    }
    Ok(())
}
fn expect_meeting(tx: &Transaction<'_>, id: &str, expected: &str) -> DataResult<()> {
    safe_id(id)?;
    if !valid_revision(expected) {
        return Err(DataError::Invalid("meeting metadata revision"));
    }
    visible_meeting(tx, id)?;
    let revision: String = tx.query_row(
        "SELECT metadata_revision FROM service_meeting_state WHERE meeting_id=?1",
        [id],
        |row| row.get(0),
    )?;
    check_revision(&revision, expected)
}
fn meeting_revision(tx: &Transaction<'_>, id: &str) -> DataResult<String> {
    let revision = revision();
    if tx.execute(
        "UPDATE service_meeting_state SET metadata_revision=?2 WHERE meeting_id=?1",
        params![id, revision],
    )? != 1
    {
        return Err(DataError::Invalid("meeting projection not initialized"));
    }
    // Associations do not change any source body. Invalidate retrieval cursors
    // and update only the title document's metadata revision, without loading
    // an entire transcript or rebuilding its raw/clean representations.
    tx.execute("UPDATE service_search_documents SET revision=?2 WHERE source_kind='title' AND entity_id=?1", params![id,revision])?;
    tx.execute(
        "UPDATE service_search_state SET generation=generation+1 WHERE singleton=1",
        [],
    )?;
    Ok(revision)
}
fn tag_by_name(tx: &Transaction<'_>, name: &str) -> DataResult<(String, bool)> {
    let name = named(name)?;
    let found: Option<String> = tx.query_row("SELECT CASE WHEN length(CAST(id AS BLOB)) BETWEEN 1 AND 128 THEN id END FROM tags WHERE name=?1 COLLATE NOCASE", [name], |row| row.get(0)).optional()?;
    if let Some(id) = found {
        entity_revision(tx, "tag", &id)?;
        return Ok((id, false));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let revision = revision();
    tx.execute("INSERT INTO tags(id,name) VALUES(?1,?2)", params![id, name])?;
    tx.execute(
        "INSERT INTO service_tag_state VALUES(?1,?2)",
        params![id, revision],
    )?;
    Ok((id, true))
}

fn batch_ids(tx: &Transaction<'_>, sql: &str, id: &str) -> DataResult<Vec<String>> {
    let mut statement = tx.prepare(sql)?;
    let rows = statement
        .query_map([id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &rows {
        safe_id(id)?;
    }
    Ok(rows)
}
fn delete_folder_on(tx: &Transaction<'_>, id: &str, ctx: &MutationContext) -> DataResult<()> {
    let (parent, valid): (Option<String>, bool) = tx.query_row(
        "SELECT CASE WHEN length(CAST(parent_id AS BLOB))<=128 THEN parent_id END,parent_id IS NULL OR (typeof(parent_id)='text' AND length(CAST(parent_id AS BLOB)) BETWEEN 1 AND 128) FROM folders WHERE id=?1",
        [id], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    if !valid {
        return Err(DataError::Invalid("folder parent"));
    }
    parent_valid(tx, id, parent.as_deref())?;
    loop {
        let ids = batch_ids(tx, "SELECT CASE WHEN length(CAST(id AS BLOB)) BETWEEN 1 AND 128 THEN id END FROM meetings WHERE folder_id=?1 ORDER BY id LIMIT 100", id)?;
        if ids.is_empty() {
            break;
        }
        for meeting in ids {
            tx.execute(
                "UPDATE meetings SET folder_id=?2 WHERE id=?1",
                params![meeting, parent],
            )?;
            let revision = meeting_revision(tx, &meeting)?;
            consistency::change(tx, "meeting", &meeting, &revision, &ctx.principal)?;
        }
    }
    loop {
        let ids = batch_ids(tx, "SELECT CASE WHEN length(CAST(id AS BLOB)) BETWEEN 1 AND 128 THEN id END FROM folders WHERE parent_id=?1 ORDER BY id LIMIT 100", id)?;
        if ids.is_empty() {
            break;
        }
        for child in ids {
            tx.execute(
                "UPDATE folders SET parent_id=?2 WHERE id=?1",
                params![child, parent],
            )?;
            let revision = set_entity_revision(tx, "folder", &child)?;
            consistency::change(tx, "folder", &child, &revision, &ctx.principal)?;
        }
    }
    tx.execute("DELETE FROM folders WHERE id=?1", [id])?;
    Ok(())
}
fn delete_tag_on(tx: &Transaction<'_>, id: &str, ctx: &MutationContext) -> DataResult<()> {
    loop {
        let ids = batch_ids(tx, "SELECT CASE WHEN length(CAST(meeting_id AS BLOB)) BETWEEN 1 AND 128 THEN meeting_id END FROM meeting_tags WHERE tag_id=?1 ORDER BY meeting_id LIMIT 100", id)?;
        if ids.is_empty() {
            break;
        }
        for meeting in ids {
            tx.execute(
                "DELETE FROM meeting_tags WHERE meeting_id=?1 AND tag_id=?2",
                params![meeting, id],
            )?;
            let revision = meeting_revision(tx, &meeting)?;
            consistency::change(tx, "meeting", &meeting, &revision, &ctx.principal)?;
        }
    }
    tx.execute("DELETE FROM tags WHERE id=?1", [id])?;
    Ok(())
}

impl Store {
    pub fn service_desktop_library_mutate(
        &self,
        ctx: &MutationContext,
        input: &DesktopLibraryMutation,
    ) -> DataResult<MutationOutcome> {
        if input.idempotency_key() != ctx.idempotency_key {
            return Err(DataError::Invalid("library request key"));
        }
        let tool = match input.operation() {
            DesktopLibraryOperation::FolderCreate => "desktop_folder_create",
            DesktopLibraryOperation::FolderRename => "desktop_folder_rename",
            DesktopLibraryOperation::FolderMove => "desktop_folder_move",
            DesktopLibraryOperation::FolderDelete => "desktop_folder_delete",
            DesktopLibraryOperation::FolderSetTemplate => "desktop_folder_set_template",
            DesktopLibraryOperation::FolderUpdate => "desktop_folder_update",
            DesktopLibraryOperation::TagCreate => "desktop_tag_create",
            DesktopLibraryOperation::TagRename => "desktop_tag_rename",
            DesktopLibraryOperation::TagDelete => "desktop_tag_delete",
            DesktopLibraryOperation::MeetingMove => "desktop_meeting_move",
            DesktopLibraryOperation::MeetingTag => "desktop_meeting_tag",
            DesktopLibraryOperation::MeetingUntag => "desktop_meeting_untag",
            DesktopLibraryOperation::MeetingTagNamed => "desktop_meeting_tag_named",
        };
        consistency::mutate(self, ctx, tool, input, |tx| {
            match input {
                DesktopLibraryMutation::FolderCreate {
                    name, parent_id, ..
                } => {
                    let name = named(name)?;
                    let id = uuid::Uuid::new_v4().to_string();
                    parent_valid(tx, &id, parent_id.as_deref())?;
                    let revision = revision();
                    tx.execute(
                        "INSERT INTO folders(id,name,parent_id,created_at) VALUES(?1,?2,?3,?4)",
                        params![id, name, parent_id, Utc::now()],
                    )?;
                    tx.execute(
                        "INSERT INTO service_folder_state VALUES(?1,?2)",
                        params![id, revision],
                    )?;
                    consistency::outcome(tx, "folder", &id, &revision, ctx)
                }
                DesktopLibraryMutation::FolderRename {
                    folder_id,
                    name,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "folder", folder_id, expected_revision)?;
                    tx.execute(
                        "UPDATE folders SET name=?2 WHERE id=?1",
                        params![folder_id, named(name)?],
                    )?;
                    let revision = set_entity_revision(tx, "folder", folder_id)?;
                    consistency::outcome(tx, "folder", folder_id, &revision, ctx)
                }
                DesktopLibraryMutation::FolderMove {
                    folder_id,
                    parent_id,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "folder", folder_id, expected_revision)?;
                    parent_valid(tx, folder_id, parent_id.as_deref())?;
                    tx.execute(
                        "UPDATE folders SET parent_id=?2 WHERE id=?1",
                        params![folder_id, parent_id],
                    )?;
                    let revision = set_entity_revision(tx, "folder", folder_id)?;
                    consistency::outcome(tx, "folder", folder_id, &revision, ctx)
                }
                DesktopLibraryMutation::FolderSetTemplate {
                    folder_id,
                    template_id,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "folder", folder_id, expected_revision)?;
                    template_exists(tx, template_id.as_deref())?;
                    tx.execute(
                        "UPDATE folders SET default_template_id=?2 WHERE id=?1",
                        params![folder_id, template_id],
                    )?;
                    let revision = set_entity_revision(tx, "folder", folder_id)?;
                    consistency::outcome(tx, "folder", folder_id, &revision, ctx)
                }
                DesktopLibraryMutation::FolderUpdate {
                    folder_id,
                    name,
                    parent_id,
                    default_template_id,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "folder", folder_id, expected_revision)?;
                    let name = named(name)?;
                    parent_valid(tx, folder_id, parent_id.as_deref())?;
                    template_exists(tx, default_template_id.as_deref())?;
                    tx.execute("UPDATE folders SET name=?2,parent_id=?3,default_template_id=?4 WHERE id=?1", params![folder_id,name,parent_id,default_template_id])?;
                    let revision = set_entity_revision(tx, "folder", folder_id)?;
                    consistency::outcome(tx, "folder", folder_id, &revision, ctx)
                }
                DesktopLibraryMutation::FolderDelete {
                    folder_id,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "folder", folder_id, expected_revision)?;
                    delete_folder_on(tx, folder_id, ctx)?;
                    consistency::outcome(tx, "folder", folder_id, &revision(), ctx)
                }
                DesktopLibraryMutation::TagCreate { name, .. } => {
                    let (id, _) = tag_by_name(tx, name)?;
                    consistency::outcome(tx, "tag", &id, &entity_revision(tx, "tag", &id)?, ctx)
                }
                DesktopLibraryMutation::TagRename {
                    tag_id,
                    name,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "tag", tag_id, expected_revision)?;
                    let name = named(name)?;
                    let duplicate: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM tags WHERE name=?1 COLLATE NOCASE AND id!=?2)",
                        params![name, tag_id],
                        |row| row.get(0),
                    )?;
                    if duplicate {
                        return Err(DataError::Invalid("duplicate tag name"));
                    }
                    tx.execute("UPDATE tags SET name=?2 WHERE id=?1", params![tag_id, name])?;
                    let revision = set_entity_revision(tx, "tag", tag_id)?;
                    consistency::outcome(tx, "tag", tag_id, &revision, ctx)
                }
                DesktopLibraryMutation::TagDelete {
                    tag_id,
                    expected_revision,
                    ..
                } => {
                    expect_entity(tx, "tag", tag_id, expected_revision)?;
                    delete_tag_on(tx, tag_id, ctx)?;
                    consistency::outcome(tx, "tag", tag_id, &revision(), ctx)
                }
                DesktopLibraryMutation::MeetingMove {
                    meeting_id,
                    folder_id,
                    expected_revision,
                    ..
                } => {
                    expect_meeting(tx, meeting_id, expected_revision)?;
                    if let Some(id) = folder_id {
                        entity_revision(tx, "folder", id)?;
                    }
                    tx.execute(
                        "UPDATE meetings SET folder_id=?2 WHERE id=?1",
                        params![meeting_id, folder_id],
                    )?;
                    let revision = meeting_revision(tx, meeting_id)?;
                    consistency::outcome(tx, "meeting", meeting_id, &revision, ctx)
                }
                DesktopLibraryMutation::MeetingTag {
                    meeting_id,
                    tag_id,
                    expected_revision,
                    ..
                }
                | DesktopLibraryMutation::MeetingUntag {
                    meeting_id,
                    tag_id,
                    expected_revision,
                    ..
                } => {
                    expect_meeting(tx, meeting_id, expected_revision)?;
                    entity_revision(tx, "tag", tag_id)?;
                    if matches!(input, DesktopLibraryMutation::MeetingTag { .. }) {
                        tx.execute(
                            "INSERT OR IGNORE INTO meeting_tags(meeting_id,tag_id) VALUES(?1,?2)",
                            params![meeting_id, tag_id],
                        )?;
                    } else {
                        tx.execute(
                            "DELETE FROM meeting_tags WHERE meeting_id=?1 AND tag_id=?2",
                            params![meeting_id, tag_id],
                        )?;
                        if tx.execute("DELETE FROM tags WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM meeting_tags WHERE tag_id=?1)", [tag_id])? != 0 {
                            consistency::change(tx, "tag", tag_id, &revision(), &ctx.principal)?;
                        }
                    }
                    let revision = meeting_revision(tx, meeting_id)?;
                    consistency::outcome(tx, "meeting", meeting_id, &revision, ctx)
                }
                DesktopLibraryMutation::MeetingTagNamed {
                    meeting_id,
                    name,
                    expected_revision,
                    ..
                } => {
                    // Check the meeting CAS before creating a name. Conflict must
                    // not leave a detached tag from a partial two-call update.
                    expect_meeting(tx, meeting_id, expected_revision)?;
                    let (tag, created) = tag_by_name(tx, name)?;
                    if created {
                        consistency::change(
                            tx,
                            "tag",
                            &tag,
                            &entity_revision(tx, "tag", &tag)?,
                            &ctx.principal,
                        )?;
                    }
                    tx.execute(
                        "INSERT OR IGNORE INTO meeting_tags(meeting_id,tag_id) VALUES(?1,?2)",
                        params![meeting_id, tag],
                    )?;
                    let revision = meeting_revision(tx, meeting_id)?;
                    consistency::outcome(tx, "meeting", meeting_id, &revision, ctx)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Store {
        let store = Store::open_in_memory().unwrap();
        if !store
            .conn
            .lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='service_folder_state')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        {
            store
                .conn
                .lock()
                .execute_batch(include_str!("library_state.sql"))
                .unwrap();
        }
        store.service_rebuild_search().unwrap();
        store
    }
    fn call(store: &Store, key: &str, mut body: serde_json::Value) -> DataResult<MutationOutcome> {
        body["idempotency_key"] = key.into();
        let input = serde_json::from_value(body).unwrap();
        store.service_desktop_library_mutate(
            &MutationContext {
                principal: "desktop".into(),
                idempotency_key: key.into(),
            },
            &input,
        )
    }
    fn folder(store: &Store, key: &str, name: &str, parent: Option<&str>) -> MutationOutcome {
        call(
            store,
            key,
            json!({"op":"folder_create","name":name,"parent_id":parent}),
        )
        .unwrap()
    }
    fn rev(store: &Store, kind: &str, id: &str) -> String {
        entity_revision(&store.conn.lock(), kind, id).unwrap()
    }
    fn page(
        store: &Store,
        cursor: Option<DesktopLibraryCursor>,
        limit: u16,
        bytes: u32,
    ) -> DataResult<DesktopLibraryPage> {
        store.service_desktop_library_page(&DesktopLibraryPageRequest {
            cursor,
            limit,
            max_bytes: bytes,
        })
    }
    fn meeting(store: &Store, key: &str) -> String {
        store
            .service_create_text_meeting(
                &MutationContext {
                    principal: "desktop".into(),
                    idempotency_key: key.into(),
                },
                &NewTextMeeting {
                    title: format!("Meeting {key}"),
                    started_at: Utc::now(),
                    participants: vec!["Анна".into()],
                    folder_id: None,
                    tag_ids: Vec::new(),
                    calendar_uid: None,
                    transcript: Some(vec![crate::NewSegment {
                        start_ms: 0,
                        end_ms: 1000,
                        speaker: Some("Анна".into()),
                        text: " Original raw текст ".into(),
                    }]),
                },
            )
            .unwrap()
            .entity_id
    }
    fn metadata(store: &Store, id: &str) -> String {
        store
            .service_meeting(id)
            .unwrap()
            .unwrap()
            .metadata_revision
    }

    #[test]
    fn explicit_legacy_enrollment_and_event_pruning_preserve_durable_revisions() {
        let store = fixture();
        let old = store.create_folder("Legacy folder", None).unwrap();
        assert!(matches!(
            page(&store, None, 100, LIBRARY_BYTES),
            Err(DataError::Invalid(_))
        ));
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_folder_state", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            initialize_library_on(&tx).unwrap();
            tx.commit().unwrap();
        }
        let before = rev(&store, "folder", &old.id);
        let changed = call(&store, "rename-legacy", json!({"op":"folder_rename","folder_id":old.id,"name":"Renamed","expected_revision":before})).unwrap();
        store
            .conn
            .lock()
            .execute("DELETE FROM service_changes", [])
            .unwrap();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            initialize_library_on(&tx).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(rev(&store, "folder", &old.id), changed.revision);
        let after = page(&store, None, 100, LIBRARY_BYTES).unwrap();
        assert_eq!(after.snapshot_sequence, changed.change_sequence);
        assert!(
            matches!(&after.items[0], DesktopLibraryItem::Folder { revision, .. } if revision == &changed.revision)
        );
    }

    #[test]
    fn mixed_structure_pages_are_byte_bounded_and_bind_scope_sequence_and_query() {
        let store = fixture();
        for index in 0..45 {
            folder(
                &store,
                &format!("folder-{index}"),
                &format!("Folder {index:03}"),
                None,
            );
            call(
                &store,
                &format!("tag-{index}"),
                json!({"op":"tag_create","name":format!("Tag {index:03}")}),
            )
            .unwrap();
        }
        let first = page(&store, None, 7, 2048).unwrap();
        let cursor = first.next.clone().unwrap();
        let mut foreign = cursor.clone();
        foreign.library_scope = "0".repeat(32);
        assert!(matches!(
            page(&store, Some(foreign), 7, 2048),
            Err(DataError::StaleCursor)
        ));
        assert!(matches!(
            page(&store, Some(cursor.clone()), 8, 2048),
            Err(DataError::StaleCursor)
        ));
        assert!(matches!(
            page(&store, Some(cursor.clone()), 7, 4096),
            Err(DataError::StaleCursor)
        ));
        let mut ids = Vec::new();
        let mut next = None;
        loop {
            let current = page(&store, next, 7, 2048).unwrap();
            assert!(current.items.len() <= 7);
            assert!(
                serde_json::to_vec(&json!({"kind":"page","data":&current}))
                    .unwrap()
                    .len()
                    <= 2048
            );
            for item in &current.items {
                ids.push(match item {
                    DesktopLibraryItem::Folder { id, .. } | DesktopLibraryItem::Tag { id, .. } => {
                        id.clone()
                    }
                });
            }
            next = current.next;
            if next.is_none() {
                break;
            }
        }
        assert_eq!(ids.len(), 90);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 90);
        folder(&store, "late-folder", "Late", None);
        assert!(matches!(
            page(&store, Some(cursor), 7, 2048),
            Err(DataError::StaleCursor)
        ));
    }

    #[test]
    fn pages_do_not_map_unused_invalid_lookahead_or_skip_an_over_budget_row() {
        let store = fixture();
        folder(&store, "first", "A", None);
        let huge = folder(&store, "huge", "z", None);
        store
            .conn
            .lock()
            .execute(
                "UPDATE folders SET name=?2 WHERE id=?1",
                params![huge.entity_id, "z".repeat(1_000_000)],
            )
            .unwrap();
        let first = page(&store, None, 1, LIBRARY_BYTES).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.next.as_ref().unwrap().offset, 1);
        assert!(matches!(
            page(&store, first.next, 1, LIBRARY_BYTES),
            Err(DataError::Invalid(_))
        ));
        let prefix = page(&store, None, 100, LIBRARY_BYTES).unwrap();
        assert_eq!(prefix.items.len(), 1);
        assert_eq!(prefix.next.unwrap().offset, 1);
        store
            .conn
            .lock()
            .execute(
                "UPDATE folders SET name=?2 WHERE id=?1",
                params![huge.entity_id, format!("z{}", "\u{1}".repeat(500))],
            )
            .unwrap();
        let prefix = page(&store, None, 100, 1024).unwrap();
        assert_eq!(prefix.items.len(), 1);
        assert!(matches!(
            page(&store, prefix.next, 100, 1024),
            Err(DataError::Invalid(_))
        ));
    }

    #[test]
    fn folder_cas_replay_atomic_update_default_templates_and_cycles() {
        let store = fixture();
        let root = folder(&store, "root", " Root ", None);
        let child = folder(&store, "child", "Child", Some(&root.entity_id));
        store
            .upsert_template(&crate::Template {
                id: "custom".into(),
                name: "Custom".into(),
                description: String::new(),
                body: "Body".into(),
                builtin: false,
            })
            .unwrap();
        let input = json!({"op":"folder_update","folder_id":root.entity_id,"name":" Updated ","parent_id":null,"default_template_id":"custom","expected_revision":root.revision});
        let updated = call(&store, "atomic-update", input.clone()).unwrap();
        assert_eq!(
            store
                .list_folders()
                .unwrap()
                .into_iter()
                .find(|folder| folder.id == root.entity_id)
                .unwrap()
                .name,
            "Updated"
        );
        assert_eq!(call(&store, "atomic-update", input).unwrap(), updated);
        assert!(matches!(
            call(
                &store,
                "stale",
                json!({"op":"folder_rename","folder_id":root.entity_id,"name":"Stale","expected_revision":root.revision})
            ),
            Err(DataError::Conflict { .. })
        ));
        assert!(call(&store, "atomic-update", json!({"op":"folder_rename","folder_id":root.entity_id,"name":"Changed tool","expected_revision":updated.revision})).is_ok());
        let current = rev(&store, "folder", &root.entity_id);
        assert!(matches!(
            call(
                &store,
                "cycle",
                json!({"op":"folder_move","folder_id":root.entity_id,"parent_id":child.entity_id,"expected_revision":current})
            ),
            Err(DataError::Invalid(_))
        ));
        assert!(matches!(
            call(
                &store,
                "self",
                json!({"op":"folder_move","folder_id":root.entity_id,"parent_id":root.entity_id,"expected_revision":current})
            ),
            Err(DataError::Invalid(_))
        ));
        assert!(matches!(
            call(
                &store,
                "missing-template",
                json!({"op":"folder_set_template","folder_id":root.entity_id,"template_id":"missing","expected_revision":current})
            ),
            Err(DataError::NotFound)
        ));
        let saved = store
            .list_folders()
            .unwrap()
            .into_iter()
            .find(|folder| folder.id == root.entity_id)
            .unwrap();
        assert_eq!(saved.default_template_id.as_deref(), Some("custom"));
        assert_eq!(saved.parent_id, None);
        let renamed = json!({"op":"folder_rename","folder_id":child.entity_id,"name":"Renamed","expected_revision":child.revision});
        call(&store, "rename-child", renamed.clone()).unwrap();
        let mut different = renamed;
        different["name"] = "Different".into();
        assert!(matches!(
            call(&store, "rename-child", different),
            Err(DataError::IdempotencyConflict)
        ));
    }

    #[test]
    fn folder_delete_reparents_only_direct_children_and_rolls_back_every_revision_on_failure() {
        let store = fixture();
        let parent = folder(&store, "parent", "Parent", None);
        let target = folder(&store, "target", "Target", Some(&parent.entity_id));
        let child = folder(&store, "child", "Child", Some(&target.entity_id));
        let grandchild = folder(&store, "grandchild", "Grandchild", Some(&child.entity_id));
        let other = folder(&store, "other", "Other", None);
        let direct = meeting(&store, "direct");
        let nested = meeting(&store, "nested");
        for (key, id, folder_id) in [
            ("move-direct", &direct, &target.entity_id),
            ("move-nested", &nested, &child.entity_id),
        ] {
            call(&store, key, json!({"op":"meeting_move","meeting_id":id,"folder_id":folder_id,"expected_revision":metadata(&store,id)})).unwrap();
        }
        store.conn.lock().execute("UPDATE meetings SET status='recording',duration_ms=12345,has_video=1,error='preserved capture warning' WHERE id=?1", [&direct]).unwrap();
        let original_card = store.get_meeting(&direct).unwrap().unwrap();
        let speaker = crate::Speaker {
            id: "saved-voice".into(),
            name: "Saved voice".into(),
            embedding: vec![0.6, 0.8],
        };
        store.upsert_speaker(&speaker).unwrap();
        let profiles = store.list_voice_profiles().unwrap();
        let samples = store.list_voice_samples(&speaker.id).unwrap();
        let direct_before = store.service_meeting(&direct).unwrap().unwrap();
        let nested_before = metadata(&store, &nested);
        let raw_before = store.segments(&direct).unwrap();
        let count = store.service_changes(0, 100).unwrap().len();
        store.conn.lock().execute_batch(&format!("CREATE TRIGGER fail_library_delete BEFORE INSERT ON service_changes WHEN new.entity_kind='folder' AND new.entity_id='{}' BEGIN SELECT RAISE(ABORT,'injected folder deletion failure'); END;",target.entity_id)).unwrap();
        let input = json!({"op":"folder_delete","folder_id":target.entity_id,"expected_revision":target.revision});
        assert!(matches!(
            call(&store, "delete-failed", input.clone()),
            Err(DataError::Sqlite(_))
        ));
        assert_eq!(rev(&store, "folder", &child.entity_id), child.revision);
        assert_eq!(metadata(&store, &direct), direct_before.metadata_revision);
        assert_eq!(
            store
                .get_meeting(&direct)
                .unwrap()
                .unwrap()
                .folder_id
                .as_deref(),
            Some(target.entity_id.as_str())
        );
        assert_eq!(store.service_changes(0, 100).unwrap().len(), count);
        assert_eq!(
            store
                .conn
                .lock()
                .query_row(
                    "SELECT COUNT(*) FROM service_idempotency WHERE key='delete-failed'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER fail_library_delete")
            .unwrap();
        let deleted = call(&store, "delete-ok", input).unwrap();
        assert_ne!(rev(&store, "folder", &child.entity_id), child.revision);
        assert_eq!(
            rev(&store, "folder", &grandchild.entity_id),
            grandchild.revision
        );
        assert_eq!(rev(&store, "folder", &other.entity_id), other.revision);
        assert_eq!(rev(&store, "folder", &parent.entity_id), parent.revision);
        assert_eq!(
            store
                .list_folders()
                .unwrap()
                .into_iter()
                .find(|folder| folder.id == child.entity_id)
                .unwrap()
                .parent_id
                .as_deref(),
            Some(parent.entity_id.as_str())
        );
        assert_eq!(
            store
                .get_meeting(&direct)
                .unwrap()
                .unwrap()
                .folder_id
                .as_deref(),
            Some(parent.entity_id.as_str())
        );
        assert_ne!(metadata(&store, &direct), direct_before.metadata_revision);
        assert_eq!(metadata(&store, &nested), nested_before);
        assert_eq!(store.segments(&direct).unwrap(), raw_before);
        assert_eq!(
            store
                .service_meeting(&direct)
                .unwrap()
                .unwrap()
                .lifecycle_revision,
            direct_before.lifecycle_revision
        );
        let mut expected_card = original_card;
        expected_card.folder_id = Some(parent.entity_id.clone());
        assert_eq!(store.get_meeting(&direct).unwrap().unwrap(), expected_card);
        assert_eq!(store.list_voice_profiles().unwrap(), profiles);
        assert_eq!(store.list_voice_samples(&speaker.id).unwrap(), samples);
        assert!(store
            .service_changes(0, 100)
            .unwrap()
            .iter()
            .any(|change| change.entity_id == child.entity_id && change.entity_kind == "folder"));
        assert_eq!(deleted.entity_id, target.entity_id);
    }

    #[test]
    fn named_tags_are_atomic_nocase_and_last_untag_and_global_delete_are_scoped() {
        let store = fixture();
        let first = meeting(&store, "first");
        let second = meeting(&store, "second");
        let unrelated = meeting(&store, "unrelated");
        let before = metadata(&store, &first);
        assert!(matches!(
            call(
                &store,
                "stale-name",
                json!({"op":"meeting_tag_named","meeting_id":first,"name":"Must not exist","expected_revision":"stale"})
            ),
            Err(DataError::Conflict { .. })
        ));
        assert!(store.list_tags().unwrap().is_empty());
        call(&store, "name-first", json!({"op":"meeting_tag_named","meeting_id":first,"name":" Budget ","expected_revision":before})).unwrap();
        let tag = store.list_tags().unwrap().pop().unwrap();
        call(&store, "name-second", json!({"op":"meeting_tag_named","meeting_id":second,"name":"budget","expected_revision":metadata(&store,&second)})).unwrap();
        assert_eq!(store.list_tags().unwrap().len(), 1);
        assert_eq!(store.list_tags().unwrap()[0].name, "Budget");
        assert_eq!(store.list_tags().unwrap()[0].meeting_count, 2);
        let reused = call(
            &store,
            "existing",
            json!({"op":"tag_create","name":"BUDGET"}),
        )
        .unwrap();
        assert_eq!(reused.entity_id, tag.id);
        let other = call(
            &store,
            "other-tag",
            json!({"op":"tag_create","name":"Other"}),
        )
        .unwrap();
        assert!(matches!(
            call(
                &store,
                "duplicate",
                json!({"op":"tag_rename","tag_id":other.entity_id,"name":"budget","expected_revision":other.revision})
            ),
            Err(DataError::Invalid(_))
        ));
        call(&store, "untag-first", json!({"op":"meeting_untag","meeting_id":first,"tag_id":tag.id,"expected_revision":metadata(&store,&first)})).unwrap();
        assert!(store
            .list_tags()
            .unwrap()
            .iter()
            .any(|row| row.id == tag.id));
        let unaffected = metadata(&store, &unrelated);
        let affected = metadata(&store, &second);
        call(&store, "delete-tag", json!({"op":"tag_delete","tag_id":tag.id,"expected_revision":rev(&store,"tag",&tag.id)})).unwrap();
        assert_ne!(metadata(&store, &second), affected);
        assert_eq!(metadata(&store, &unrelated), unaffected);
        assert!(store.meeting_tags(&second).unwrap().is_empty());
        call(&store, "other-attach", json!({"op":"meeting_tag","meeting_id":first,"tag_id":other.entity_id,"expected_revision":metadata(&store,&first)})).unwrap();
        call(&store, "last-untag", json!({"op":"meeting_untag","meeting_id":first,"tag_id":other.entity_id,"expected_revision":metadata(&store,&first)})).unwrap();
        assert!(store.list_tags().unwrap().is_empty());
    }

    #[test]
    fn deletion_revisions_cover_multiple_bounded_batches_of_children_and_meetings() {
        let store = fixture();
        let target = folder(&store, "batch-target", "Target", None);
        let mut children = Vec::new();
        let mut meetings = Vec::new();
        for index in 0..101 {
            children.push(folder(
                &store,
                &format!("child-{index}"),
                &format!("Child {index}"),
                Some(&target.entity_id),
            ));
            let id = meeting(&store, &format!("meeting-{index}"));
            call(&store, &format!("move-{index}"), json!({"op":"meeting_move","meeting_id":id,"folder_id":target.entity_id,"expected_revision":metadata(&store,&id)})).unwrap();
            meetings.push((id.clone(), metadata(&store, &id)));
        }
        call(&store, "delete-batches", json!({"op":"folder_delete","folder_id":target.entity_id,"expected_revision":target.revision})).unwrap();
        for child in children {
            assert_ne!(rev(&store, "folder", &child.entity_id), child.revision);
        }
        for (id, revision) in meetings {
            assert_ne!(metadata(&store, &id), revision);
            assert!(store.get_meeting(&id).unwrap().unwrap().folder_id.is_none());
        }
        assert!(store
            .list_folders()
            .unwrap()
            .iter()
            .all(|folder| folder.parent_id.is_none()));
    }
}

//! Private, bounded chat reads. Schema v17 passively observes the existing SQL
//! writers; ordinary native startup remains v9 and gains no RPC write path.
include!("chats_types.rs");

use super::{check_revision, DataError, DataResult};
use crate::Store;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

const JS_SAFE: u64 = 9_007_199_254_740_991;

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.contains('\0')
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn rowid(value: &str) -> Option<i64> {
    let parsed = value.parse::<i64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}
fn count(value: i64) -> DataResult<u64> {
    u64::try_from(value)
        .ok()
        .filter(|n| *n <= JS_SAFE)
        .ok_or(DataError::Invalid("chat stored byte count"))
}
fn fits(value: &impl Serialize, max: usize) -> bool {
    struct Budget(usize, usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= self.1)
                .ok_or_else(|| std::io::Error::other("chat byte budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(0, max), value).is_ok()
}
fn budget(value: u32) -> DataResult<usize> {
    if !(1024..=MAX_CHAT_DATA_BYTES as u32).contains(&value) {
        return Err(DataError::Invalid("chat byte budget"));
    }
    Ok(value as usize)
}
fn hash(value: &impl Serialize) -> DataResult<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn scope_on(conn: &Connection) -> DataResult<String> {
    let scope: String = conn.query_row(
        "SELECT substr(cursor_scope,1,33) FROM service_search_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if scope.len() != 32
        || !scope
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(DataError::Invalid("chat library scope"));
    }
    Ok(scope)
}
fn catalog_on(conn: &Connection) -> DataResult<String> {
    let revision: String = conn.query_row(
        "SELECT substr(revision,1,129) FROM service_chat_catalog WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if !token(&revision) {
        return Err(DataError::Invalid("chat catalog revision"));
    }
    Ok(revision)
}
fn prefix(bytes: &[u8], max: usize) -> DataResult<String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()])
            .map_err(|_| DataError::Invalid("chat stored UTF-8"))?,
        Err(_) => return Err(DataError::Invalid("chat stored UTF-8")),
    };
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(text[..end].into())
}
fn date(value: Option<String>) -> DataResult<DateTime<Utc>> {
    let value = value.ok_or(DataError::Invalid("chat date budget"))?;
    // Match the legacy Store's pinned rusqlite decoder, including naive SQLite
    // timestamps. Cursor comparisons still use the untouched SQL TEXT value.
    <DateTime<Utc> as rusqlite::types::FromSql>::column_result(rusqlite::types::ValueRef::Text(
        value.as_bytes(),
    ))
    .map_err(|_| DataError::Invalid("chat stored date"))
}

fn chat_header_on(conn: &Connection, id: &str) -> DataResult<DesktopChatHeader> {
    if !identifier(id) {
        return Err(DataError::Invalid("chat identifier"));
    }
    let saved=conn.query_row(
        "SELECT c.rowid,substr(s.generation,1,129),substr(s.metadata_revision,1,129),substr(s.messages_revision,1,129),substr(s.erasure_revision,1,129),s.title_bytes,s.scope_kind_bytes,s.scope_id_bytes,COALESCE(substr(CAST(c.title AS BLOB),1,259),X''),COALESCE(substr(CAST(c.scope_kind AS BLOB),1,259),X''),CASE WHEN c.scope_id IS NULL THEN NULL ELSE COALESCE(substr(CAST(c.scope_id AS BLOB),1,259),X'') END,CASE WHEN length(CAST(c.created_at AS BLOB))<=128 THEN c.created_at END FROM chats c JOIN service_chat_state s ON s.chat_id=c.id AND s.state='present' WHERE c.id=?1",
        [id],|r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<i64>>(7)?,r.get::<_,Vec<u8>>(8)?,r.get::<_,Vec<u8>>(9)?,r.get::<_,Option<Vec<u8>>>(10)?,r.get::<_,Option<String>>(11)?)))
        .optional()?;
    let saved = match saved {
        Some(saved) => saved,
        None => {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM chats WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            return Err(if exists {
                DataError::IndexUnavailable
            } else {
                DataError::NotFound
            });
        }
    };
    let (
        rowid,
        generation,
        metadata_revision,
        messages_revision,
        erasure_revision,
        title_bytes,
        scope_kind_bytes,
        scope_id_bytes,
        title,
        kind,
        scope_id,
        created_at,
    ) = saved;
    for revision in [
        &generation,
        &metadata_revision,
        &messages_revision,
        &erasure_revision,
    ] {
        if !token(revision) {
            return Err(DataError::Invalid("chat stored revision"));
        }
    }
    let header = DesktopChatHeader {
        id: id.into(),
        rowid: rowid.to_string(),
        generation,
        metadata_revision,
        messages_revision,
        erasure_revision,
        title_preview: prefix(&title, 256)?,
        scope_kind_preview: prefix(&kind, 256)?,
        scope_id_preview: scope_id.as_deref().map(|v| prefix(v, 256)).transpose()?,
        title_bytes: count(title_bytes)?,
        scope_kind_bytes: count(scope_kind_bytes)?,
        scope_id_bytes: scope_id_bytes.map(count).transpose()?,
        created_at: date(created_at)?,
    };
    if header.title_preview.len() as u64 > header.title_bytes
        || header.scope_kind_preview.len() as u64 > header.scope_kind_bytes
        || header.scope_id_preview.is_some() != header.scope_id_bytes.is_some()
        || header
            .scope_id_preview
            .as_ref()
            .zip(header.scope_id_bytes)
            .is_some_and(|(v, n)| v.len() as u64 > n)
    {
        return Err(DataError::Invalid("chat header counts"));
    }
    Ok(header)
}
fn message_header_on(
    conn: &Connection,
    chat_id: &str,
    id: &str,
) -> DataResult<DesktopChatMessageHeader> {
    if !identifier(id) {
        return Err(DataError::Invalid("chat message identifier"));
    }
    let saved=conn.query_row(
        "SELECT m.rowid,substr(s.generation,1,129),substr(s.revision,1,129),s.role_bytes,s.content_bytes,s.citations_bytes,COALESCE(substr(CAST(m.role AS BLOB),1,131),X''),CASE WHEN length(CAST(m.created_at AS BLOB))<=128 THEN m.created_at END FROM chat_messages m JOIN service_chat_message_state s ON s.message_id=m.id AND s.chat_id=m.chat_id AND s.state='present' WHERE m.id=?1 AND m.chat_id=?2",
        params![id,chat_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,Vec<u8>>(6)?,r.get::<_,Option<String>>(7)?)))
        .optional()?;
    let saved = match saved {
        Some(saved) => saved,
        None => {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id=?1 AND chat_id=?2)",
                params![id, chat_id],
                |r| r.get(0),
            )?;
            return Err(if exists {
                DataError::IndexUnavailable
            } else {
                DataError::NotFound
            });
        }
    };
    let (rowid, generation, revision, role_bytes, content_bytes, citations_bytes, role, created_at) =
        saved;
    if !token(&generation) || !token(&revision) {
        return Err(DataError::Invalid("message stored revision"));
    }
    let header = DesktopChatMessageHeader {
        id: id.into(),
        chat_id: chat_id.into(),
        rowid: rowid.to_string(),
        generation,
        revision,
        role_preview: prefix(&role, 128)?,
        role_bytes: count(role_bytes)?,
        content_bytes: count(content_bytes)?,
        citations_bytes: count(citations_bytes)?,
        created_at: date(created_at)?,
    };
    if header.role_preview.len() as u64 > header.role_bytes {
        return Err(DataError::Invalid("chat message counts"));
    }
    Ok(header)
}

// The query uses only a closed, caller-independent SQL fragment. LikeChat keeps
// arbitrarily large legacy scope keys entirely inside SQLite.
fn selection(scope: &DesktopChatScope) -> &'static str {
    match scope {
        DesktopChatScope::AllCatalog => "1",
        DesktopChatScope::Exact {..} => "c.scope_kind=?1 AND c.scope_id IS ?2",
        DesktopChatScope::LikeChat {..} => "c.scope_kind=(SELECT scope_kind FROM chats WHERE id=?3) AND c.scope_id IS (SELECT scope_id FROM chats WHERE id=?3)",
    }
}
fn selection_arguments<'a>(
    conn: &Connection,
    scope: &'a DesktopChatScope,
) -> DataResult<(Option<&'a str>, Option<&'a str>, Option<&'a str>)> {
    match scope {
        DesktopChatScope::AllCatalog => Ok((None, None, None)),
        DesktopChatScope::Exact { kind, scope_id }
            if kind.len() <= 128 && scope_id.as_ref().is_none_or(|v| v.len() <= 8192) =>
        {
            Ok((Some(kind), scope_id.as_deref(), None))
        }
        DesktopChatScope::LikeChat {
            chat_id,
            generation,
        } if identifier(chat_id) && token(generation) => {
            let header = chat_header_on(conn, chat_id)?;
            check_revision(&header.generation, generation)?;
            Ok((None, None, Some(chat_id)))
        }
        _ => Err(DataError::Invalid("chat scope")),
    }
}

fn list_on(
    conn: &Connection,
    scope: &DesktopChatScope,
    limit: u16,
    max_bytes: u32,
    cursor: &Option<DesktopChatListCursor>,
) -> DataResult<DesktopChatsReply> {
    let max = budget(max_bytes)?;
    if !(1..=100).contains(&limit) {
        return Err(DataError::Invalid("chat page limit"));
    }
    let library_scope = scope_on(conn)?;
    let catalog_revision = catalog_on(conn)?;
    let query_hash = hash(&("chat_list_v1", scope, limit, max_bytes))?;
    let (kind, scope_id, like_id) = selection_arguments(conn, scope)?;
    let after = if let Some(cursor) = cursor {
        if cursor.library_scope != library_scope
            || cursor.catalog_revision != catalog_revision
            || cursor.query_hash != query_hash
        {
            return Err(DataError::StaleCursor);
        }
        Some(rowid(&cursor.after_rowid).ok_or(DataError::Invalid("chat cursor rowid"))?)
    } else {
        None
    };
    if let Some(after) = after {
        let exists: bool = conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM chats c WHERE ({}) AND c.rowid=?4)",
                selection(scope)
            ),
            params![kind, scope_id, like_id, after],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(DataError::StaleCursor);
        }
    }
    let sql=format!("SELECT c.rowid,CASE WHEN length(CAST(c.id AS BLOB)) BETWEEN 1 AND 512 AND instr(CAST(c.id AS BLOB),X'00')=0 THEN c.id END FROM chats c WHERE ({}) AND (?4 IS NULL OR c.created_at<(SELECT created_at FROM chats WHERE rowid=?4) OR (c.created_at=(SELECT created_at FROM chats WHERE rowid=?4) AND c.rowid>?4)) ORDER BY c.created_at DESC,c.rowid ASC LIMIT ?5",selection(scope));
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(params![
        kind,
        scope_id,
        like_id,
        after,
        u32::from(limit) + 1
    ])?;
    let mut page = DesktopChatListPage {
        library_scope,
        catalog_revision,
        items: Vec::new(),
        next: None,
    };
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        if page.items.len() == usize::from(limit) {
            has_more = true;
            break;
        }
        let row_id: i64 = row.get(0)?;
        let id: Option<String> = row.get(1)?;
        let item = id
            .as_deref()
            .ok_or(DataError::Invalid("legacy chat identifier budget"))
            .and_then(|id| chat_header_on(conn, id));
        let item = match item {
            Err(DataError::Invalid(_)) if !page.items.is_empty() => {
                has_more = true;
                break;
            }
            item => item?,
        };
        page.items.push(item);
        page.next = Some(DesktopChatListCursor {
            library_scope: page.library_scope.clone(),
            catalog_revision: page.catalog_revision.clone(),
            query_hash: query_hash.clone(),
            after_rowid: row_id.to_string(),
        });
        if !fits(&DesktopChatsReply::List(page.clone()), max) {
            page.items.pop();
            if page.items.is_empty() {
                return Err(DataError::Invalid("chat header exceeds page budget"));
            }
            has_more = true;
            break;
        }
    }
    if after.is_some() && page.items.is_empty() {
        return Err(DataError::StaleCursor);
    }
    page.next = if has_more {
        Some(DesktopChatListCursor {
            library_scope: page.library_scope.clone(),
            catalog_revision: page.catalog_revision.clone(),
            query_hash,
            after_rowid: page
                .items
                .last()
                .ok_or(DataError::Invalid("chat page progress"))?
                .rowid
                .clone(),
        })
    } else {
        None
    };
    Ok(DesktopChatsReply::List(page))
}

fn messages_on(conn: &Connection, request: &DesktopChatsRequest) -> DataResult<DesktopChatsReply> {
    let DesktopChatsRequest::Messages {
        chat_id,
        expected_generation: generation,
        expected_messages_revision: messages_revision,
        limit,
        max_bytes,
        cursor,
    } = request
    else {
        return Err(DataError::Invalid("message read operation"));
    };
    let (limit, max_bytes) = (*limit, *max_bytes);
    let (chat_id, generation, messages_revision) = (
        chat_id.as_str(),
        generation.as_str(),
        messages_revision.as_str(),
    );
    let max = budget(max_bytes)?;
    if !(1..=100).contains(&limit) || !token(generation) || !token(messages_revision) {
        return Err(DataError::Invalid("chat message page"));
    }
    let chat = chat_header_on(conn, chat_id)?;
    check_revision(&chat.generation, generation)?;
    check_revision(&chat.messages_revision, messages_revision)?;
    let library_scope = scope_on(conn)?;
    let query_hash = hash(&(
        "chat_messages_v1",
        chat_id,
        generation,
        messages_revision,
        limit,
        max_bytes,
    ))?;
    let after = if let Some(cursor) = cursor {
        if cursor.library_scope != library_scope
            || cursor.chat_id != chat_id
            || cursor.generation != generation
            || cursor.messages_revision != messages_revision
            || cursor.query_hash != query_hash
        {
            return Err(DataError::StaleCursor);
        }
        Some(rowid(&cursor.after_rowid).ok_or(DataError::Invalid("chat cursor rowid"))?)
    } else {
        None
    };
    if let Some(after) = after {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=?1 AND rowid=?2)",
            params![chat_id, after],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(DataError::StaleCursor);
        }
    }
    let mut statement=conn.prepare("SELECT m.rowid,CASE WHEN length(CAST(m.id AS BLOB)) BETWEEN 1 AND 512 AND instr(CAST(m.id AS BLOB),X'00')=0 THEN m.id END FROM chat_messages m WHERE m.chat_id=?1 AND (?2 IS NULL OR m.created_at>(SELECT created_at FROM chat_messages WHERE rowid=?2) OR (m.created_at=(SELECT created_at FROM chat_messages WHERE rowid=?2) AND m.rowid>?2)) ORDER BY m.created_at ASC,m.rowid ASC LIMIT ?3")?;
    let mut rows = statement.query(params![chat_id, after, u32::from(limit) + 1])?;
    let mut page = DesktopChatMessagesPage {
        library_scope,
        chat_id: chat_id.into(),
        generation: generation.into(),
        messages_revision: messages_revision.into(),
        items: Vec::new(),
        next: None,
    };
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        if page.items.len() == usize::from(limit) {
            has_more = true;
            break;
        }
        let row_id: i64 = row.get(0)?;
        let id: Option<String> = row.get(1)?;
        let item = id
            .as_deref()
            .ok_or(DataError::Invalid("legacy message identifier budget"))
            .and_then(|id| message_header_on(conn, chat_id, id));
        let item = match item {
            Err(DataError::Invalid(_)) if !page.items.is_empty() => {
                has_more = true;
                break;
            }
            item => item?,
        };
        page.items.push(item);
        page.next = Some(DesktopChatMessagesCursor {
            library_scope: page.library_scope.clone(),
            chat_id: chat_id.into(),
            generation: generation.into(),
            messages_revision: messages_revision.into(),
            query_hash: query_hash.clone(),
            after_rowid: row_id.to_string(),
        });
        if !fits(&DesktopChatsReply::Messages(page.clone()), max) {
            page.items.pop();
            if page.items.is_empty() {
                return Err(DataError::Invalid("message header exceeds page budget"));
            }
            has_more = true;
            break;
        }
    }
    if after.is_some() && page.items.is_empty() {
        return Err(DataError::StaleCursor);
    }
    page.next = if has_more {
        Some(DesktopChatMessagesCursor {
            library_scope: page.library_scope.clone(),
            chat_id: chat_id.into(),
            generation: generation.into(),
            messages_revision: messages_revision.into(),
            query_hash,
            after_rowid: page
                .items
                .last()
                .ok_or(DataError::Invalid("message page progress"))?
                .rowid
                .clone(),
        })
    } else {
        None
    };
    Ok(DesktopChatsReply::Messages(page))
}

fn field_window(
    conn: &Connection,
    table: &str,
    column: &str,
    id: &str,
    chat_id: Option<&str>,
    offset: u64,
    max: usize,
) -> DataResult<String> {
    // Both identifiers originate exclusively in the closed field enums below.
    let parent = if table == "chats" {
        "?4 IS NULL"
    } else {
        "chat_id=?4"
    };
    let sql=format!("SELECT COALESCE(substr(CAST({column} AS BLOB),?1,?2),X'') FROM {table} WHERE id=?3 AND {parent}");
    let bytes: Vec<u8> = conn.query_row(
        &sql,
        params![
            i64::try_from(offset + 1).map_err(|_| DataError::Invalid("chat field offset"))?,
            i64::try_from(max + 3).map_err(|_| DataError::Invalid("chat field window"))?,
            id,
            chat_id
        ],
        |r| r.get(0),
    )?;
    prefix(&bytes, max)
}
fn fit_fragment(
    text: &str,
    offset: u64,
    total: u64,
    max: usize,
    make: impl Fn(&str) -> DesktopChatsReply,
) -> DataResult<DesktopChatsReply> {
    // A terminal page omits its cursor and can fit even when every shorter
    // nonterminal page exceeds the envelope budget. Check that endpoint first.
    if offset + text.len() as u64 == total {
        let terminal = make(text);
        if fits(&terminal, max) {
            return Ok(terminal);
        }
    }
    let mut low = 0usize;
    let mut high = text.len();
    let mut best = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        let mut end = middle;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if fits(&make(&text[..end]), max) {
            best = Some(end);
            low = middle + 1;
        } else {
            if middle == 0 {
                break;
            }
            high = middle - 1;
        }
    }
    let end = best.ok_or(DataError::Invalid("chat field envelope exceeds budget"))?;
    if end == 0 && offset < total {
        return Err(DataError::Invalid("chat field budget cannot progress"));
    }
    Ok(make(&text[..end]))
}
fn chat_field_on(
    conn: &Connection,
    request: &DesktopChatsRequest,
) -> DataResult<DesktopChatsReply> {
    let DesktopChatsRequest::ReadChatField {
        chat_id,
        expected_generation: generation,
        expected_metadata_revision: revision,
        field,
        cursor,
        max_bytes,
    } = request
    else {
        return Err(DataError::Invalid("chat field operation"));
    };
    let (field, max_bytes) = (*field, *max_bytes);
    let (chat_id, generation, revision) =
        (chat_id.as_str(), generation.as_str(), revision.as_str());
    let max = budget(max_bytes)?;
    if !token(generation) || !token(revision) {
        return Err(DataError::Invalid("chat field revision"));
    }
    let header = chat_header_on(conn, chat_id)?;
    check_revision(&header.generation, generation)?;
    check_revision(&header.metadata_revision, revision)?;
    let (column, total, present) = match field {
        DesktopChatField::Title => ("title", header.title_bytes, true),
        DesktopChatField::ScopeKind => ("scope_kind", header.scope_kind_bytes, true),
        DesktopChatField::ScopeId => (
            "scope_id",
            header.scope_id_bytes.unwrap_or(0),
            header.scope_id_bytes.is_some(),
        ),
    };
    let library_scope = scope_on(conn)?;
    let offset = if let Some(c) = cursor {
        if c.library_scope != library_scope
            || c.chat_id != chat_id
            || c.generation != generation
            || c.metadata_revision != revision
            || c.field != field
            || c.total_bytes != total
            || c.present != present
            || c.max_bytes != max_bytes
        {
            return Err(DataError::StaleCursor);
        }
        if c.byte_offset == 0 || c.byte_offset >= total {
            return Err(DataError::Invalid("chat field cursor offset"));
        }
        c.byte_offset
    } else {
        0
    };
    let text = field_window(conn, "chats", column, chat_id, None, offset, max)?;
    if text.len() as u64 > total - offset {
        return Err(DataError::Invalid("chat field count mismatch"));
    }
    fit_fragment(&text, offset, total, max, |text| {
        let end = offset + text.len() as u64;
        DesktopChatsReply::ChatField(DesktopChatFieldPage {
            library_scope: library_scope.clone(),
            chat_id: chat_id.into(),
            generation: generation.into(),
            metadata_revision: revision.into(),
            field,
            byte_offset: offset,
            total_bytes: total,
            present,
            text: text.into(),
            next: (end < total).then(|| DesktopChatFieldCursor {
                library_scope: library_scope.clone(),
                chat_id: chat_id.into(),
                generation: generation.into(),
                metadata_revision: revision.into(),
                field,
                byte_offset: end,
                total_bytes: total,
                present,
                max_bytes,
            }),
        })
    })
}
fn message_field_on(
    conn: &Connection,
    request: &DesktopChatsRequest,
) -> DataResult<DesktopChatsReply> {
    let DesktopChatsRequest::ReadMessageField {
        chat_id,
        message_id,
        expected_chat_generation: chat_generation,
        expected_generation: generation,
        expected_revision: revision,
        field,
        cursor,
        max_bytes,
    } = request
    else {
        return Err(DataError::Invalid("message field operation"));
    };
    let (field, max_bytes) = (*field, *max_bytes);
    let (chat_id, message_id, chat_generation, generation, revision) = (
        chat_id.as_str(),
        message_id.as_str(),
        chat_generation.as_str(),
        generation.as_str(),
        revision.as_str(),
    );
    let max = budget(max_bytes)?;
    if !token(chat_generation) || !token(generation) || !token(revision) {
        return Err(DataError::Invalid("message field revision"));
    }
    let chat = chat_header_on(conn, chat_id)?;
    check_revision(&chat.generation, chat_generation)?;
    let header = message_header_on(conn, chat_id, message_id)?;
    check_revision(&header.generation, generation)?;
    check_revision(&header.revision, revision)?;
    let (column, total) = match field {
        DesktopChatMessageField::Role => ("role", header.role_bytes),
        DesktopChatMessageField::Content => ("content", header.content_bytes),
        DesktopChatMessageField::CitationsStoredJson => ("citations_json", header.citations_bytes),
    };
    let library_scope = scope_on(conn)?;
    let offset = if let Some(c) = cursor {
        if c.library_scope != library_scope
            || c.chat_id != chat_id
            || c.message_id != message_id
            || c.chat_generation != chat_generation
            || c.generation != generation
            || c.revision != revision
            || c.field != field
            || c.total_bytes != total
            || c.max_bytes != max_bytes
        {
            return Err(DataError::StaleCursor);
        }
        if c.byte_offset == 0 || c.byte_offset >= total {
            return Err(DataError::Invalid("message field cursor offset"));
        }
        c.byte_offset
    } else {
        0
    };
    let text = field_window(
        conn,
        "chat_messages",
        column,
        message_id,
        Some(chat_id),
        offset,
        max,
    )?;
    if text.len() as u64 > total - offset {
        return Err(DataError::Invalid("message field count mismatch"));
    }
    fit_fragment(&text, offset, total, max, |text| {
        let end = offset + text.len() as u64;
        DesktopChatsReply::MessageField(DesktopChatMessageFieldPage {
            library_scope: library_scope.clone(),
            chat_id: chat_id.into(),
            message_id: message_id.into(),
            chat_generation: chat_generation.into(),
            generation: generation.into(),
            revision: revision.into(),
            field,
            byte_offset: offset,
            total_bytes: total,
            text: text.into(),
            next: (end < total).then(|| DesktopChatMessageFieldCursor {
                library_scope: library_scope.clone(),
                chat_id: chat_id.into(),
                message_id: message_id.into(),
                chat_generation: chat_generation.into(),
                generation: generation.into(),
                revision: revision.into(),
                field,
                byte_offset: end,
                total_bytes: total,
                max_bytes,
            }),
        })
    })
}

impl Store {
    /// Every operation observes one deferred read snapshot and never seeds state.
    pub fn service_desktop_chats(
        &self,
        request: &DesktopChatsRequest,
    ) -> DataResult<DesktopChatsReply> {
        if !fits(request, MAX_CHAT_REQUEST_BYTES) {
            return Err(DataError::Invalid("chat request budget"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let reply = match request {
            DesktopChatsRequest::List {
                scope,
                limit,
                max_bytes,
                cursor,
            } => list_on(&tx, scope, *limit, *max_bytes, cursor)?,
            DesktopChatsRequest::Describe { chat_id } => {
                DesktopChatsReply::Header(chat_header_on(&tx, chat_id)?)
            }
            DesktopChatsRequest::Messages { .. } => messages_on(&tx, request)?,
            DesktopChatsRequest::ReadChatField { .. } => chat_field_on(&tx, request)?,
            DesktopChatsRequest::ReadMessageField { .. } => message_field_on(&tx, request)?,
        };
        if !fits(&reply, MAX_CHAT_DATA_BYTES) {
            return Err(DataError::Invalid("chat reply budget"));
        }
        tx.commit()?;
        Ok(reply)
    }
}

/// Erasure retains history while invalidating all possible library-wide pending
/// completions, even an empty assistant with no citation yet. Later completion
/// APIs must compare this erasure fence; existing native routing is untouched.
pub(super) fn erase_meeting_references_on(conn: &Connection, meeting_id: &str) -> DataResult<()> {
    let erased = super::revision();
    conn.execute("UPDATE service_chat_state SET erasure_revision=?2,messages_revision=lower(hex(randomblob(16))) WHERE state='present' AND chat_id IN (SELECT c.id FROM chats c WHERE (c.scope_kind='meeting' AND c.scope_id=?1) OR NOT(c.scope_kind='meeting' AND c.scope_id IS NOT NULL) OR EXISTS(SELECT 1 FROM chat_messages m,json_each(CASE WHEN json_valid(m.citations_json) THEN m.citations_json ELSE '[]' END) j WHERE m.chat_id=c.id AND CASE WHEN j.type='object' THEN json_extract(j.value,'$.meeting_id') END=?1))",params![meeting_id,erased])?;
    conn.execute("UPDATE service_chat_message_state SET revision=lower(hex(randomblob(16))) WHERE state='present' AND chat_id IN (SELECT chat_id FROM service_chat_state WHERE erasure_revision=?1)",[&erased])?;
    // Preserve the original citation-selection semantics, including invalid-JSON
    // fallback. The explicit fences above also cover rows with citations='[]'.
    conn.execute("UPDATE chat_messages SET citations_json='[]' WHERE chat_id IN (SELECT id FROM chats WHERE scope_kind='meeting' AND scope_id=?1) OR EXISTS(SELECT 1 FROM json_each(CASE WHEN json_valid(citations_json) THEN citations_json ELSE '[]' END) j WHERE CASE WHEN j.type='object' THEN json_extract(j.value,'$.meeting_id') END=?1)",[meeting_id])?;
    conn.execute(
        "UPDATE service_chat_catalog SET revision=lower(hex(randomblob(16))) WHERE singleton=1",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "chats_tests.rs"]
mod tests;

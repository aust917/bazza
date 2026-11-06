//! Bounded public note projections over the fixture repository.
use super::*;
use crate::service_data::desktop::snapshot_on;
use rusqlite::Row;

const MAX_NOTE_LIST_OFFSET: usize = 1_000_000;

// Keep bodies and private actor identifiers out of the metadata projection.
const HEADER_COLUMNS: &str = r#"
    CASE WHEN length(CAST(n.id AS BLOB)) BETWEEN 1 AND 128 THEN n.id END,
    CASE WHEN n.meeting_id IS NULL OR length(CAST(n.meeting_id AS BLOB)) BETWEEN 1 AND 128
         THEN n.meeting_id ELSE '' END,
    CASE WHEN length(CAST(n.title AS BLOB)) BETWEEN 1 AND 512 THEN n.title END,
    CASE WHEN length(CAST(n.revision AS BLOB)) BETWEEN 1 AND 128 THEN n.revision END,
    CASE WHEN length(CAST(n.origin AS BLOB)) BETWEEN 1 AND 64 THEN n.origin END,
    CASE WHEN length(CAST(n.created_at AS BLOB)) BETWEEN 1 AND 64 THEN n.created_at END,
    CASE WHEN length(CAST(n.updated_at AS BLOB)) BETWEEN 1 AND 64 THEN n.updated_at END
"#;
const VISIBLE_NOTES: &str = r#"
    FROM service_notes n LEFT JOIN service_meeting_state m ON m.meeting_id=n.meeting_id
    WHERE (n.meeting_id IS NULL OR m.tombstoned=0)
"#;

#[derive(Clone, Copy, Serialize)]
pub enum FixtureNoteScope<'a> {
    All,
    Standalone,
    Meeting(&'a str),
}

pub type FixtureNoteHeader = NoteListEntry;

#[derive(Serialize)]
pub struct FixtureNotesPage {
    pub items: Vec<FixtureNoteHeader>,
    pub next: Option<SearchCursor>,
}

pub struct FixtureNoteSnapshot<'a> {
    conn: &'a Connection,
    scope: String,
    pub header: FixtureNoteHeader,
}

pub struct FixtureNoteWindow {
    pub total_bytes: u64,
    pub text: String,
}

fn read_header(row: &Row<'_>) -> rusqlite::Result<FixtureNoteHeader> {
    Ok(FixtureNoteHeader {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        title: row.get(2)?,
        revision: row.get(3)?,
        origin: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn validate_header(header: &FixtureNoteHeader) -> DataResult<()> {
    safe_id(&header.id)?;
    if let Some(meeting_id) = &header.meeting_id {
        safe_id(meeting_id)?;
    }
    bounded_text(&header.title, 1, 512)?;
    bounded_text(&header.revision, 1, 128)?;
    bounded_text(&header.origin, 1, 64)
}

impl Store {
    pub fn fixture_notes(
        &self,
        filter: FixtureNoteScope<'_>,
        cursor: Option<&SearchCursor>,
        limit: u16,
        max_bytes: u32,
    ) -> DataResult<FixtureNotesPage> {
        if !(1..=100).contains(&limit) || !(1024..=MAX_DATA_BYTES as u32).contains(&max_bytes) {
            return Err(DataError::Invalid("note list budget"));
        }
        let (meeting_id, predicate) = match filter {
            FixtureNoteScope::All => (None, "AND ?1 IS NULL"),
            FixtureNoteScope::Standalone => (None, "AND ?1 IS NULL AND n.meeting_id IS NULL"),
            FixtureNoteScope::Meeting(meeting_id) => {
                safe_id(meeting_id)?;
                (Some(meeting_id), "AND n.meeting_id=?1")
            }
        };
        let query_hash = query_hash(&("fixture-notes-v1", filter, limit, max_bytes))?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (library_scope, generation) = snapshot_on(&tx)?;
        let offset = match cursor {
            None => 0,
            Some(cursor)
                if cursor.library_scope == library_scope
                    && cursor.generation == generation
                    && cursor.query_hash == query_hash
                    && (1..=MAX_NOTE_LIST_OFFSET).contains(&cursor.offset) =>
            {
                cursor.offset
            }
            Some(_) => return Err(DataError::StaleCursor),
        };
        let mut stmt = tx.prepare(&format!(
            "SELECT {HEADER_COLUMNS} {VISIBLE_NOTES} {predicate}
             ORDER BY n.id LIMIT ?2 OFFSET ?3"
        ))?;
        let mut rows = stmt.query(params![meeting_id, i64::from(limit) + 1, offset as i64])?;
        let mut page = FixtureNotesPage {
            items: Vec::new(),
            next: None,
        };
        let next = |count| SearchCursor {
            library_scope: library_scope.clone(),
            generation,
            query_hash: query_hash.clone(),
            offset: offset + count,
        };
        // Budget the full cursor as well as the page, so an escaped title
        // cannot force an unbounded read or a non-advancing continuation.
        let envelope_bytes = serde_json::to_vec(&next(usize::from(limit)))?.len() + 32;
        let mut bytes = envelope_bytes;
        let mut more = false;
        for _ in 0..limit {
            let Some(row) = rows.next()? else {
                break;
            };
            let item = read_header(row)?;
            validate_header(&item)?;
            bytes += serde_json::to_vec(&item)?.len() + 1;
            if bytes > max_bytes as usize {
                if page.items.is_empty() {
                    return Err(DataError::Invalid("note row exceeds page budget"));
                }
                more = true;
                break;
            }
            page.items.push(item);
        }
        if more || rows.next()?.is_some() {
            let cursor = next(page.items.len());
            if cursor.offset > MAX_NOTE_LIST_OFFSET {
                return Err(DataError::Invalid("note list offset"));
            }
            page.next = Some(cursor);
        } else if page.items.is_empty() && offset > 0 {
            return Err(DataError::StaleCursor);
        }
        Ok(page)
    }

    pub fn with_fixture_note<T>(
        &self,
        id: &str,
        read: impl FnOnce(FixtureNoteSnapshot<'_>) -> DataResult<T>,
    ) -> DataResult<T> {
        safe_id(id)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let header = tx
            .query_row(
                &format!("SELECT {HEADER_COLUMNS} {VISIBLE_NOTES} AND n.id=?1"),
                [id],
                read_header,
            )
            .optional()?
            .ok_or(DataError::NotFound)?;
        validate_header(&header)?;
        read(FixtureNoteSnapshot {
            conn: &tx,
            scope: scope_on(&tx)?,
            header,
        })
    }
}

impl FixtureNoteSnapshot<'_> {
    pub fn cursor_tag(&self, epoch: uuid::Uuid, offset: u64) -> String {
        query_hash(&(
            "fixture-note-v1",
            &self.scope,
            epoch,
            &self.header.id,
            &self.header.revision,
            offset,
        ))
        .expect("note cursor tuple is JSON serializable")
    }

    pub fn window(&self, offset: u64, max_bytes: u32) -> DataResult<FixtureNoteWindow> {
        if offset > JS_SAFE {
            return Err(DataError::StaleCursor);
        }
        if !(1..=65_536).contains(&max_bytes) {
            return Err(DataError::Invalid("note byte budget"));
        }
        // Fetch one extra code point's worth of bytes to identify the last
        // whole code point within the requested budget.
        let (total, bytes): (i64, Vec<u8>) = self.conn.query_row(
            "SELECT length(CAST(markdown AS BLOB)),COALESCE(substr(CAST(markdown AS BLOB),?2+1,?3),X'')
             FROM service_notes WHERE id=?1 AND revision=?4",
            params![self.header.id, offset as i64, i64::from(max_bytes) + 3, self.header.revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let total_bytes = u64::try_from(total)
            .ok()
            .filter(|total| *total <= JS_SAFE)
            .ok_or(DataError::Invalid("note length"))?;
        if offset > total_bytes || (offset > 0 && offset == total_bytes) {
            return Err(DataError::StaleCursor);
        }
        let window = utf8_window(&bytes)?;
        let mut length = window.len().min(max_bytes as usize);
        while !window.is_char_boundary(length) {
            length -= 1;
        }
        let text = &window[..length];
        if text.contains('\0') {
            return Err(DataError::Invalid("note content"));
        }
        Ok(FixtureNoteWindow {
            total_bytes,
            text: text.into(),
        })
    }
}

#[cfg(test)]
#[path = "fixture_notes_tests.rs"]
mod tests;

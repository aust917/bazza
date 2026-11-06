//! Read current summary metadata and bounded content under one fixture snapshot.
use super::*;
use chrono::{DateTime, Utc};

pub struct FixtureSummarySnapshot<'a> {
    conn: &'a Connection,
    scope: String,
    meeting_id: &'a str,
    pub revision: String,
    pub document_state: DocumentState,
    pub origin: String,
    pub auto_replace: bool,
    pub automatic_generation_suppressed: bool,
    pub source_missing: bool,
    pub source_label: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

pub struct FixtureSummaryWindow {
    pub total_bytes: u64,
    pub text: String,
}

impl Store {
    pub fn with_fixture_summary<T>(
        &self,
        meeting_id: &str,
        read: impl FnOnce(FixtureSummarySnapshot<'_>) -> DataResult<T>,
    ) -> DataResult<T> {
        safe_id(meeting_id)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        visible_meeting(&tx, meeting_id)?;
        let scope = scope_on(&tx)?;
        let snapshot = tx
            .query_row(
                "SELECT CASE WHEN length(CAST(s.revision AS BLOB))<=128 THEN s.revision END,
                    s.document_state,
                    CASE WHEN length(CAST(s.origin AS BLOB))<=64 THEN s.origin END,
                    s.auto_replace,s.automatic_generation_suppressed,s.source_missing,
                    CASE WHEN length(CAST(s.source_label AS BLOB))>256 THEN char(0)
                         ELSE NULLIF(s.source_label,'') END,
                    CASE WHEN length(CAST(b.created_at AS BLOB))<=64 THEN b.created_at END,
                    b.meeting_id IS NOT NULL
             FROM service_summary_state s LEFT JOIN summaries b ON b.meeting_id=s.meeting_id
             WHERE s.meeting_id=?1",
                [meeting_id],
                |row| {
                    let state = state_kind(row.get(1)?)?;
                    let published_at: Option<DateTime<Utc>> = row.get(7)?;
                    let has_body: bool = row.get(8)?;
                    if has_body != (state == DocumentState::Present)
                        || has_body != published_at.is_some()
                    {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    Ok(FixtureSummarySnapshot {
                        conn: &tx,
                        scope,
                        meeting_id,
                        revision: row.get(0)?,
                        document_state: state,
                        origin: row.get(2)?,
                        auto_replace: row.get(3)?,
                        automatic_generation_suppressed: row.get(4)?,
                        source_missing: row.get(5)?,
                        source_label: row.get(6)?,
                        published_at,
                    })
                },
            )
            .optional()?
            .ok_or(DataError::NotFound)?;
        bounded_text(&snapshot.revision, 1, 128)?;
        bounded_text(&snapshot.origin, 1, 64)?;
        if let Some(label) = &snapshot.source_label {
            bounded_text(label, 1, 256)?;
        }
        read(snapshot)
    }
}

impl FixtureSummarySnapshot<'_> {
    pub fn cursor_tag(&self, epoch: uuid::Uuid, field: DesktopSummaryField, offset: u64) -> String {
        query_hash(&(
            "fixture-public-summary-v1",
            &self.scope,
            epoch,
            self.meeting_id,
            &self.revision,
            field,
            offset,
        ))
        .expect("summary cursor tuple is JSON serializable")
    }

    pub fn window(
        &self,
        field: DesktopSummaryField,
        offset: u64,
        max_bytes: u32,
    ) -> DataResult<FixtureSummaryWindow> {
        if self.document_state != DocumentState::Present || offset > JS_SAFE {
            return Err(DataError::StaleCursor);
        }
        if !(1..=65_536).contains(&max_bytes) {
            return Err(DataError::Invalid("summary byte budget"));
        }
        let column = match field {
            DesktopSummaryField::Markdown => "markdown",
            DesktopSummaryField::StructuredJson => "structured_json",
        };
        let sql = format!(
            "SELECT length(CAST({column} AS BLOB)),COALESCE(substr(CAST({column} AS BLOB),?2+1,?3),X'')
             FROM summaries WHERE meeting_id=?1"
        );
        // One code point of lookahead lets a too-small budget fail cleanly.
        let (total, bytes): (i64, Vec<u8>) = self.conn.query_row(
            &sql,
            params![self.meeting_id, offset as i64, i64::from(max_bytes) + 3],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let total_bytes = u64::try_from(total)
            .ok()
            .filter(|total| *total <= JS_SAFE)
            .ok_or(DataError::Invalid("summary length"))?;
        if offset > total_bytes || (offset > 0 && offset == total_bytes) {
            return Err(DataError::StaleCursor);
        }
        let window = utf8_window(&bytes)?;
        let mut length = window.len().min(max_bytes as usize);
        while !window.is_char_boundary(length) {
            length -= 1;
        }
        Ok(FixtureSummaryWindow {
            total_bytes,
            text: window[..length].to_owned(),
        })
    }
}

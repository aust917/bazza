//! Read windows within one snapshot. The public projection consumes these
//! windows without opening another owner or reading the whole raw source.
use super::*;
use bazza_domain::local_asr::model_id_valid;
use chrono::{DateTime, Utc};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureTranscriptRepresentation {
    Raw,
    Effective,
    Both,
}

pub struct FixtureTranscriptSource {
    pub id: String,
    pub origin: String,
    pub source_label: Option<String>,
    pub provider_label: Option<String>,
    pub model_label: Option<String>,
    pub language: Option<String>,
    pub created_at: DateTime<Utc>,
    pub segment_count: u64,
}

pub struct FixtureTranscriptWindow {
    pub id: i64,
    pub start: i64,
    pub end: i64,
    pub speaker: Option<String>,
    pub manual: bool,
    pub total_bytes: u64,
    // The selected UTF-8 window is raw followed by effective in Both mode.
    pub text: String,
    pub raw_prefix_bytes: usize,
}

pub struct FixtureTranscriptSnapshot<'a> {
    conn: &'a Connection,
    scope: String,
    explicit_source: bool,
    pub state: TranscriptState,
    pub source: Option<FixtureTranscriptSource>,
}

impl Store {
    pub fn with_fixture_transcript<T>(
        &self,
        meeting_id: &str,
        source_id: Option<&str>,
        read: impl FnOnce(FixtureTranscriptSnapshot<'_>) -> DataResult<T>,
    ) -> DataResult<T> {
        safe_id(meeting_id)?;
        if let Some(source_id) = source_id {
            safe_id(source_id)?;
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        visible_meeting(&tx, meeting_id)?;
        let state = transcript_state(&tx, meeting_id)?;
        let source = source_id.or(state.current_source_id.as_deref());
        if (state.document_state == DocumentState::Present) != state.current_source_id.is_some() {
            return Err(DataError::Invalid("transcript current source"));
        }
        let source = source
            .map(|id| read_source(&tx, meeting_id, id))
            .transpose()?;
        read(FixtureTranscriptSnapshot {
            conn: &tx,
            scope: scope_on(&tx)?,
            explicit_source: source_id.is_some(),
            state,
            source,
        })
    }
}

fn read_source(conn: &Connection, meeting: &str, id: &str) -> DataResult<FixtureTranscriptSource> {
    let mut source = conn
        .query_row(
            "SELECT CASE WHEN length(CAST(origin AS BLOB))<=32 THEN origin END,
                    CASE WHEN length(CAST(source_label AS BLOB))>256 THEN '' ELSE source_label END,
                    CASE WHEN length(CAST(language AS BLOB))>64 THEN '' ELSE language END,
                    CASE WHEN length(CAST(created_at AS BLOB))<=64 THEN created_at END,
                    json_array_length(raw_json)
             FROM service_transcript_sources
             WHERE id=?1 AND meeting_id=?2 AND json_type(raw_json)='array'",
            params![id, meeting],
            |row| {
                let count: i64 = row.get(4)?;
                if count < 0 || count as u64 > JS_SAFE {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(FixtureTranscriptSource {
                    id: id.into(),
                    origin: row.get(0)?,
                    source_label: row.get(1)?,
                    language: row.get(2)?,
                    created_at: row.get(3)?,
                    segment_count: count as u64,
                    provider_label: None,
                    model_label: None,
                })
            },
        )
        .optional()?
        .ok_or(DataError::NotFound)?;
    if let Some(label) = &source.source_label {
        bounded_text(label, 1, 256)?;
    }
    if let Some(language) = &source.language {
        bounded_text(language, 1, 64)?;
    }
    if source.origin == "captured" {
        if let Some(provider @ ("local" | "soniox")) = source.source_label.as_deref() {
            source.provider_label = Some(provider.into());
            // The capture lease's selection belongs to this source only when
            // the immutable capture anchor links it. Never consult mutable STT
            // settings or use the current meeting's model for an archive.
            source.model_label = conn
                .query_row(
                    "SELECT CASE WHEN length(CAST(json_extract(l.provider_json,'$.model') AS BLOB))>256
                                 THEN '' ELSE json_extract(l.provider_json,'$.model') END
                     FROM service_post_process_anchors a
                     JOIN service_capture_leases l ON l.job_id=a.job_id AND l.meeting_id=a.meeting_id
                     WHERE a.root_source_id=?1 AND a.meeting_id=?2 AND l.state='finalized'
                       AND json_extract(l.provider_json,'$.provider')=?3
                       AND json_extract(l.provider_json,'$.language') IS ?4",
                    params![id, meeting, provider, source.language],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            if let Some(model) = &source.model_label {
                bounded_text(model, 1, 256)?;
            }
        }
    } else if source.origin == "local_asr" {
        if source.source_label.as_deref() != Some("local") {
            return Err(DataError::Invalid("local ASR source label"));
        }
        // The model comes from this source's immutable lineage, even after it
        // becomes an archive. No-model records an attempted quality ID.
        let (model, outcome, caption_count): (String, String, i64) = conn
            .query_row(
                "SELECT CASE WHEN length(CAST(l.model AS BLOB))<=128 THEN l.model ELSE '' END,
                        CASE WHEN l.outcome IN ('complete','no_model') THEN l.outcome ELSE '' END,
                        l.caption_count
                 FROM service_post_process_local_sources l
                 JOIN service_post_process_anchors a ON a.job_id=l.job_id
                 WHERE l.source_id=?1 AND a.meeting_id=?2 AND a.root_source_id IS NOT l.source_id",
                params![id, meeting],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(DataError::Invalid("local ASR source lineage"))?;
        if !model_id_valid(&model)
            || u64::try_from(caption_count).ok() != Some(source.segment_count)
        {
            return Err(DataError::Invalid("local ASR source metadata"));
        }
        source.model_label = match outcome.as_str() {
            "complete" => Some(model),
            "no_model" if caption_count == 0 => None,
            _ => return Err(DataError::Invalid("local ASR source outcome")),
        };
        source.provider_label = Some("local".into());
    }
    Ok(source)
}

impl FixtureTranscriptSnapshot<'_> {
    pub fn cursor_tag(
        &self,
        epoch: uuid::Uuid,
        representation: FixtureTranscriptRepresentation,
        index: u64,
        offset: u64,
    ) -> String {
        query_hash(&(
            "fixture-public-transcript-v1",
            &self.scope,
            epoch,
            &self.state.meeting_id,
            &self.state.revision,
            &self.state.current_source_id,
            self.source.as_ref().map(|source| &source.id),
            self.explicit_source,
            representation,
            index,
            offset,
        ))
        .expect("transcript cursor tuple is JSON serializable")
    }

    pub fn window(
        &self,
        representation: FixtureTranscriptRepresentation,
        index: u64,
        offset: u64,
        max: usize,
    ) -> DataResult<FixtureTranscriptWindow> {
        let source = self.source.as_ref().ok_or(DataError::StaleCursor)?;
        if index >= source.segment_count || offset > JS_SAFE || max > 65_536 {
            return Err(DataError::StaleCursor);
        }
        // A code point of lookahead distinguishes a too-small request budget
        // from an invalid UTF-8 offset without crossing the returned budget.
        let window = |offset, max, repr| {
            segment_window(
                self.conn,
                &self.state.meeting_id,
                &source.id,
                index,
                offset,
                max,
                repr,
            )?
            .ok_or(DataError::StaleCursor)
        };
        let raw = DesktopTranscriptRepresentation::Raw;
        let effective = DesktopTranscriptRepresentation::Effective;
        let (row, mut text, raw_prefix_bytes, total_bytes) = match representation {
            FixtureTranscriptRepresentation::Raw => {
                let row = window(offset, max + 4, raw)?;
                let text = utf8_window(&row.bytes)?.to_owned();
                let prefix = text.len();
                let total = row.total;
                (row, text, prefix, total)
            }
            FixtureTranscriptRepresentation::Effective => {
                let row = window(offset, max + 4, effective)?;
                let text = utf8_window(&row.bytes)?.to_owned();
                let total = row.total;
                (row, text, 0, total)
            }
            FixtureTranscriptRepresentation::Both => {
                let mut row = window(0, if offset == 0 { max + 4 } else { 0 }, raw)?;
                let mut text = String::new();
                if offset < row.total {
                    if offset > 0 {
                        row = window(offset, max + 4, raw)?;
                    }
                    text.push_str(utf8_window(&row.bytes)?);
                }
                let raw_prefix = text.len();
                let raw_complete = offset + raw_prefix as u64 >= row.total;
                let remaining = if raw_complete && raw_prefix < max {
                    max - raw_prefix + 4
                } else {
                    0
                };
                let effective_row = window(offset.saturating_sub(row.total), remaining, effective)?;
                text.push_str(utf8_window(&effective_row.bytes)?);
                let total = row
                    .total
                    .checked_add(effective_row.total)
                    .ok_or(DataError::Invalid("transcript length"))?;
                (row, text, raw_prefix, total)
            }
        };
        if total_bytes > JS_SAFE || offset > total_bytes || (offset > 0 && offset == total_bytes) {
            return Err(DataError::StaleCursor);
        }
        let mut length = text.len().min(max);
        while !text.is_char_boundary(length) {
            length -= 1;
        }
        text.truncate(length);
        if text.contains('\0')
            || row.end <= row.start
            || row.speaker.as_ref().is_some_and(|label| {
                label.is_empty() || Some(label.len() as i64) != row.speaker_bytes
            })
        {
            return Err(DataError::Invalid("public transcript segment"));
        }
        Ok(FixtureTranscriptWindow {
            id: row.id,
            start: row.start,
            end: row.end,
            speaker: row.speaker,
            manual: row.manual,
            total_bytes,
            text,
            raw_prefix_bytes: raw_prefix_bytes.min(length),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_source_metadata_refuses_absent_or_unrepresentable_lineage() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn.lock();
        conn.execute_batch(
            "INSERT INTO meetings(id,title,started_at,status,created_at)
                 VALUES('fixture','Fixture','2026-11-03T00:00:00Z','processing','2026-11-03T00:00:00Z');
             INSERT INTO service_jobs(id,kind,executor,state,revision,actor,created_at,updated_at)
                 VALUES('job','processing','gui_worker','running','job-rev','test','2026-11-03T00:00:00Z','2026-11-03T00:00:00Z');
             INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,raw_json,created_at)
                 VALUES('source','fixture','local_asr','local','[]','2026-11-03T00:00:00Z');",
        )
        .unwrap();
        assert!(matches!(
            read_source(&conn, "fixture", "source"),
            Err(DataError::Invalid("local ASR source lineage"))
        ));
        conn.execute_batch(
            "INSERT INTO service_post_process_anchors
                 VALUES('job','fixture','fingerprint','life','transcript',NULL,'absent');
             INSERT INTO service_post_process_local_sources
                 VALUES('source','job','private/path','complete',0);",
        )
        .unwrap();
        assert!(matches!(
            read_source(&conn, "fixture", "source"),
            Err(DataError::Invalid("local ASR source metadata"))
        ));
    }
}

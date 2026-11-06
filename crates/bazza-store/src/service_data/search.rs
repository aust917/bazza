use super::fixture_read::{FIXTURE_MEETING_FILTER, FIXTURE_MEETING_TIMES};
use super::*;
use crate::Store;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchRepresentation {
    Raw,
    #[default]
    Effective,
    Both,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    /// Empty selects title, transcript, summary and notes.
    pub fields: Vec<String>,
    pub representation: SearchRepresentation,
    pub meeting_ids: Vec<String>,
    pub limit: usize,
    pub cursor: Option<SearchCursor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchCursor {
    pub library_scope: String,
    pub generation: i64,
    pub query_hash: String,
    pub offset: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentHit {
    pub source_kind: String,
    pub entity_id: String,
    pub meeting_id: Option<String>,
    pub source_id: Option<String>,
    pub revision: String,
    pub representation: String,
    pub segment_id: Option<i64>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub snippet: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPage {
    pub generation: i64,
    pub hits: Vec<DocumentHit>,
    pub next: Option<SearchCursor>,
}

fn insert(
    conn: &Connection,
    kind: &str,
    id: &str,
    meeting: Option<&str>,
    source: Option<&str>,
    rev: &str,
    text: &str,
) -> DataResult<()> {
    conn.execute("INSERT INTO service_search_documents(source_kind,entity_id,meeting_id,source_id,revision,representation,text) VALUES(?1,?2,?3,?4,?5,'effective',?6)",params![kind,id,meeting,source,rev,text])?;
    Ok(())
}
fn bump(conn: &Connection) -> DataResult<()> {
    conn.execute(
        "UPDATE service_search_state SET generation=generation+1 WHERE singleton=1",
        [],
    )?;
    Ok(())
}

fn meeting_documents(conn: &Connection, id: &str) -> DataResult<()> {
    let data:Option<(String,String)>=conn.query_row("SELECT m.title,s.metadata_revision FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id WHERE m.id=?1 AND s.tombstoned=0",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((title, rev)) = data else {
        return Ok(());
    };
    insert(conn, "title", id, Some(id), None, &rev, &title)?;
    let state:Option<(String,String)>=conn.query_row("SELECT revision,current_source_id FROM service_transcript_state WHERE meeting_id=?1 AND document_state='present'",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((rev, source)) = state {
        for segment in crate::segments::segments_on(conn, id)? {
            for (representation, text) in [
                ("raw", segment.text.as_str()),
                (
                    "effective",
                    segment.clean_text.as_deref().unwrap_or(&segment.text),
                ),
            ] {
                conn.execute("INSERT INTO service_search_documents(source_kind,entity_id,meeting_id,source_id,revision,representation,segment_id,start_ms,end_ms,text) VALUES('transcript',?1,?1,?2,?3,?4,?5,?6,?7,?8)",params![id,source,rev,representation,segment.id,segment.start_ms,segment.end_ms,text])?;
            }
        }
    }
    let summary:Option<(String,String)>=conn.query_row("SELECT s.markdown,st.revision FROM summaries s JOIN service_summary_state st ON st.meeting_id=s.meeting_id WHERE s.meeting_id=?1 AND st.document_state='present'",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((text, rev)) = summary {
        insert(conn, "summary", id, Some(id), None, &rev, &text)?;
    }
    Ok(())
}

pub(super) fn rebuild_meeting_on(conn: &Connection, id: &str) -> DataResult<()> {
    conn.execute("DELETE FROM service_search_documents WHERE entity_id=?1 AND source_kind IN ('title','transcript','summary')",[id])?;
    meeting_documents(conn, id)?;
    bump(conn)
}

/// Refresh only title text and its metadata revision. Worker title output must
/// not load or replace transcript/summary documents or their row identities.
pub(super) fn refresh_title_on(
    conn: &Connection,
    id: &str,
    revision: &str,
    title: &str,
) -> DataResult<()> {
    conn.execute(
        "DELETE FROM service_search_documents WHERE source_kind='title' AND entity_id=?1",
        [id],
    )?;
    insert(conn, "title", id, Some(id), None, revision, title)?;
    bump(conn)
}

pub(super) fn rebuild_note_on(conn: &Connection, id: &str) -> DataResult<()> {
    conn.execute(
        "DELETE FROM service_search_documents WHERE entity_id=?1 AND source_kind='note'",
        [id],
    )?;
    let note:Option<(Option<String>,String,String,String)>=conn.query_row("SELECT n.meeting_id,n.title,n.markdown,n.revision FROM service_notes n LEFT JOIN service_meeting_state m ON m.meeting_id=n.meeting_id WHERE n.id=?1 AND (n.meeting_id IS NULL OR m.tombstoned=0)",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((meeting, title, text, rev)) = note {
        insert(
            conn,
            "note",
            id,
            meeting.as_deref(),
            None,
            &rev,
            &format!("{title}\n{text}"),
        )?;
    }
    bump(conn)
}

impl Store {
    /// Full transactional backfill. A failed rebuild leaves the prior readiness
    /// and projection intact. No partial index is advertised as ready.
    pub fn service_rebuild_search(&self) -> DataResult<i64> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE service_search_state SET state='building' WHERE singleton=1",
            [],
        )?;
        tx.execute("DELETE FROM service_search_documents", [])?;
        let ids: Vec<String> = tx
            .prepare("SELECT meeting_id FROM service_meeting_state WHERE tombstoned=0")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            meeting_documents(&tx, &id)?;
        }
        let notes: Vec<String> = tx
            .prepare("SELECT id FROM service_notes")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in notes {
            rebuild_note_on(&tx, &id)?;
        }
        tx.execute("UPDATE service_search_state SET state='ready',generation=generation+1 WHERE singleton=1",[])?;
        let generation = tx.query_row(
            "SELECT generation FROM service_search_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(generation)
    }
    pub fn service_search_state(&self) -> DataResult<(String, i64)> {
        Ok(self.conn.lock().query_row(
            "SELECT state,generation FROM service_search_state WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
    pub fn service_search(&self, input: &SearchRequest) -> DataResult<SearchPage> {
        search_on(&self.conn.lock(), input, None).map(|(page, _)| page)
    }
}

pub(super) fn search_on(
    conn: &Connection,
    input: &SearchRequest,
    fixture_filter: Option<&FixtureMeetingFilter>,
) -> DataResult<(SearchPage, SearchCursor)> {
    bounded_text(&input.query, 1, 2048)?;
    if !(1..=100).contains(&input.limit) || input.fields.len() > 4 || input.meeting_ids.len() > 100
    {
        return Err(DataError::Invalid("search limits"));
    }
    let mut fields = std::collections::BTreeSet::new();
    for f in &input.fields {
        if !["title", "transcript", "summary", "note"].contains(&f.as_str()) || !fields.insert(f) {
            return Err(DataError::Invalid("search fields"));
        }
    }
    for id in &input.meeting_ids {
        safe_id(id)?;
    }
    let query_identity = (
        &input.query,
        &input.fields,
        input.representation,
        &input.meeting_ids,
        input.limit,
    );
    let query_identity = if let Some(filter) = fixture_filter {
        let (_, sequence) = super::desktop::snapshot_on(conn)?;
        serde_json::to_vec(&(query_identity, filter, sequence))?
    } else {
        serde_json::to_vec(&query_identity)?
    };
    let fingerprint = format!("{:x}", Sha256::digest(query_identity));
    let offset = if let Some(c) = &input.cursor {
        if c.query_hash != fingerprint || c.offset > 1_000_000 {
            return if fixture_filter.is_some() {
                Err(DataError::StaleCursor)
            } else {
                Err(DataError::Invalid("search cursor"))
            };
        }
        c.offset
    } else {
        0
    };
    let tokens = input
        .query
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if tokens.len() > 64 {
        return Err(DataError::Invalid("search token count"));
    }
    let query = tokens
        .iter()
        .map(|s| format!("\"{s}\"*"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let (state, generation, library_scope): (String, i64, String) = conn.query_row(
        "SELECT state,generation,cursor_scope FROM service_search_state WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if state != "ready" {
        return Err(DataError::IndexUnavailable);
    }
    if input
        .cursor
        .as_ref()
        .is_some_and(|c| c.generation != generation || c.library_scope != library_scope)
    {
        return Err(DataError::StaleCursor);
    }
    let position = SearchCursor {
        library_scope,
        generation,
        query_hash: fingerprint,
        offset,
    };
    if query.is_empty() {
        return Ok((
            SearchPage {
                generation,
                hits: vec![],
                next: None,
            },
            position,
        ));
    }
    let representation = match input.representation {
        SearchRepresentation::Raw => "raw",
        SearchRepresentation::Effective => "effective",
        SearchRepresentation::Both => "both",
    };
    const SNIPPET: &str = "snippet(service_search_fts,0,'','','…',24)";
    let (dates, joins, scope, snippet, revision) = if fixture_filter.is_some() {
        (
            FIXTURE_MEETING_TIMES,
            "LEFT JOIN meetings m ON m.id=d.meeting_id
             LEFT JOIN service_meeting_state s ON s.meeting_id=m.id
             LEFT JOIN fixture_times tm ON tm.id=m.id
             CROSS JOIN (SELECT ?7 AS value) f",
            format!("AND (d.meeting_id IS NULL OR s.tombstoned=0) AND ({FIXTURE_MEETING_FILTER})"),
            format!("substr({SNIPPET},1,2048)"),
            "CASE WHEN d.source_kind='title' THEN s.metadata_revision ELSE d.revision END",
        )
    } else {
        (
            "",
            "",
            "AND ?7 IS NULL".into(),
            SNIPPET.into(),
            "d.revision",
        )
    };
    let mut st = conn.prepare(&format!(r#"
            {dates}
            SELECT d.source_kind,d.entity_id,d.meeting_id,d.source_id,{revision},
                CASE WHEN ?2='both' AND d.source_kind='transcript' AND EXISTS(
                    SELECT 1 FROM service_search_documents paired WHERE paired.entity_id=d.entity_id
                    AND paired.segment_id=d.segment_id AND paired.representation!=d.representation AND paired.text=d.text
                ) THEN 'both' ELSE d.representation END,
                d.segment_id,d.start_ms,d.end_ms,{snippet}
            FROM service_search_fts JOIN service_search_documents d ON d.id=service_search_fts.rowid
            {joins}
            WHERE service_search_fts MATCH ?1
            AND (d.source_kind!='transcript' OR ?2='both' OR d.representation=?2)
            AND NOT (?2='both' AND d.source_kind='transcript' AND d.representation='raw' AND EXISTS(
                SELECT 1 FROM service_search_documents paired WHERE paired.entity_id=d.entity_id
                AND paired.segment_id=d.segment_id AND paired.representation='effective' AND paired.text=d.text
            ))
            AND (json_array_length(?3)=0 OR d.source_kind IN (SELECT value FROM json_each(?3)))
            AND (json_array_length(?4)=0 OR d.meeting_id IN (SELECT value FROM json_each(?4)))
            {scope}
            ORDER BY bm25(service_search_fts),d.id LIMIT ?5 OFFSET ?6
        "#))?;
    let mut hits: Vec<DocumentHit> = st
        .query_map(
            params![
                query,
                representation,
                serde_json::to_string(&input.fields)?,
                serde_json::to_string(&input.meeting_ids)?,
                input.limit as i64 + 1,
                offset as i64,
                fixture_filter
                    .map(FixtureMeetingFilter::query_json)
                    .transpose()?,
            ],
            |r| {
                Ok(DocumentHit {
                    source_kind: r.get(0)?,
                    entity_id: r.get(1)?,
                    meeting_id: r.get(2)?,
                    source_id: r.get(3)?,
                    revision: r.get(4)?,
                    representation: r.get(5)?,
                    segment_id: r.get(6)?,
                    start_ms: r.get(7)?,
                    end_ms: r.get(8)?,
                    snippet: r.get(9)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    let more = hits.len() > input.limit;
    hits.truncate(input.limit);
    for hit in &mut hits {
        let mut end = hit.snippet.len().min(2048);
        while !hit.snippet.is_char_boundary(end) {
            end -= 1;
        }
        hit.snippet.truncate(end);
    }
    Ok((
        SearchPage {
            generation,
            hits,
            next: more.then_some(SearchCursor {
                offset: offset + input.limit,
                ..position.clone()
            }),
        },
        position,
    ))
}

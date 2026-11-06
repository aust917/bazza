//! Read-side snapshots for the fixture MCP projection. No operation adopts a
//! legacy row or changes search, ownership, settings, or recording state.
use super::{desktop as d, search::search_on, *};
use crate::Store;
use chrono::{DateTime, Utc};
use rusqlite::{params, Transaction};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Default, Serialize)]
pub struct FixtureMeetingFilter {
    pub from: Option<FixtureInstant>,
    pub to: Option<FixtureInstant>,
    pub meeting_ids: Option<Vec<String>>,
    pub folder_ids: Option<Vec<String>>,
    pub tag_ids: Option<Vec<String>>,
    pub participants: Option<Vec<String>>,
    pub content_kind: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct FixtureInstant {
    seconds: i64,
    nanos: u32,
}

impl std::str::FromStr for FixtureInstant {
    type Err = chrono::ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let timestamp = DateTime::parse_from_rfc3339(value)?;
        Ok(Self {
            seconds: timestamp.timestamp(),
            nanos: timestamp.timestamp_subsec_nanos(),
        })
    }
}

impl FixtureMeetingFilter {
    pub(super) fn query_json(&self) -> DataResult<String> {
        Ok(serde_json::to_string(self)?)
    }
}

// SQLite's datetime functions round fractions to milliseconds. Remove the
// fraction before timezone normalization, and compare the exact decimal tail
// separately. Accept the native SQLite/chrono spellings with ' ' or 'T'.
pub(super) const FIXTURE_MEETING_TIMES: &str = r#"
WITH fixture_time_parts AS (
  SELECT id, started_at, instr(started_at,'.') AS point,
         substr(started_at,instr(started_at,'.')+1) AS tail
    FROM meetings WHERE length(CAST(started_at AS BLOB))<=64
), fixture_times AS (
  SELECT id,
    CAST(strftime('%s',CASE WHEN point=0 THEN started_at ELSE
        substr(started_at,1,point-1)||ltrim(tail,'0123456789') END) AS INTEGER) AS seconds,
    CASE WHEN point=0 THEN 0 ELSE CAST(substr(
        substr(tail,1,length(tail)-length(ltrim(tail,'0123456789')))||'000000000',1,9
    ) AS INTEGER) END AS nanos
  FROM fixture_time_parts
)
"#;

// Predicates are conjunctive across filter fields and disjunctive within
// each set. NULL disables a field; an explicitly empty set matches nothing.
// The SQL only binds JSON values, never constructs predicates from user text.
pub(super) const FIXTURE_MEETING_FILTER: &str = r#"
 (json_extract(f.value,'$.from') IS NULL OR (tm.seconds,tm.nanos)>=(json_extract(f.value,'$.from.seconds'),json_extract(f.value,'$.from.nanos')))
 AND (json_extract(f.value,'$.to') IS NULL OR (tm.seconds,tm.nanos)<(json_extract(f.value,'$.to.seconds'),json_extract(f.value,'$.to.nanos')))
 AND (json_extract(f.value,'$.meeting_ids') IS NULL OR m.id IN (SELECT value FROM json_each(f.value,'$.meeting_ids')))
 AND (json_extract(f.value,'$.folder_ids') IS NULL OR m.folder_id IN (SELECT value FROM json_each(f.value,'$.folder_ids')))
 AND (json_extract(f.value,'$.tag_ids') IS NULL OR EXISTS(SELECT 1 FROM meeting_tags WHERE meeting_id=m.id AND tag_id IN (SELECT value FROM json_each(f.value,'$.tag_ids'))))
 AND (json_extract(f.value,'$.participants') IS NULL OR EXISTS(SELECT 1 FROM json_each(CASE WHEN json_valid(m.participants_json) THEN m.participants_json ELSE '[]' END) p WHERE p.type='text' AND p.value IN (SELECT value FROM json_each(f.value,'$.participants'))))
 AND (json_extract(f.value,'$.content_kind') IS NULL OR s.content_kind=json_extract(f.value,'$.content_kind'))
"#;

pub struct FixtureMeeting {
    pub item: d::DesktopMeeting,
    pub tag_ids: Vec<String>,
    pub current_source_id: Option<String>,
}

pub struct FixtureMeetingPage {
    pub items: Vec<FixtureMeeting>,
    pub position: SearchCursor,
    pub more: bool,
}

pub struct FixtureSearchHit {
    pub hit: DocumentHit,
    pub title: String,
    pub date: Option<DateTime<Utc>>,
}

pub struct FixtureSearchPage {
    pub generation: i64,
    pub hits: Vec<FixtureSearchHit>,
    pub next: Option<SearchCursor>,
    pub position: SearchCursor,
}

pub struct FixtureHealth {
    pub index_state: String,
    pub search_generation: i64,
    pub queued: u32,
    pub running: u32,
    pub waiting_for_worker: u32,
}

fn decorate_meeting(
    store: &Store,
    tx: &Transaction<'_>,
    mut item: d::DesktopMeeting,
) -> DataResult<FixtureMeeting> {
    let tag_ids = tx.prepare("SELECT CASE WHEN length(CAST(tag_id AS BLOB)) BETWEEN 1 AND 128 THEN tag_id END FROM meeting_tags WHERE meeting_id=?1 ORDER BY tag_id LIMIT 101")?
        .query_map([&item.id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    if tag_ids.len() > 100 {
        return Err(DataError::Invalid("fixture meeting tag count"));
    }
    let current_source_id = tx.query_row(
        "SELECT current_source_id FROM service_transcript_state WHERE meeting_id=?1",
        [&item.id],
        |r| r.get::<_, Option<String>>(0),
    )?;
    d::media(store, &mut item);
    Ok(FixtureMeeting {
        item,
        tag_ids,
        current_source_id,
    })
}

impl Store {
    pub fn fixture_read_health(&self) -> DataResult<FixtureHealth> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (index_state, search_generation) = tx.query_row(
            "SELECT state,generation FROM service_search_state WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (queued, running, waiting_for_worker) = tx.query_row(
            "SELECT count(*) FILTER(WHERE state='queued'),count(*) FILTER(WHERE state='running'),count(*) FILTER(WHERE state='waiting_for_worker') FROM service_jobs",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(FixtureHealth {
            index_state,
            search_generation,
            queued,
            running,
            waiting_for_worker,
        })
    }

    pub fn fixture_read_meeting(&self, id: &str) -> DataResult<FixtureMeeting> {
        safe_id(id)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let item = tx
            .query_row(
                &format!(
                    "SELECT {} {} WHERE m.id=?1 AND s.tombstoned=0",
                    d::MEETING_COLUMNS,
                    d::MEETING_FROM,
                ),
                [id],
                d::meeting_row,
            )
            .optional()
            .map_err(d::row_error)?
            .ok_or(DataError::NotFound)?;
        decorate_meeting(self, &tx, item)
    }

    pub fn fixture_read_meetings(
        &self,
        filter: &FixtureMeetingFilter,
        limit: u16,
        cursor: Option<SearchCursor>,
    ) -> DataResult<FixtureMeetingPage> {
        if !(1..=100).contains(&limit) {
            return Err(DataError::Invalid("fixture meetings limit"));
        }
        let filter = filter.query_json()?;
        let query_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&filter, limit))?)
        );
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (library_scope, generation) = d::snapshot_on(&tx)?;
        let offset = match cursor {
            Some(cursor)
                if cursor.library_scope == library_scope
                    && cursor.generation == generation
                    && cursor.query_hash == query_hash
                    && cursor.offset <= 1_000_000 =>
            {
                cursor.offset
            }
            Some(_) => return Err(DataError::StaleCursor),
            None => 0,
        };
        let sql = format!(
            "{FIXTURE_MEETING_TIMES} SELECT {} {} LEFT JOIN fixture_times tm ON tm.id=m.id CROSS JOIN (SELECT ?1 AS value) f WHERE s.tombstoned=0 AND ({FIXTURE_MEETING_FILTER}) ORDER BY tm.seconds DESC,tm.nanos DESC,m.id ASC LIMIT ?2 OFFSET ?3",
            d::MEETING_COLUMNS, d::MEETING_FROM,
        );
        let mut statement = tx.prepare(&sql)?;
        let mut rows = statement.query(params![filter, i64::from(limit) + 1, offset as i64])?;
        let mut items = Vec::new();
        while items.len() < usize::from(limit) {
            let Some(row) = rows.next()? else { break };
            items.push(decorate_meeting(
                self,
                &tx,
                d::meeting_row(row).map_err(d::row_error)?,
            )?);
        }
        let more = rows.next()?.is_some();
        Ok(FixtureMeetingPage {
            items,
            position: SearchCursor {
                library_scope,
                generation,
                query_hash,
                offset,
            },
            more,
        })
    }

    pub fn fixture_read_search(
        &self,
        input: &SearchRequest,
        filter: Option<&FixtureMeetingFilter>,
    ) -> DataResult<FixtureSearchPage> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (page, position) = search_on(&tx, input, filter)?;
        let mut hits = Vec::with_capacity(page.hits.len());
        for hit in page.hits {
            let (title, date) = if hit.source_kind == "note" {
                tx.query_row("SELECT CASE WHEN length(CAST(n.title AS BLOB)) BETWEEN 1 AND 512 THEN n.title END,m.started_at FROM service_notes n LEFT JOIN meetings m ON m.id=n.meeting_id WHERE n.id=?1",
                    [&hit.entity_id], |r| Ok((r.get(0)?,r.get(1)?)))?
            } else {
                tx.query_row("SELECT CASE WHEN length(CAST(m.title AS BLOB)) BETWEEN 1 AND 512 THEN m.title END,m.started_at FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id WHERE m.id=?1 AND s.tombstoned=0",
                    [&hit.meeting_id], |r| Ok((r.get(0)?,r.get(1)?)))?
            };
            hits.push(FixtureSearchHit { hit, title, date });
        }
        Ok(FixtureSearchPage {
            generation: page.generation,
            hits,
            next: page.next,
            position,
        })
    }
}

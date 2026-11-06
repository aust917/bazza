//! Read-only desktop content projections. SQLite slices BLOB bytes before they
//! cross into Rust; large legacy text never requires loading a whole document.
use super::*;
use crate::Store;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[path = "fixture_transcript.rs"]
mod fixture_transcript;
pub use fixture_transcript::*;
#[path = "fixture_summary.rs"]
mod fixture_summary;
pub use fixture_summary::*;
#[path = "fixture_notes.rs"]
mod fixture_notes;
pub use fixture_notes::*;

const MAX_DATA_BYTES: usize = 65_024;
const JS_SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTranscriptRepresentation {
    Raw,
    Effective,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEffectiveOrigin {
    Raw,
    Automatic,
    Manual,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptRequest {
    pub meeting_id: String,
    pub source_id: Option<String>,
    pub expected_revision: String,
    pub representation: DesktopTranscriptRepresentation,
    pub cursor: Option<DesktopTranscriptCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTranscriptCursor {
    pub library_scope: String,
    pub meeting_id: String,
    pub revision: String,
    pub current_source_id: Option<String>,
    pub source_id: Option<String>,
    pub representation: DesktopTranscriptRepresentation,
    pub segment_index: u64,
    pub byte_offset: u64,
    pub query_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopTranscriptFragment {
    pub segment_index: u64,
    pub segment_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub has_clean_text: bool,
    pub clean_text_manual: bool,
    pub effective_origin: DesktopEffectiveOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopTranscriptPage {
    pub library_scope: String,
    pub state: TranscriptState,
    pub source_id: Option<String>,
    pub representation: DesktopTranscriptRepresentation,
    pub items: Vec<DesktopTranscriptFragment>,
    pub next: Option<DesktopTranscriptCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopSummaryField {
    Markdown,
    StructuredJson,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSummaryRequest {
    pub meeting_id: String,
    pub expected_revision: String,
    pub field: DesktopSummaryField,
    pub cursor: Option<DesktopSummaryCursor>,
    pub max_bytes: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSummaryCursor {
    pub library_scope: String,
    pub meeting_id: String,
    pub revision: String,
    pub field: DesktopSummaryField,
    pub byte_offset: u64,
    pub query_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopSummaryPage {
    pub library_scope: String,
    pub state: SummaryState,
    pub field: DesktopSummaryField,
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub next: Option<DesktopSummaryCursor>,
}

fn limits(max: u32) -> DataResult<usize> {
    if !(1024..=65_536).contains(&max) {
        return Err(DataError::Invalid("desktop content budget"));
    }
    Ok((max as usize).min(MAX_DATA_BYTES))
}

fn scope_on(conn: &Connection) -> DataResult<String> {
    let scope: String = conn.query_row(
        "SELECT substr(cursor_scope,1,33) FROM service_search_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if scope.len() != 32 || !scope.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DataError::Invalid("desktop content scope"));
    }
    Ok(scope)
}

fn state_kind(value: String) -> rusqlite::Result<DocumentState> {
    match value.as_str() {
        "absent" => Ok(DocumentState::Absent),
        "present" => Ok(DocumentState::Present),
        "deleted" => Ok(DocumentState::Deleted),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn transcript_state(conn: &Connection, id: &str) -> DataResult<TranscriptState> {
    let state = conn.query_row("SELECT substr(meeting_id,1,129),substr(revision,1,129),substr(current_source_id,1,129),document_state,deletion_epoch FROM service_transcript_state WHERE meeting_id=?1", [id], |r| Ok(TranscriptState {
        meeting_id:r.get(0)?,revision:r.get(1)?,current_source_id:r.get(2)?,document_state:state_kind(r.get(3)?)?,deletion_epoch:r.get(4)?,
    })).optional()?.ok_or(DataError::NotFound)?;
    bounded_text(&state.revision, 1, 128)?;
    if let Some(source) = &state.current_source_id {
        safe_id(source)?;
    }
    if state.meeting_id != id || state.deletion_epoch < 0 || state.deletion_epoch as u64 > JS_SAFE {
        return Err(DataError::Invalid("transcript state"));
    }
    Ok(state)
}

fn summary_state(conn: &Connection, id: &str) -> DataResult<SummaryState> {
    let state = conn.query_row("SELECT substr(meeting_id,1,129),substr(revision,1,129),document_state,deletion_epoch,automatic_generation_suppressed,substr(origin,1,65),auto_replace,substr(source_label,1,257),substr(source_revision,1,129),source_missing FROM service_summary_state WHERE meeting_id=?1", [id], |r| Ok(SummaryState {
        meeting_id:r.get(0)?,revision:r.get(1)?,document_state:state_kind(r.get(2)?)?,deletion_epoch:r.get(3)?,automatic_generation_suppressed:r.get(4)?,origin:r.get(5)?,auto_replace:r.get(6)?,source_label:r.get(7)?,source_revision:r.get(8)?,source_missing:r.get(9)?,
    })).optional()?.ok_or(DataError::NotFound)?;
    bounded_text(&state.revision, 1, 128)?;
    bounded_text(&state.origin, 1, 64)?;
    if let Some(label) = &state.source_label {
        bounded_text(label, 0, 256)?;
    }
    if let Some(revision) = &state.source_revision {
        bounded_text(revision, 1, 128)?;
    }
    if state.meeting_id != id || state.deletion_epoch < 0 || state.deletion_epoch as u64 > JS_SAFE {
        return Err(DataError::Invalid("summary state"));
    }
    Ok(state)
}

fn fits(value: &impl Serialize, max: usize) -> bool {
    struct Budget {
        used: usize,
        max: usize,
    }
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|used| *used <= self.max)
                .ok_or_else(|| std::io::Error::other("desktop content budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget { used: 0, max }, value).is_ok()
}

fn query_hash(value: &impl Serialize) -> DataResult<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn utf8_window(bytes: &[u8]) -> DataResult<&str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
            Ok(std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("validated UTF-8 prefix"))
        }
        _ => Err(DataError::Invalid("content offset or UTF-8")),
    }
}

/// Find the largest UTF-8 prefix whose entire serialized page, including its
/// actual continuation cursor, fits. Full text is tried first because the last
/// fragment can remove the cursor and therefore reduce overhead.
pub fn fitting_prefix(text: &str, mut accepted: impl FnMut(usize) -> bool) -> usize {
    if accepted(text.len()) {
        return text.len();
    }
    let (mut low, mut high) = (0, text.len());
    while low < high {
        let mut mid = low + (high - low).div_ceil(2);
        while !text.is_char_boundary(mid) {
            mid -= 1;
        }
        if mid == low {
            mid = low + text[low..].chars().next().map_or(0, char::len_utf8);
            if mid > high || mid == low {
                break;
            }
        }
        if accepted(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

struct SegmentWindow {
    id: i64,
    start: i64,
    end: i64,
    speaker: Option<String>,
    speaker_bytes: Option<i64>,
    has_clean: bool,
    manual: bool,
    origin: DesktopEffectiveOrigin,
    total: u64,
    bytes: Vec<u8>,
}

fn segment_window(
    conn: &Connection,
    id: &str,
    source: &str,
    index: u64,
    offset: u64,
    max: usize,
    representation: DesktopTranscriptRepresentation,
) -> DataResult<Option<SegmentWindow>> {
    // Current and archived reads use the same immutable source/order and its
    // exact overlays. Mutable legacy segments are never a substitute for raw.
    // JSON stays in SQLite; j.value/raw_json never become Rust strings.
    let base = "SELECT json_extract(j.value,'$.id') id,json_extract(j.value,'$.start_ms') start_ms,json_extract(j.value,'$.end_ms') end_ms,CASE WHEN p.segment_key IS NOT NULL THEN p.label ELSE json_extract(j.value,'$.speaker') END speaker,json_extract(j.value,'$.text') text,o.clean_text,COALESCE(o.manual,0) clean_text_manual FROM service_transcript_sources s,json_each(s.raw_json) j LEFT JOIN service_transcript_overlays o ON o.source_id=s.id AND o.segment_key=CAST(json_extract(j.value,'$.id') AS TEXT) LEFT JOIN service_transcript_speaker_overlays p ON p.source_id=s.id AND p.segment_key=CAST(json_extract(j.value,'$.id') AS TEXT) WHERE s.meeting_id=?1 AND s.id=?2 AND j.key=?3";
    let sql=format!("WITH item AS ({base}), chosen AS (SELECT *,COALESCE(clean_text,text) effective,CASE WHEN clean_text IS NOT NULL AND clean_text_manual=1 THEN 2 WHEN clean_text IS NOT NULL THEN 1 ELSE 0 END effective_origin FROM item), representation AS (SELECT *,CASE WHEN ?6=0 THEN text ELSE effective END selected FROM chosen) SELECT id,start_ms,end_ms,substr(speaker,1,257),length(CAST(speaker AS BLOB)),clean_text IS NOT NULL,clean_text_manual,effective_origin,length(CAST(selected AS BLOB)),COALESCE(substr(CAST(selected AS BLOB),?4+1,?5),X'') FROM representation");
    let row = conn
        .query_row(
            &sql,
            params![
                id,
                source,
                index as i64,
                offset as i64,
                max as i64,
                if representation == DesktopTranscriptRepresentation::Raw {
                    0
                } else {
                    1
                }
            ],
            |r| {
                let origin = match r.get::<_, i64>(7)? {
                    0 => DesktopEffectiveOrigin::Raw,
                    1 => DesktopEffectiveOrigin::Automatic,
                    2 => DesktopEffectiveOrigin::Manual,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                let total: i64 = r.get(8)?;
                if total < 0 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(SegmentWindow {
                    id: r.get(0)?,
                    start: r.get(1)?,
                    end: r.get(2)?,
                    speaker: r.get(3)?,
                    speaker_bytes: r.get(4)?,
                    has_clean: r.get(5)?,
                    manual: r.get(6)?,
                    origin,
                    total: total as u64,
                    bytes: r.get(9)?,
                })
            },
        )
        .optional()?;
    if let Some(row) = &row {
        if row.id < 0
            || row.id as u64 > JS_SAFE
            || row.start < 0
            || row.end < row.start
            || row.end as u64 > JS_SAFE
            || row.total > JS_SAFE
            || row.speaker_bytes.is_some_and(|n| !(0..=256).contains(&n))
            || row.speaker.as_ref().is_some_and(|s| s.contains('\0'))
            || (row.manual && !row.has_clean)
        {
            return Err(DataError::Invalid("transcript segment metadata"));
        }
        if offset > row.total || (offset > 0 && offset == row.total) {
            return Err(DataError::Invalid("transcript fragment offset"));
        }
    }
    Ok(row)
}

pub(super) fn transcript_on(
    conn: &Connection,
    request: &DesktopTranscriptRequest,
) -> DataResult<DesktopTranscriptPage> {
    safe_id(&request.meeting_id)?;
    bounded_text(&request.expected_revision, 1, 128)?;
    if let Some(source) = &request.source_id {
        safe_id(source)?;
    }
    if !(1..=100).contains(&request.limit) {
        return Err(DataError::Invalid("transcript fragment limit"));
    }
    let max = limits(request.max_bytes)?;
    let hash = query_hash(&(
        &request.meeting_id,
        &request.source_id,
        &request.expected_revision,
        request.representation,
        request.limit,
        request.max_bytes,
    ))?;
    visible_meeting(conn, &request.meeting_id)?;
    let scope = scope_on(conn)?;
    let state = transcript_state(conn, &request.meeting_id)?;
    let source = request
        .source_id
        .clone()
        .or_else(|| state.current_source_id.clone());
    if request.cursor.as_ref().is_some_and(|c| {
        c.library_scope != scope
            || c.meeting_id != request.meeting_id
            || c.revision != state.revision
            || c.current_source_id != state.current_source_id
            || c.source_id != source
            || c.representation != request.representation
            || c.query_hash != hash
            || c.segment_index > JS_SAFE
            || c.byte_offset > JS_SAFE
            || c.source_id.is_none()
            || (c.segment_index == 0 && c.byte_offset == 0)
    }) {
        return Err(DataError::StaleCursor);
    }
    check_revision(&state.revision, &request.expected_revision)?;
    let mut page = DesktopTranscriptPage {
        library_scope: scope.clone(),
        state: state.clone(),
        source_id: source.clone(),
        representation: request.representation,
        items: vec![],
        next: None,
    };
    let Some(source) = source else {
        if state.document_state == DocumentState::Present {
            return Err(DataError::Invalid("transcript source missing"));
        }
        if !fits(&page, max) {
            return Err(DataError::Invalid("transcript metadata budget"));
        }
        return Ok(page);
    };
    let count:i64=conn.query_row("SELECT json_array_length(raw_json) FROM service_transcript_sources WHERE id=?1 AND meeting_id=?2 AND json_type(raw_json)='array'",params![source,request.meeting_id],|r|r.get(0)).optional()?.ok_or(DataError::NotFound)?;
    if count < 0 || count as u64 > JS_SAFE {
        return Err(DataError::Invalid("transcript segment count"));
    }
    let count = count as u64;
    let mut index = request.cursor.as_ref().map_or(0, |c| c.segment_index);
    let mut offset = request.cursor.as_ref().map_or(0, |c| c.byte_offset);
    if request.cursor.is_some() && index >= count {
        return Err(DataError::StaleCursor);
    }
    let next_at = |segment_index, byte_offset| DesktopTranscriptCursor {
        library_scope: scope.clone(),
        meeting_id: request.meeting_id.clone(),
        revision: state.revision.clone(),
        current_source_id: state.current_source_id.clone(),
        source_id: Some(source.clone()),
        representation: request.representation,
        segment_index,
        byte_offset,
        query_hash: hash.clone(),
    };
    while index < count && page.items.len() < usize::from(request.limit) {
        let row = segment_window(
            conn,
            &request.meeting_id,
            &source,
            index,
            offset,
            max,
            request.representation,
        )?
        .ok_or(DataError::StaleCursor)?;
        let window = utf8_window(&row.bytes)?;
        let fragment = DesktopTranscriptFragment {
            segment_index: index,
            segment_id: row.id,
            start_ms: row.start,
            end_ms: row.end,
            speaker: row.speaker,
            text: String::new(),
            byte_offset: offset,
            total_bytes: row.total,
            has_clean_text: row.has_clean,
            clean_text_manual: row.manual,
            effective_origin: row.origin,
        };
        page.items.push(fragment);
        let next_for = |len: usize| {
            let end = offset + len as u64;
            if end < row.total {
                Some(next_at(index, end))
            } else if index + 1 < count {
                Some(next_at(index + 1, 0))
            } else {
                None
            }
        };
        let length = fitting_prefix(window, |length| {
            page.items.last_mut().unwrap().text = window[..length].into();
            page.next = next_for(length);
            fits(&page, max)
        });
        page.items.last_mut().unwrap().text = window[..length].into();
        page.next = next_for(length);
        if !fits(&page, max) || (length == 0 && row.total > offset) {
            page.items.pop();
            page.next = Some(next_at(index, offset));
            if page.items.is_empty() {
                return Err(DataError::Invalid("transcript metadata budget"));
            }
            break;
        }
        if offset + (length as u64) < row.total {
            break;
        }
        index += 1;
        offset = 0;
    }
    if !fits(&page, max) {
        return Err(DataError::Invalid("transcript page budget"));
    }
    Ok(page)
}

impl Store {
    pub fn service_desktop_transcript(
        &self,
        request: &DesktopTranscriptRequest,
    ) -> DataResult<DesktopTranscriptPage> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        transcript_on(&tx, request)
    }

    pub fn service_desktop_summary(
        &self,
        request: &DesktopSummaryRequest,
    ) -> DataResult<DesktopSummaryPage> {
        safe_id(&request.meeting_id)?;
        bounded_text(&request.expected_revision, 1, 128)?;
        let max = limits(request.max_bytes)?;
        let hash = query_hash(&(
            &request.meeting_id,
            &request.expected_revision,
            request.field,
            request.max_bytes,
        ))?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        visible_meeting(&tx, &request.meeting_id)?;
        let scope = scope_on(&tx)?;
        let state = summary_state(&tx, &request.meeting_id)?;
        if request.cursor.as_ref().is_some_and(|c| {
            c.library_scope != scope
                || c.meeting_id != request.meeting_id
                || c.revision != state.revision
                || c.field != request.field
                || c.query_hash != hash
                || c.byte_offset > JS_SAFE
                || c.byte_offset == 0
        }) {
            return Err(DataError::StaleCursor);
        }
        check_revision(&state.revision, &request.expected_revision)?;
        let offset = request.cursor.as_ref().map_or(0, |c| c.byte_offset);
        let mut page = DesktopSummaryPage {
            library_scope: scope.clone(),
            state: state.clone(),
            field: request.field,
            text: String::new(),
            byte_offset: offset,
            total_bytes: 0,
            next: None,
        };
        if state.document_state != DocumentState::Present {
            if request.cursor.is_some() {
                return Err(DataError::StaleCursor);
            }
            if !fits(&page, max) {
                return Err(DataError::Invalid("summary metadata budget"));
            }
            return Ok(page);
        }
        let field = match request.field {
            DesktopSummaryField::Markdown => "markdown",
            DesktopSummaryField::StructuredJson => "structured_json",
        };
        let sql=format!("SELECT length(CAST({field} AS BLOB)),COALESCE(substr(CAST({field} AS BLOB),?2+1,?3),X'') FROM summaries WHERE meeting_id=?1");
        let (total, bytes): (i64, Vec<u8>) = tx
            .query_row(
                &sql,
                params![request.meeting_id, offset as i64, max as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(DataError::Invalid("summary projection missing"))?;
        if total < 0
            || total as u64 > JS_SAFE
            || offset > total as u64
            || (offset > 0 && offset == total as u64)
        {
            return Err(DataError::Invalid("summary fragment offset"));
        }
        page.total_bytes = total as u64;
        let window = utf8_window(&bytes)?;
        let length = fitting_prefix(window, |length| {
            page.text = window[..length].into();
            page.next = if offset + (length as u64) < total as u64 {
                Some(DesktopSummaryCursor {
                    library_scope: scope.clone(),
                    meeting_id: request.meeting_id.clone(),
                    revision: state.revision.clone(),
                    field: request.field,
                    byte_offset: offset + length as u64,
                    query_hash: hash.clone(),
                })
            } else {
                None
            };
            fits(&page, max)
        });
        page.text = window[..length].into();
        page.next = if offset + (length as u64) < total as u64 {
            Some(DesktopSummaryCursor {
                library_scope: scope,
                meeting_id: request.meeting_id.clone(),
                revision: state.revision.clone(),
                field: request.field,
                byte_offset: offset + length as u64,
                query_hash: hash,
            })
        } else {
            None
        };
        if !fits(&page, max) || (length == 0 && total as u64 > offset) {
            return Err(DataError::Invalid("summary metadata budget"));
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ctx(key: &str) -> MutationContext {
        MutationContext {
            principal: "desktop-content-test".into(),
            idempotency_key: key.into(),
        }
    }

    fn meeting(store: &Store, with_transcript: bool) -> String {
        store
            .service_create_text_meeting(
                &ctx(&revision()),
                &NewTextMeeting {
                    title: "Искусственная встреча".into(),
                    started_at: Utc::now(),
                    participants: vec![],
                    folder_id: None,
                    tag_ids: vec![],
                    calendar_uid: None,
                    transcript: with_transcript.then(|| {
                        (0..4)
                            .map(|i| crate::NewSegment {
                                start_ms: i * 1000,
                                end_ms: (i + 1) * 1000,
                                speaker: None,
                                text: format!("raw {i}"),
                            })
                            .collect()
                    }),
                },
            )
            .unwrap()
            .entity_id
    }

    fn transcript_request(store: &Store, id: &str) -> DesktopTranscriptRequest {
        DesktopTranscriptRequest {
            meeting_id: id.into(),
            source_id: None,
            expected_revision: store.service_transcript_state(id).unwrap().revision,
            representation: DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 3,
            max_bytes: 2048,
        }
    }

    fn summary_request(
        store: &Store,
        id: &str,
        field: DesktopSummaryField,
    ) -> DesktopSummaryRequest {
        DesktopSummaryRequest {
            meeting_id: id.into(),
            expected_revision: store.service_summary_state(id).unwrap().revision,
            field,
            cursor: None,
            max_bytes: 2048,
        }
    }

    fn summary_fixture(store: &Store, id: &str, markdown: &str, structured: &str) {
        let conn = store.conn.lock();
        conn.execute("INSERT INTO summaries(meeting_id,template_id,markdown,structured_json,model,created_at) VALUES(?1,'general',?2,?3,'synthetic',?4)",params![id,markdown,structured,Utc::now()]).unwrap();
        conn.execute("UPDATE service_summary_state SET document_state='present',revision=?2 WHERE meeting_id=?1",params![id,revision()]).unwrap();
    }

    fn current_source_fixture(store: &Store, id: &str, texts: &[String]) -> String {
        let source = revision();
        let raw:Vec<_>=texts.iter().enumerate().map(|(index,text)|serde_json::json!({
            "id":100+index,"start_ms":index*1000,"end_ms":(index+1)*1000,"speaker":null,"text":text
        })).collect();
        let conn = store.conn.lock();
        conn.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,raw_json,created_at) VALUES(?1,?2,'legacy',?3,?4)",params![source,id,serde_json::to_string(&raw).unwrap(),Utc::now()]).unwrap();
        conn.execute("UPDATE service_transcript_state SET current_source_id=?2,revision=?3,document_state='present' WHERE meeting_id=?1",params![id,source,revision()]).unwrap();
        source
    }

    fn collect_transcript(
        store: &Store,
        mut request: DesktopTranscriptRequest,
        expected: &[String],
    ) -> Vec<DesktopTranscriptFragment> {
        let mut result = vec![String::new(); expected.len()];
        let mut fragments = Vec::new();
        let max = (request.max_bytes as usize).min(MAX_DATA_BYTES);
        for _ in 0..1500 {
            let page = store.service_desktop_transcript(&request).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= max);
            assert!(page.items.len() <= usize::from(request.limit));
            for item in page.items {
                let index = item.segment_index as usize;
                assert_eq!(item.byte_offset, result[index].len() as u64);
                assert_eq!(item.total_bytes, expected[index].len() as u64);
                assert!(
                    !item.text.is_empty() || item.total_bytes == 0,
                    "nonempty segment must progress"
                );
                result[index].push_str(&item.text);
                fragments.push(item);
            }
            if let Some(next) = page.next {
                if let Some(previous) = request.cursor.as_ref() {
                    assert!(
                        (next.segment_index, next.byte_offset)
                            > (previous.segment_index, previous.byte_offset)
                    );
                }
                request.cursor = Some(next);
            } else {
                assert_eq!(result, expected);
                return fragments;
            }
        }
        panic!("transcript continuation stalled");
    }

    fn collect_summary(store: &Store, mut request: DesktopSummaryRequest, expected: &str) {
        let mut result = String::new();
        let max = (request.max_bytes as usize).min(MAX_DATA_BYTES);
        for _ in 0..1500 {
            let page = store.service_desktop_summary(&request).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= max);
            assert_eq!(page.byte_offset, result.len() as u64);
            assert_eq!(page.total_bytes, expected.len() as u64);
            assert!(!page.text.is_empty() || expected.is_empty());
            result.push_str(&page.text);
            if let Some(next) = page.next {
                assert_eq!(next.byte_offset, result.len() as u64);
                request.cursor = Some(next);
            } else {
                assert_eq!(result, expected);
                return;
            }
        }
        panic!("summary continuation stalled");
    }

    #[test]
    fn giant_current_utf8_raw_and_effective_fragments_preserve_empty_manual_and_nul() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, true);
        let giant = "Р🙂\0\"\n".repeat(12000);
        let raw = vec![giant.clone(), "raw 1".into(), String::new(), "raw 3".into()];
        let source = current_source_fixture(&store, &id, &raw);
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,'101','',1),(?1,'102','',0),(?1,'103','Автоматический текст',0)",[&source]).unwrap();
            // A mutable projection may be corrupted by an unenrolled legacy
            // writer. Raw/order/count and effective overlays stay source-backed.
            conn.execute(
                "DELETE FROM segments WHERE meeting_id=?1 AND start_ms=2000",
                [&id],
            )
            .unwrap();
            conn.execute("UPDATE segments SET text='divergent projection',clean_text='divergent overlay',clean_text_manual=1,start_ms=10000-start_ms WHERE meeting_id=?1",[&id]).unwrap();
        }
        let fragments = collect_transcript(&store, transcript_request(&store, &id), &raw);
        assert!(fragments.iter().filter(|f| f.segment_index == 0).count() > 1);
        let mut request = transcript_request(&store, &id);
        request.representation = DesktopTranscriptRepresentation::Effective;
        let effective = vec![
            giant,
            String::new(),
            String::new(),
            "Автоматический текст".into(),
        ];
        let fragments = collect_transcript(&store, request, &effective);
        let manual = fragments.iter().find(|f| f.segment_index == 1).unwrap();
        assert!(manual.has_clean_text && manual.clean_text_manual);
        assert_eq!(manual.effective_origin, DesktopEffectiveOrigin::Manual);
        assert_eq!(manual.total_bytes, 0);
        let automatic_empty = fragments.iter().find(|f| f.segment_index == 2).unwrap();
        assert!(automatic_empty.has_clean_text && !automatic_empty.clean_text_manual);
        assert_eq!(
            automatic_empty.effective_origin,
            DesktopEffectiveOrigin::Automatic
        );
        assert_eq!(automatic_empty.total_bytes, 0);
        assert_eq!(
            fragments
                .iter()
                .find(|f| f.segment_index == 3)
                .unwrap()
                .effective_origin,
            DesktopEffectiveOrigin::Automatic
        );
    }

    #[test]
    fn archived_json_is_paged_without_losing_overlay_or_giant_raw_text() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, true);
        let source = revision();
        let giant = "архив🙂\n\"".repeat(16000);
        let raw = serde_json::to_string(&serde_json::json!([
            {"id":111,"start_ms":0,"end_ms":1000,"speaker":"Анна","text":giant},
            {"id":112,"start_ms":1000,"end_ms":2000,"speaker":null,"text":"archived raw"}
        ]))
        .unwrap();
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,raw_json,created_at) VALUES(?1,?2,'legacy',?3,?4)",params![source,id,raw,Utc::now()]).unwrap();
            conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,'112','',1)",[&source]).unwrap();
        }
        let mut request = transcript_request(&store, &id);
        request.source_id = Some(source);
        request.max_bytes = 65536;
        collect_transcript(
            &store,
            request.clone(),
            &[giant.clone(), "archived raw".into()],
        );
        request.representation = DesktopTranscriptRepresentation::Effective;
        let fragments = collect_transcript(&store, request, &[giant, String::new()]);
        assert!(fragments.last().unwrap().clean_text_manual);
        assert_eq!(fragments.last().unwrap().total_bytes, 0);
    }

    #[test]
    fn giant_legacy_summary_fields_progress_with_exact_utf8_and_serialized_budgets() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, false);
        let markdown = "🙂\"\0\nБюджет".repeat(20000);
        let structured = serde_json::to_string(&serde_json::json!({"legacy":markdown})).unwrap();
        summary_fixture(&store, &id, &markdown, &structured);
        collect_summary(
            &store,
            summary_request(&store, &id, DesktopSummaryField::Markdown),
            &markdown,
        );
        let mut request = summary_request(&store, &id, DesktopSummaryField::StructuredJson);
        request.max_bytes = 65536;
        collect_summary(&store, request, &structured);
        // Read paths do not silently repair or parse a legacy structured body.
        store
            .conn
            .lock()
            .execute(
                "UPDATE summaries SET structured_json='legacy invalid JSON' WHERE meeting_id=?1",
                [&id],
            )
            .unwrap();
        collect_summary(
            &store,
            summary_request(&store, &id, DesktopSummaryField::StructuredJson),
            "legacy invalid JSON",
        );
    }

    #[test]
    fn transcript_cursors_reject_changed_scope_revision_source_query_and_mid_utf8() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, true);
        current_source_fixture(
            &store,
            &id,
            &[
                "🙂".repeat(4000),
                "one".into(),
                "two".into(),
                "three".into(),
            ],
        );
        let mut request = transcript_request(&store, &id);
        request.max_bytes = 1024;
        let first = store.service_desktop_transcript(&request).unwrap();
        request.cursor = Some(first.next.unwrap());
        for variant in [
            "scope",
            "meeting",
            "revision",
            "source",
            "representation",
            "limit",
            "budget",
        ] {
            let mut bad = request.clone();
            match variant {
                "scope" => bad.cursor.as_mut().unwrap().library_scope = "a".repeat(32),
                "meeting" => bad.cursor.as_mut().unwrap().meeting_id = "other".into(),
                "revision" => bad.cursor.as_mut().unwrap().revision = "other".into(),
                "source" => bad.source_id = Some("different-source".into()),
                "representation" => bad.representation = DesktopTranscriptRepresentation::Effective,
                "limit" => bad.limit = 2,
                "budget" => bad.max_bytes = 2048,
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    store.service_desktop_transcript(&bad),
                    Err(DataError::StaleCursor)
                ),
                "{variant}"
            );
        }
        let mut invalid = request.clone();
        invalid.cursor.as_mut().unwrap().byte_offset = 1;
        assert!(matches!(
            store.service_desktop_transcript(&invalid),
            Err(DataError::Invalid(_))
        ));
        invalid.cursor.as_mut().unwrap().byte_offset = 16000;
        assert!(matches!(
            store.service_desktop_transcript(&invalid),
            Err(DataError::Invalid(_))
        ));
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_transcript_state SET revision=?2 WHERE meeting_id=?1",
                params![id, revision()],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_transcript(&request),
            Err(DataError::StaleCursor)
        ));
        request.cursor = None;
        assert!(matches!(
            store.service_desktop_transcript(&request),
            Err(DataError::Conflict { .. })
        ));
    }

    #[test]
    fn summary_cursors_bind_scope_field_revision_and_utf8_offsets() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, false);
        let giant = "🙂".repeat(4000);
        summary_fixture(&store, &id, &giant, "{}");
        let mut request = summary_request(&store, &id, DesktopSummaryField::Markdown);
        request.cursor = store.service_desktop_summary(&request).unwrap().next;
        for variant in ["scope", "field", "revision", "meeting", "budget"] {
            let mut bad = request.clone();
            match variant {
                "scope" => bad.cursor.as_mut().unwrap().library_scope = "a".repeat(32),
                "field" => bad.field = DesktopSummaryField::StructuredJson,
                "revision" => bad.cursor.as_mut().unwrap().revision = "different".into(),
                "meeting" => bad.cursor.as_mut().unwrap().meeting_id = "other".into(),
                "budget" => bad.max_bytes = 1024,
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    store.service_desktop_summary(&bad),
                    Err(DataError::StaleCursor)
                ),
                "{variant}"
            );
        }
        let mut bad = request.clone();
        bad.cursor.as_mut().unwrap().byte_offset = 1;
        assert!(matches!(
            store.service_desktop_summary(&bad),
            Err(DataError::Invalid(_))
        ));
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_search_state SET cursor_scope=?1",
                [revision().replace('-', "")],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_summary(&request),
            Err(DataError::StaleCursor)
        ));
        let mut fresh = summary_request(&store, &id, DesktopSummaryField::Markdown);
        fresh.cursor = store.service_desktop_summary(&fresh).unwrap().next;
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_summary_state SET revision=?2 WHERE meeting_id=?1",
                params![id, revision()],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_summary(&fresh),
            Err(DataError::StaleCursor)
        ));
    }

    #[test]
    fn absent_deleted_and_empty_documents_keep_persistent_state_without_fake_fragments() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, false);
        let initial_t = transcript_request(&store, &id);
        let initial_s = summary_request(&store, &id, DesktopSummaryField::Markdown);
        let absent_t = store.service_desktop_transcript(&initial_t).unwrap();
        let absent_s = store.service_desktop_summary(&initial_s).unwrap();
        assert_eq!(absent_t.state.document_state, DocumentState::Absent);
        assert!(absent_t.items.is_empty() && absent_t.next.is_none());
        assert_eq!(absent_s.state.document_state, DocumentState::Absent);
        assert!(absent_s.text.is_empty() && absent_s.next.is_none());
        store
            .service_delete_transcript(&ctx("delete-transcript"), &id, &initial_t.expected_revision)
            .unwrap();
        store
            .service_delete_summary(&ctx("delete-summary"), &id, &initial_s.expected_revision)
            .unwrap();
        let deleted_t = store
            .service_desktop_transcript(&transcript_request(&store, &id))
            .unwrap();
        let deleted_s = store
            .service_desktop_summary(&summary_request(&store, &id, DesktopSummaryField::Markdown))
            .unwrap();
        assert_eq!(deleted_t.state.document_state, DocumentState::Deleted);
        assert_eq!(deleted_s.state.document_state, DocumentState::Deleted);
        assert_ne!(deleted_t.state.revision, initial_t.expected_revision);
        assert_ne!(deleted_s.state.revision, initial_s.expected_revision);
        assert!(deleted_t.items.is_empty() && deleted_t.next.is_none());
        assert!(deleted_s.text.is_empty() && deleted_s.next.is_none());
        assert!(deleted_s.state.automatic_generation_suppressed);
        assert!(matches!(
            store.service_desktop_transcript(&initial_t),
            Err(DataError::Conflict { .. })
        ));

        let other = meeting(&store, false);
        summary_fixture(&store, &other, "", "");
        collect_summary(
            &store,
            summary_request(&store, &other, DesktopSummaryField::Markdown),
            "",
        );
    }

    #[test]
    fn content_reads_refuse_foreign_sources_tombstones_and_invalid_budgets() {
        let store = Store::open_in_memory().unwrap();
        let id = meeting(&store, true);
        let other = meeting(&store, true);
        let mut request = transcript_request(&store, &id);
        request.source_id = store
            .service_transcript_state(&other)
            .unwrap()
            .current_source_id;
        assert!(matches!(
            store.service_desktop_transcript(&request),
            Err(DataError::NotFound)
        ));
        request.source_id = None;
        for max in [0, 1023, 65537] {
            request.max_bytes = max;
            assert!(matches!(
                store.service_desktop_transcript(&request),
                Err(DataError::Invalid(_))
            ));
        }
        request.max_bytes = 2048;
        request.limit = 101;
        assert!(matches!(
            store.service_desktop_transcript(&request),
            Err(DataError::Invalid(_))
        ));
        request.limit = 1;
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_meeting_state SET tombstoned=1 WHERE meeting_id=?1",
                [&id],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_transcript(&request),
            Err(DataError::NotFound)
        ));
        let summary = summary_request(&store, &other, DesktopSummaryField::Markdown);
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_meeting_state SET tombstoned=1 WHERE meeting_id=?1",
                [&other],
            )
            .unwrap();
        assert!(matches!(
            store.service_desktop_summary(&summary),
            Err(DataError::NotFound)
        ));
    }
}

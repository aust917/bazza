//! Service-only document mutations. Original timed text is immutable; the
//! legacy `segments`/`summaries` tables remain current projections for cutover.
use super::*;
use crate::{NewSegment, Store, Summary};
use bazza_domain::agent_summary::AgentSummaryCommit;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const DOCUMENT_BYTES: usize = 262_144;
const FRAGMENT_BYTES: usize = 65_536;
const MAX_TIME_MS: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentState {
    Absent,
    Present,
    Deleted,
}

fn document_state(value: String) -> rusqlite::Result<DocumentState> {
    match value.as_str() {
        "absent" => Ok(DocumentState::Absent),
        "present" => Ok(DocumentState::Present),
        "deleted" => Ok(DocumentState::Deleted),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptState {
    pub meeting_id: String,
    pub revision: String,
    pub current_source_id: Option<String>,
    pub document_state: DocumentState,
    pub deletion_epoch: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptImportOrigin {
    Imported,
    ExternalAgent,
}

impl TranscriptImportOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::ExternalAgent => "external_agent",
        }
    }
}

/// Origin is supplied by the trusted application boundary, not an external
/// tool's unchecked claim. The public contract has no origin parameter.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptWrite {
    pub expected_revision: String,
    pub segments: Vec<NewSegment>,
    pub source_label: Option<String>,
    pub language: Option<String>,
    pub origin: TranscriptImportOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptCorrection {
    pub segment_id: i64,
    /// Some("") is an explicit empty manual overlay; None resets to raw.
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptCorrections {
    pub expected_revision: String,
    pub corrections: Vec<TranscriptCorrection>,
}

#[derive(Clone, Debug)]
pub struct TranscriptRead {
    pub source_id: Option<String>,
    pub expected_revision: Option<String>,
    pub offset: usize,
    pub limit: usize,
    pub max_bytes: usize,
}

impl Default for TranscriptRead {
    fn default() -> Self {
        Self {
            source_id: None,
            expected_revision: None,
            offset: 0,
            limit: 100,
            max_bytes: FRAGMENT_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSourceSegment {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
    pub clean_text: Option<String>,
    pub clean_text_manual: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptSourcePage {
    pub state: TranscriptState,
    pub source_id: Option<String>,
    pub origin: Option<String>,
    pub source_label: Option<String>,
    pub language: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub segments: Vec<TranscriptSourceSegment>,
    pub next_offset: Option<usize>,
}

type TranscriptSourceMetadata = (String, Option<String>, Option<String>, DateTime<Utc>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryState {
    pub meeting_id: String,
    pub revision: String,
    pub document_state: DocumentState,
    pub deletion_epoch: i64,
    pub automatic_generation_suppressed: bool,
    pub origin: String,
    pub auto_replace: bool,
    pub source_label: Option<String>,
    pub source_revision: Option<String>,
    pub source_missing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummarySnapshot {
    pub state: SummaryState,
    pub current: Option<Summary>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryWriteMode {
    Create,
    Replace,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryAuthor {
    Manual,
    ExternalAgent,
}

impl SummaryAuthor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ExternalAgent => "external_agent",
        }
    }
}

/// `author` is assigned by the trusted caller, as for transcript origin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryWrite {
    pub expected_revision: String,
    pub mode: SummaryWriteMode,
    pub markdown: String,
    pub structured_json: serde_json::Value,
    pub source_label: Option<String>,
    pub author: SummaryAuthor,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedSummary {
    pub expected_summary_revision: String,
    pub expected_transcript_revision: String,
    pub markdown: String,
    pub structured_json: serde_json::Value,
    pub template_id: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryVersion {
    pub id: String,
    pub revision: String,
    pub summary: Summary,
    pub origin: String,
    pub auto_replace: bool,
    pub source_label: Option<String>,
    pub source_revision: Option<String>,
}

pub(super) fn transcript_state_on(conn: &Connection, id: &str) -> DataResult<TranscriptState> {
    conn.query_row(
        "SELECT meeting_id,revision,current_source_id,document_state,deletion_epoch FROM service_transcript_state WHERE meeting_id=?1",
        [id], |r| Ok(TranscriptState { meeting_id:r.get(0)?,revision:r.get(1)?,current_source_id:r.get(2)?,
            document_state:document_state(r.get(3)?)?,deletion_epoch:r.get(4)? }))
        .optional()?.ok_or(DataError::NotFound)
}

pub(super) fn summary_state_on(conn: &Connection, id: &str) -> DataResult<SummaryState> {
    conn.query_row(
        "SELECT meeting_id,revision,document_state,deletion_epoch,automatic_generation_suppressed,origin,auto_replace,source_label,source_revision,source_missing FROM service_summary_state WHERE meeting_id=?1",
        [id], |r| Ok(SummaryState {meeting_id:r.get(0)?,revision:r.get(1)?,document_state:document_state(r.get(2)?)?,deletion_epoch:r.get(3)?,
            automatic_generation_suppressed:r.get(4)?,origin:r.get(5)?,auto_replace:r.get(6)?,source_label:r.get(7)?,source_revision:r.get(8)?,source_missing:r.get(9)?}))
        .optional()?.ok_or(DataError::NotFound)
}

pub(super) fn validate_segments(input: &TranscriptWrite) -> DataResult<()> {
    if input.segments.is_empty() || input.segments.len() > 1000 {
        return Err(DataError::Invalid("transcript segment count"));
    }
    if let Some(label) = &input.source_label {
        bounded_text(label, 1, 256)?;
    }
    if let Some(language) = &input.language {
        bounded_text(language, 2, 64)?;
        if !language.split('-').all(|part| {
            !part.is_empty() && part.len() <= 8 && part.bytes().all(|c| c.is_ascii_alphanumeric())
        }) || !language
            .split('-')
            .next()
            .is_some_and(|part| part.len() >= 2 && part.bytes().all(|c| c.is_ascii_alphabetic()))
        {
            return Err(DataError::Invalid("transcript language"));
        }
    }
    let mut last_start = -1;
    for segment in &input.segments {
        if segment.start_ms < 0
            || segment.start_ms < last_start
            || segment.end_ms <= segment.start_ms
            || segment.end_ms > MAX_TIME_MS
        {
            return Err(DataError::Invalid("transcript timing"));
        }
        bounded_text(&segment.text, 0, FRAGMENT_BYTES)?;
        if let Some(speaker) = &segment.speaker {
            bounded_text(speaker, 1, 256)?;
        }
        last_start = segment.start_ms;
    }
    Ok(())
}

/// Install a new raw source and replace the current projection in one caller's
/// transaction. Previously captured overlays stay attached to the old source.
pub(super) fn replace_transcript_on(
    tx: &Transaction<'_>,
    id: &str,
    old: &TranscriptState,
    input: &TranscriptWrite,
) -> DataResult<String> {
    // A complete combined result owns the canonical projection and an archived
    // initial version. Later recognition belongs in a separate version; generic
    // raw replacement would detach that archive/effective pointer from search.
    if crate::transcript_versions::has_combined_publication_on(tx, id)? {
        return Err(DataError::Busy);
    }
    if let Some(source_id) = &old.current_source_id {
        tx.execute(
            "DELETE FROM service_transcript_overlays WHERE source_id=?1",
            [source_id],
        )?;
        tx.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) SELECT ?1,CAST(id AS TEXT),clean_text,clean_text_manual FROM segments WHERE meeting_id=?2 AND clean_text IS NOT NULL",
            params![source_id,id])?;
    }
    tx.execute("DELETE FROM segments WHERE meeting_id=?1", [id])?;
    tx.execute("DELETE FROM meeting_speakers WHERE meeting_id=?1", [id])?;
    let mut raw = Vec::with_capacity(input.segments.len());
    {
        let mut insert = tx.prepare(
            "INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for segment in &input.segments {
            insert.execute(params![
                id,
                segment.start_ms,
                segment.end_ms,
                segment.speaker,
                segment.text
            ])?;
            raw.push(
                serde_json::json!({"id":tx.last_insert_rowid(),"start_ms":segment.start_ms,
                "end_ms":segment.end_ms,"speaker":segment.speaker,"text":segment.text}),
            );
        }
    }
    let source_id = revision();
    tx.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,language,raw_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![source_id,id,input.origin.as_str(),input.source_label,input.language,serde_json::to_string(&raw)?,Utc::now()])?;
    let rev = revision();
    tx.execute("UPDATE service_transcript_state SET revision=?2,current_source_id=?3,document_state='present' WHERE meeting_id=?1",params![id,rev,source_id])?;
    tx.execute("UPDATE meetings SET duration_ms=?2 WHERE id=?1 AND EXISTS(SELECT 1 FROM service_meeting_state s WHERE s.meeting_id=?1 AND s.content_kind='text')",params![id,input.segments.iter().map(|s|s.end_ms).max().unwrap_or(0)])?;
    // Old provider copies describe the replaced source and must not be used by a
    // later reprocess as if they were the new imported transcript.
    tx.execute(
        "DELETE FROM settings WHERE key IN (?1,?2)",
        params![format!("stt:{id}"), format!("stt_previous:{id}")],
    )?;
    crate::settings::clear_meeting_voice_queries_on(tx, id)?;
    crate::ai::rebuild_chunks_on(tx, id)?;
    super::search::rebuild_meeting_on(tx, id)?;
    Ok(rev)
}

fn archive_summary_on(tx: &Transaction<'_>, id: &str, state: &SummaryState) -> DataResult<()> {
    tx.execute("INSERT INTO service_summary_history(id,meeting_id,revision,template_id,markdown,structured_json,model,created_at,origin,auto_replace,source_label,source_revision) SELECT ?1,meeting_id,?2,template_id,markdown,structured_json,model,created_at,?3,?4,?5,?6 FROM summaries WHERE meeting_id=?7",
        params![revision(),state.revision,state.origin,state.auto_replace,state.source_label,state.source_revision,id])?;
    Ok(())
}

fn validate_summary(markdown: &str, structured: &serde_json::Value) -> DataResult<()> {
    bounded_text(markdown, 0, DOCUMENT_BYTES)?;
    bounded_text(&serde_json::to_string(structured)?, 0, DOCUMENT_BYTES)
}

fn write_summary_on(
    tx: &Transaction<'_>,
    id: &str,
    markdown: &str,
    structured: &serde_json::Value,
    template: &str,
    model: &str,
) -> DataResult<()> {
    tx.execute("INSERT INTO summaries(meeting_id,template_id,markdown,structured_json,model,created_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(meeting_id) DO UPDATE SET template_id=excluded.template_id,markdown=excluded.markdown,structured_json=excluded.structured_json,model=excluded.model,created_at=excluded.created_at",
        params![id,template,markdown,serde_json::to_string(structured)?,model,Utc::now()])?;
    Ok(())
}

pub(super) fn install_generated_summary_on(
    tx: &Transaction<'_>,
    meeting_id: &str,
    input: &GeneratedSummary,
    expected_source_id: Option<&str>,
) -> DataResult<String> {
    live_meeting(tx, meeting_id)?;
    let state = summary_state_on(tx, meeting_id)?;
    check_revision(&state.revision, &input.expected_summary_revision)?;
    let transcript = transcript_state_on(tx, meeting_id)?;
    check_revision(&transcript.revision, &input.expected_transcript_revision)?;
    if transcript.document_state != DocumentState::Present {
        return Err(DataError::Invalid("generation requires transcript"));
    }
    if expected_source_id.is_some_and(|id| transcript.current_source_id.as_deref() != Some(id)) {
        return Err(DataError::Forbidden);
    }
    if state.automatic_generation_suppressed
        || (state.document_state == DocumentState::Present && !state.auto_replace)
    {
        return Err(DataError::Forbidden);
    }
    if state.document_state == DocumentState::Present {
        archive_summary_on(tx, meeting_id, &state)?;
    }
    write_summary_on(
        tx,
        meeting_id,
        &input.markdown,
        &input.structured_json,
        &input.template_id,
        &input.model,
    )?;
    let rev = revision();
    tx.execute("UPDATE service_summary_state SET revision=?2,document_state='present',origin='configured_generation',auto_replace=1,source_label=NULL,source_revision=?3,source_missing=0 WHERE meeting_id=?1",params![meeting_id,rev,transcript.revision])?;
    super::search::rebuild_meeting_on(tx, meeting_id)?;
    Ok(rev)
}

impl Store {
    pub fn service_transcript_state(&self, meeting_id: &str) -> DataResult<TranscriptState> {
        safe_id(meeting_id)?;
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        transcript_state_on(&conn, meeting_id)
    }

    /// Reads a current or archived source with its exact overlay snapshot. Pages
    /// pin the transcript state revision; public adapter continuation is separate.
    pub fn service_read_transcript(
        &self,
        meeting_id: &str,
        request: &TranscriptRead,
    ) -> DataResult<TranscriptSourcePage> {
        safe_id(meeting_id)?;
        if !(1..=100).contains(&request.limit)
            || !(1..=FRAGMENT_BYTES).contains(&request.max_bytes)
            || request.offset > i64::MAX as usize
            || (request.offset > 0 && request.expected_revision.is_none())
        {
            return Err(DataError::Invalid("transcript page"));
        }
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        let state = transcript_state_on(&conn, meeting_id)?;
        if let Some(expected) = &request.expected_revision {
            check_revision(&state.revision, expected)?;
        }
        let source_id = request
            .source_id
            .as_ref()
            .or(state.current_source_id.as_ref());
        let mut page = TranscriptSourcePage {
            state: state.clone(),
            source_id: source_id.cloned(),
            origin: None,
            source_label: None,
            language: None,
            created_at: None,
            segments: Vec::new(),
            next_offset: None,
        };
        let Some(source_id) = source_id else {
            return Ok(page);
        };
        safe_id(source_id)?;
        let metadata: Option<TranscriptSourceMetadata> = conn.query_row(
            "SELECT origin,source_label,language,created_at FROM service_transcript_sources WHERE id=?1 AND meeting_id=?2",
            params![source_id,meeting_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let (origin, source_label, language, created_at) = metadata.ok_or(DataError::NotFound)?;
        page.origin = Some(origin);
        page.source_label = source_label;
        page.language = language;
        page.created_at = Some(created_at);
        let mut statement=conn.prepare("SELECT j.value,o.clean_text,COALESCE(o.manual,0),p.segment_key IS NOT NULL,p.label FROM service_transcript_sources s,json_each(s.raw_json) j LEFT JOIN service_transcript_overlays o ON o.source_id=s.id AND o.segment_key=CAST(json_extract(j.value,'$.id') AS TEXT) LEFT JOIN service_transcript_speaker_overlays p ON p.source_id=s.id AND p.segment_key=CAST(json_extract(j.value,'$.id') AS TEXT) WHERE s.id=?1 AND s.meeting_id=?2 AND j.key>=?3 ORDER BY j.key LIMIT ?4")?;
        let rows = statement.query_map(
            params![
                source_id,
                meeting_id,
                request.offset as i64,
                request.limit as i64 + 1
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )?;
        let mut bytes = 0;
        for row in rows {
            let (raw, clean_text, manual, has_speaker_overlay, speaker_overlay) = row?;
            if page.segments.len() == request.limit {
                page.next_offset = Some(request.offset + page.segments.len());
                break;
            }
            #[derive(Deserialize)]
            struct Raw {
                id: i64,
                start_ms: i64,
                end_ms: i64,
                speaker: Option<String>,
                text: String,
            }
            let raw: Raw = serde_json::from_str(&raw)?;
            let size = raw.text.len() + clean_text.as_ref().map_or(0, String::len);
            if bytes + size > request.max_bytes {
                if page.segments.is_empty() {
                    return Err(DataError::Invalid("segment exceeds page budget"));
                }
                page.next_offset = Some(request.offset + page.segments.len());
                break;
            }
            bytes += size;
            page.segments.push(TranscriptSourceSegment {
                id: raw.id,
                start_ms: raw.start_ms,
                end_ms: raw.end_ms,
                speaker: if has_speaker_overlay {
                    speaker_overlay
                } else {
                    raw.speaker
                },
                text: raw.text,
                clean_text,
                clean_text_manual: manual,
            });
        }
        Ok(page)
    }

    pub fn service_import_transcript(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        input: &TranscriptWrite,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        validate_segments(input)?;
        super::consistency::mutate(self, ctx, "import_transcript", &(meeting_id, input), |tx| {
            live_meeting(tx, meeting_id)?;
            let state = transcript_state_on(tx, meeting_id)?;
            check_revision(&state.revision, &input.expected_revision)?;
            if state.document_state == DocumentState::Present {
                return Err(DataError::Conflict {
                    current_revision: state.revision,
                });
            }
            let rev = replace_transcript_on(tx, meeting_id, &state, input)?;
            super::consistency::outcome(tx, "transcript", meeting_id, &rev, ctx)
        })
    }

    pub fn service_replace_transcript(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        input: &TranscriptWrite,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        validate_segments(input)?;
        super::consistency::mutate(
            self,
            ctx,
            "replace_transcript",
            &(meeting_id, input),
            |tx| {
                live_meeting(tx, meeting_id)?;
                let state = transcript_state_on(tx, meeting_id)?;
                check_revision(&state.revision, &input.expected_revision)?;
                if state.document_state != DocumentState::Present {
                    return Err(DataError::Conflict {
                        current_revision: state.revision,
                    });
                }
                let rev = replace_transcript_on(tx, meeting_id, &state, input)?;
                super::consistency::outcome(tx, "transcript", meeting_id, &rev, ctx)
            },
        )
    }

    pub fn service_set_transcript_corrections(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        input: &TranscriptCorrections,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        if input.corrections.is_empty() || input.corrections.len() > 100 {
            return Err(DataError::Invalid("correction count"));
        }
        let mut ids = HashSet::new();
        for correction in &input.corrections {
            if correction.segment_id <= 0 || !ids.insert(correction.segment_id) {
                return Err(DataError::Invalid("duplicate or invalid segment"));
            }
            if let Some(text) = &correction.text {
                bounded_text(text, 0, FRAGMENT_BYTES)?;
            }
        }
        super::consistency::mutate(
            self,
            ctx,
            "set_transcript_corrections",
            &(meeting_id, input),
            |tx| {
                live_meeting(tx, meeting_id)?;
                let state = transcript_state_on(tx, meeting_id)?;
                check_revision(&state.revision, &input.expected_revision)?;
                let source_id = state.current_source_id.ok_or(DataError::NotFound)?;
                // Resolve the entire batch before writing any overlay.
                for correction in &input.corrections {
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM segments WHERE id=?1 AND meeting_id=?2)",
                        params![correction.segment_id, meeting_id],
                        |r| r.get(0),
                    )?;
                    if !exists {
                        return Err(DataError::NotFound);
                    }
                }
                for correction in &input.corrections {
                    tx.execute("UPDATE segments SET clean_text=?3,clean_text_manual=?4 WHERE id=?1 AND meeting_id=?2",params![correction.segment_id,meeting_id,correction.text,correction.text.is_some()])?;
                    if let Some(text) = &correction.text {
                        tx.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,?3,1) ON CONFLICT(source_id,segment_key) DO UPDATE SET clean_text=excluded.clean_text,manual=1",params![source_id,correction.segment_id.to_string(),text])?;
                    } else {
                        tx.execute("DELETE FROM service_transcript_overlays WHERE source_id=?1 AND segment_key=?2",params![source_id,correction.segment_id.to_string()])?;
                    }
                }
                let rev = revision();
                tx.execute(
                    "UPDATE service_transcript_state SET revision=?2 WHERE meeting_id=?1",
                    params![meeting_id, rev],
                )?;
                crate::ai::rebuild_chunks_on(tx, meeting_id)?;
                super::search::rebuild_meeting_on(tx, meeting_id)?;
                super::consistency::outcome(tx, "transcript", meeting_id, &rev, ctx)
            },
        )
    }

    pub fn service_delete_transcript(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        expected_revision: &str,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        super::consistency::mutate(
            self,
            ctx,
            "delete_transcript",
            &(meeting_id, expected_revision),
            |tx| {
                live_meeting(tx, meeting_id)?;
                let state = transcript_state_on(tx, meeting_id)?;
                check_revision(&state.revision, expected_revision)?;
                super::postprocess_checkpoint::invalidate_on(tx, meeting_id)?;
                tx.execute("DELETE FROM segments WHERE meeting_id=?1", [meeting_id])?;
                tx.execute("DELETE FROM chunks WHERE meeting_id=?1", [meeting_id])?;
                tx.execute(
                    "DELETE FROM meeting_speakers WHERE meeting_id=?1",
                    [meeting_id],
                )?;
                tx.execute(
                    "DELETE FROM service_transcript_sources WHERE meeting_id=?1",
                    [meeting_id],
                )?;
                tx.execute(
                    "DELETE FROM settings WHERE key IN (?1,?2)",
                    params![
                        format!("stt:{meeting_id}"),
                        format!("stt_previous:{meeting_id}")
                    ],
                )?;
                crate::settings::clear_meeting_voice_queries_on(tx, meeting_id)?;
                crate::transcript_versions::delete_versions_on(tx, meeting_id)?;
                super::chats::erase_meeting_references_on(tx, meeting_id)?;
                let rev = revision();
                tx.execute("UPDATE service_transcript_state SET revision=?2,current_source_id=NULL,document_state='deleted',deletion_epoch=deletion_epoch+1 WHERE meeting_id=?1",params![meeting_id,rev])?;
                tx.execute("UPDATE meetings SET duration_ms=0 WHERE id=?1 AND EXISTS(SELECT 1 FROM service_meeting_state s WHERE s.meeting_id=?1 AND s.content_kind='text')",[meeting_id])?;
                let summary = summary_state_on(tx, meeting_id)?;
                if summary.document_state == DocumentState::Present
                    && summary.source_revision.is_some()
                    && !summary.source_missing
                {
                    let summary_rev = revision();
                    tx.execute("UPDATE service_summary_state SET revision=?2,source_missing=1 WHERE meeting_id=?1",params![meeting_id,summary_rev])?;
                    super::consistency::change(
                        tx,
                        "summary",
                        meeting_id,
                        &summary_rev,
                        &ctx.principal,
                    )?;
                }
                super::execution::invalidate_publications_on(tx, "transcript", meeting_id)?;
                super::search::rebuild_meeting_on(tx, meeting_id)?;
                let mut result =
                    super::consistency::outcome(tx, "transcript", meeting_id, &rev, ctx)?;
                result.job_id = Some(super::execution::queue_deletion_on(
                    tx,
                    meeting_id,
                    "transcript",
                    &rev,
                    &ctx.principal,
                )?);
                Ok(result)
            },
        )
    }

    pub fn service_summary_state(&self, meeting_id: &str) -> DataResult<SummaryState> {
        safe_id(meeting_id)?;
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        summary_state_on(&conn, meeting_id)
    }

    pub fn service_read_summary(&self, meeting_id: &str) -> DataResult<SummarySnapshot> {
        safe_id(meeting_id)?;
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        let state = summary_state_on(&conn, meeting_id)?;
        let current=conn.query_row("SELECT meeting_id,template_id,markdown,structured_json,model,created_at FROM summaries WHERE meeting_id=?1",[meeting_id],|r| {
            let structured:String=r.get(3)?;
            Ok(Summary {meeting_id:r.get(0)?,template_id:r.get(1)?,markdown:r.get(2)?,structured:serde_json::from_str(&structured).unwrap_or(serde_json::Value::Null),model:r.get(4)?,created_at:r.get(5)?})
        }).optional()?;
        Ok(SummarySnapshot { state, current })
    }

    pub fn service_summary_history(
        &self,
        meeting_id: &str,
        limit: usize,
    ) -> DataResult<Vec<SummaryVersion>> {
        safe_id(meeting_id)?;
        if !(1..=100).contains(&limit) {
            return Err(DataError::Invalid("history limit"));
        }
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        let mut st=conn.prepare("SELECT id,revision,template_id,markdown,structured_json,model,created_at,origin,auto_replace,source_label,source_revision FROM service_summary_history WHERE meeting_id=?1 ORDER BY rowid DESC LIMIT ?2")?;
        let rows = st
            .query_map(params![meeting_id, limit as i64], |r| {
                let structured: String = r.get(4)?;
                Ok(SummaryVersion {
                    id: r.get(0)?,
                    revision: r.get(1)?,
                    summary: Summary {
                        meeting_id: meeting_id.into(),
                        template_id: r.get(2)?,
                        markdown: r.get(3)?,
                        structured: serde_json::from_str(&structured)
                            .unwrap_or(serde_json::Value::Null),
                        model: r.get(5)?,
                        created_at: r.get(6)?,
                    },
                    origin: r.get(7)?,
                    auto_replace: r.get(8)?,
                    source_label: r.get(9)?,
                    source_revision: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn service_write_summary(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        input: &SummaryWrite,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        validate_summary(&input.markdown, &input.structured_json)?;
        if let Some(label) = &input.source_label {
            bounded_text(label, 1, 256)?;
        }
        super::consistency::mutate(self, ctx, "write_summary", &(meeting_id, input), |tx| {
            live_meeting(tx, meeting_id)?;
            let state = summary_state_on(tx, meeting_id)?;
            check_revision(&state.revision, &input.expected_revision)?;
            let present = state.document_state == DocumentState::Present;
            if matches!(input.mode, SummaryWriteMode::Create) == present {
                return Err(DataError::Conflict {
                    current_revision: state.revision,
                });
            }
            if present {
                archive_summary_on(tx, meeting_id, &state)?;
            }
            let transcript = transcript_state_on(tx, meeting_id)?;
            let source_revision = (transcript.document_state == DocumentState::Present)
                .then_some(transcript.revision);
            write_summary_on(
                tx,
                meeting_id,
                &input.markdown,
                &input.structured_json,
                "external",
                input.author.as_str(),
            )?;
            let rev = revision();
            tx.execute("UPDATE service_summary_state SET revision=?2,document_state='present',automatic_generation_suppressed=0,origin=?3,auto_replace=0,source_label=?4,source_revision=?5,source_missing=0 WHERE meeting_id=?1",params![meeting_id,rev,input.author.as_str(),input.source_label,source_revision])?;
            super::search::rebuild_meeting_on(tx, meeting_id)?;
            super::consistency::outcome(tx, "summary", meeting_id, &rev, ctx)
        })
    }

    /// Publish an explicitly selected desktop agent result after inference.
    /// Input and output revisions are compared in the same transaction as publication.
    pub fn service_commit_agent_summary(
        &self,
        ctx: &MutationContext,
        result: &AgentSummaryCommit,
    ) -> DataResult<MutationOutcome> {
        let id = result.meeting_id.as_str();
        safe_id(id)?;
        safe_id(&result.expected_source_id)?;
        safe_id(&result.template_id)?;
        bounded_text(&result.model, 1, 256)?;
        if result.markdown.trim().is_empty() {
            return Err(DataError::Invalid("empty agent summary"));
        }
        let mut structured = result.structured_json.clone();
        let fields = structured
            .as_object_mut()
            .ok_or(DataError::Invalid("agent summary sections"))?;
        fields.insert("generation".into(), serde_json::json!({
            "agent": result.provider,
            "model": result.model,
            "template_id": result.template_id,
        }));
        validate_summary(&result.markdown, &structured)?;

        super::consistency::mutate(self, ctx, "commit_agent_summary", result, |tx| {
            live_meeting(tx, id)?;
            let lifecycle: String = tx.query_row(
                "SELECT lifecycle_revision FROM service_meeting_state WHERE meeting_id=?1",
                [id],
                |row| row.get(0),
            )?;
            check_revision(&lifecycle, &result.expected_lifecycle_revision)?;
            let transcript = transcript_state_on(tx, id)?;
            check_revision(&transcript.revision, &result.expected_transcript_revision)?;
            if transcript.document_state != DocumentState::Present
                || transcript.current_source_id.as_deref() != Some(result.expected_source_id.as_str())
            {
                return Err(DataError::Conflict { current_revision: transcript.revision });
            }
            let summary = summary_state_on(tx, id)?;
            check_revision(&summary.revision, &result.expected_summary_revision)?;
            if summary.document_state == DocumentState::Present {
                archive_summary_on(tx, id, &summary)?;
            }
            write_summary_on(tx, id, &result.markdown, &structured, &result.template_id, &result.model)?;
            let rev = revision();
            tx.execute(
                "UPDATE service_summary_state SET revision=?2,document_state='present',
                 automatic_generation_suppressed=0,origin='external_agent',auto_replace=0,
                 source_label=?3,source_revision=?4,source_missing=0 WHERE meeting_id=?1",
                params![id, rev, result.provider.label(), transcript.revision],
            )?;
            super::search::rebuild_meeting_on(tx, id)?;
            super::consistency::outcome(tx, "summary", id, &rev, ctx)
        })
    }

    /// Called only by the configured native generation worker. Both input and
    /// target generations are checked at commit, after expensive inference.
    pub fn service_commit_generated_summary(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        input: &GeneratedSummary,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        validate_summary(&input.markdown, &input.structured_json)?;
        bounded_text(&input.template_id, 1, 256)?;
        bounded_text(&input.model, 1, 256)?;
        super::consistency::mutate(
            self,
            ctx,
            "commit_generated_summary",
            &(meeting_id, input),
            |tx| {
                let rev = install_generated_summary_on(tx, meeting_id, input, None)?;
                super::consistency::outcome(tx, "summary", meeting_id, &rev, ctx)
            },
        )
    }

    pub fn service_delete_summary(
        &self,
        ctx: &MutationContext,
        meeting_id: &str,
        expected_revision: &str,
    ) -> DataResult<MutationOutcome> {
        safe_id(meeting_id)?;
        super::consistency::mutate(
            self,
            ctx,
            "delete_summary",
            &(meeting_id, expected_revision),
            |tx| {
                live_meeting(tx, meeting_id)?;
                let state = summary_state_on(tx, meeting_id)?;
                check_revision(&state.revision, expected_revision)?;
                tx.execute("DELETE FROM summaries WHERE meeting_id=?1", [meeting_id])?;
                tx.execute(
                    "DELETE FROM service_summary_history WHERE meeting_id=?1",
                    [meeting_id],
                )?;
                let rev = revision();
                tx.execute("UPDATE service_summary_state SET revision=?2,document_state='deleted',deletion_epoch=deletion_epoch+1,automatic_generation_suppressed=1,auto_replace=0,source_label=NULL,source_revision=NULL,source_missing=0 WHERE meeting_id=?1",params![meeting_id,rev])?;
                super::execution::invalidate_publications_on(tx, "summary", meeting_id)?;
                super::search::rebuild_meeting_on(tx, meeting_id)?;
                let mut result = super::consistency::outcome(tx, "summary", meeting_id, &rev, ctx)?;
                result.job_id = Some(super::execution::queue_deletion_on(
                    tx,
                    meeting_id,
                    "summary",
                    &rev,
                    &ctx.principal,
                )?);
                Ok(result)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(key: &str) -> MutationContext {
        MutationContext {
            principal: "test-agent".into(),
            idempotency_key: key.into(),
        }
    }

    fn fixture(kind: &str) -> (tempfile::TempDir, Store, String) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&crate::database_path(temp.path())).unwrap();
        let mut meeting = crate::Meeting::new_recording("Document fixture", None);
        meeting.status = crate::MeetingStatus::Ready;
        meeting.duration_ms = 50_000;
        store.create_meeting(&meeting).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO service_meeting_state(meeting_id,content_kind,metadata_revision,lifecycle_revision) VALUES(?1,?2,?3,?4)",params![meeting.id,kind,revision(),revision()]).unwrap();
            conn.execute("INSERT INTO service_transcript_state(meeting_id,revision,document_state) VALUES(?1,?2,'absent')",params![meeting.id,revision()]).unwrap();
            conn.execute("INSERT INTO service_summary_state(meeting_id,revision,document_state,origin,auto_replace) VALUES(?1,?2,'absent','legacy',0)",params![meeting.id,revision()]).unwrap();
        }
        (temp, store, meeting.id)
    }

    fn transcript(store: &Store, id: &str, text: &str) -> TranscriptWrite {
        TranscriptWrite {
            expected_revision: store.service_transcript_state(id).unwrap().revision,
            segments: vec![
                NewSegment {
                    start_ms: 0,
                    end_ms: 1000,
                    speaker: Some("Анна".into()),
                    text: text.into(),
                },
                NewSegment {
                    start_ms: 1000,
                    end_ms: 2000,
                    speaker: None,
                    text: "Second untouched sentence".into(),
                },
            ],
            source_label: Some("Synthetic external source".into()),
            language: Some("ru-RU".into()),
            origin: TranscriptImportOrigin::ExternalAgent,
        }
    }

    fn write_summary(store: &Store, id: &str, mode: SummaryWriteMode, text: &str) -> SummaryWrite {
        SummaryWrite {
            expected_revision: store.service_summary_state(id).unwrap().revision,
            mode,
            markdown: text.into(),
            structured_json: serde_json::json!({"summary":text}),
            source_label: Some("Agent draft".into()),
            author: SummaryAuthor::ExternalAgent,
        }
    }

    fn voice_query_keys(id: &str) -> [String; 3] {
        [
            format!("speaker_embeddings:{id}"),
            format!("speaker_embeddings:old-policy:{id}"),
            format!("speaker_embeddings:future-policy:{id}"),
        ]
    }

    #[test]
    fn sources_are_immutable_and_replace_archives_exact_raw_and_overlays() {
        let (_temp, store, id) = fixture("text");
        let input = transcript(&store, &id, "Original raw бюджет");
        let result = store
            .service_import_transcript(&context("import"), &id, &input)
            .unwrap();
        let initial = store
            .service_read_transcript(&id, &TranscriptRead::default())
            .unwrap();
        let old_source = initial.source_id.clone().unwrap();
        let old_id = initial.segments[0].id;
        let cache_keys = voice_query_keys(&id);
        let retained_keys = [
            format!("highlights:{id}"),
            format!("speaker_embeddings:old-policy:other-{id}"),
        ];
        for key in cache_keys.iter().chain(&retained_keys) {
            store.set_setting(key, &"synthetic observation").unwrap();
        }
        store
            .service_set_transcript_corrections(
                &context("clean"),
                &id,
                &TranscriptCorrections {
                    expected_revision: result.revision,
                    corrections: vec![TranscriptCorrection {
                        segment_id: old_id,
                        text: Some("".into()),
                    }],
                },
            )
            .unwrap();
        for key in &cache_keys {
            assert!(store.get_setting::<String>(key).unwrap().is_some());
        }
        assert!(store
            .conn
            .lock()
            .execute(
                "UPDATE service_transcript_sources SET raw_json='[]' WHERE id=?1",
                [&old_source]
            )
            .is_err());
        let replace = transcript(&store, &id, "Replacement raw");
        store
            .service_replace_transcript(&context("replace"), &id, &replace)
            .unwrap();
        let archived = store
            .service_read_transcript(
                &id,
                &TranscriptRead {
                    source_id: Some(old_source),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(archived.segments[0].text, "Original raw бюджет");
        assert_eq!(archived.segments[0].clean_text.as_deref(), Some(""));
        assert!(archived.segments[0].clean_text_manual);
        let current = store
            .service_read_transcript(&id, &TranscriptRead::default())
            .unwrap();
        assert_eq!(current.segments[0].text, "Replacement raw");
        for key in &cache_keys {
            assert!(store.get_setting::<String>(key).unwrap().is_none());
        }
        for key in &retained_keys {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("synthetic observation")
            );
        }
        assert_eq!(store.get_meeting(&id).unwrap().unwrap().duration_ms, 2000);
        assert!(!store.root().join("recordings").exists());
    }

    #[test]
    fn correction_batch_validates_every_segment_and_preserves_empty_reset_raw() {
        let (_temp, store, id) = fixture("recorded");
        store
            .service_import_transcript(
                &context("import"),
                &id,
                &transcript(&store, &id, "Raw telescope"),
            )
            .unwrap();
        let before = store.segments(&id).unwrap();
        let revision = store.service_transcript_state(&id).unwrap().revision;
        let invalid = TranscriptCorrections {
            expected_revision: revision.clone(),
            corrections: vec![
                TranscriptCorrection {
                    segment_id: before[0].id,
                    text: Some("partial write must rollback".into()),
                },
                TranscriptCorrection {
                    segment_id: i64::MAX,
                    text: Some("bad".into()),
                },
            ],
        };
        assert!(matches!(
            store.service_set_transcript_corrections(&context("bad-batch"), &id, &invalid),
            Err(DataError::NotFound)
        ));
        assert_eq!(store.segments(&id).unwrap(), before);
        assert_eq!(
            store.service_transcript_state(&id).unwrap().revision,
            revision
        );
        let corrected = store
            .service_set_transcript_corrections(
                &context("empty"),
                &id,
                &TranscriptCorrections {
                    expected_revision: revision,
                    corrections: vec![TranscriptCorrection {
                        segment_id: before[0].id,
                        text: Some("".into()),
                    }],
                },
            )
            .unwrap();
        let rows = store.segments(&id).unwrap();
        assert_eq!(rows[0].text, "Raw telescope");
        assert_eq!(rows[0].clean_text.as_deref(), Some(""));
        assert!(rows[0].clean_text_manual);
        store
            .service_set_transcript_corrections(
                &context("reset"),
                &id,
                &TranscriptCorrections {
                    expected_revision: corrected.revision,
                    corrections: vec![TranscriptCorrection {
                        segment_id: before[0].id,
                        text: None,
                    }],
                },
            )
            .unwrap();
        assert_eq!(store.segments(&id).unwrap(), before);
        assert_eq!(store.get_meeting(&id).unwrap().unwrap().duration_ms, 50_000);
    }

    #[test]
    fn transcript_delete_erases_archives_provider_copies_vectors_and_citations() {
        let (_temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("import"),
                &id,
                &transcript(&store, &id, "Raw source only"),
            )
            .unwrap();
        store
            .service_replace_transcript(
                &context("replace"),
                &id,
                &transcript(&store, &id, "Next source"),
            )
            .unwrap();
        store
            .service_write_summary(
                &context("summary"),
                &id,
                &write_summary(&store, &id, SummaryWriteMode::Create, "Retained summary"),
            )
            .unwrap();
        store
            .set_setting(
                &format!("stt:{id}"),
                &serde_json::json!({"captions":["provider copy"]}),
            )
            .unwrap();
        store
            .set_setting(
                &format!("stt_previous:{id}"),
                &serde_json::json!({"captions":["previous provider"]}),
            )
            .unwrap();
        store.set_setting("unrelated", &"keep").unwrap();
        let cache_keys = voice_query_keys(&id);
        for key in &cache_keys {
            store.set_setting(key, &vec![0.25_f32, 0.75]).unwrap();
        }
        let retained_keys = [
            format!("highlights:{id}"),
            format!("speaker_embeddings:future-policy:other-{id}"),
        ];
        for key in &retained_keys {
            store.set_setting(key, &"keep").unwrap();
        }
        let chat = store.create_chat("all", None, "Mixed citations").unwrap();
        let message = store
            .add_message(
                &chat.id,
                "assistant",
                "Retained chat text",
                &serde_json::json!([{"meeting_id":id},"plain string"]),
            )
            .unwrap();
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO chunk_vectors(chunk_id,dim,vec) SELECT id,1,X'0000803f' FROM chunks WHERE meeting_id=?1",[&id]).unwrap();
        }
        let state = store.service_transcript_state(&id).unwrap();
        let summary_before = store.service_summary_state(&id).unwrap();
        let outcome = store
            .service_delete_transcript(&context("delete"), &id, &state.revision)
            .unwrap();
        assert!(outcome.job_id.is_some());
        assert!(store.segments(&id).unwrap().is_empty());
        assert!(store.chunks(&id).unwrap().is_empty());
        let conn = store.conn.lock();
        for table in [
            "service_transcript_sources",
            "service_transcript_overlays",
            "chunk_vectors",
        ] {
            assert_eq!(
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            conn.query_row(
                "SELECT citations_json FROM chat_messages WHERE id=?1",
                [message.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "[]"
        );
        drop(conn);
        assert!(store
            .get_setting::<serde_json::Value>(&format!("stt:{id}"))
            .unwrap()
            .is_none());
        assert!(store
            .get_setting::<serde_json::Value>(&format!("stt_previous:{id}"))
            .unwrap()
            .is_none());
        for key in &cache_keys {
            assert!(store.get_setting::<Vec<f32>>(key).unwrap().is_none());
        }
        for key in &retained_keys {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("keep")
            );
        }
        assert_eq!(
            store.get_setting::<String>("unrelated").unwrap().as_deref(),
            Some("keep")
        );
        let summary = store.service_read_summary(&id).unwrap();
        assert_eq!(summary.current.unwrap().markdown, "Retained summary");
        assert!(summary.state.source_missing);
        assert_ne!(summary.state.revision, summary_before.revision);
        let deleted = store.service_transcript_state(&id).unwrap();
        assert_eq!(deleted.document_state, DocumentState::Deleted);
        assert_eq!(deleted.deletion_epoch, 1);
        assert_ne!(deleted.revision, state.revision);
        assert_eq!(store.get_meeting(&id).unwrap().unwrap().duration_ms, 0);
    }

    #[test]
    fn desktop_agent_summary_commits_only_the_observed_meeting_and_input() {
        use bazza_domain::agent_summary::SummaryProvider;

        let (_temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("agent-input"),
                &id,
                &transcript(&store, &id, "Explicitly shared transcript"),
            )
            .unwrap();
        let input = store.service_transcript_state(&id).unwrap();
        let original = store
            .service_write_summary(
                &context("agent-prior"),
                &id,
                &write_summary(&store, &id, SummaryWriteMode::Create, "Prior summary"),
            )
            .unwrap();
        let mut result = AgentSummaryCommit {
            meeting_id: id.clone(),
            expected_lifecycle_revision: store.service_meeting(&id).unwrap().unwrap().lifecycle_revision,
            expected_transcript_revision: input.revision,
            expected_source_id: input.current_source_id.unwrap(),
            expected_summary_revision: original.revision.clone(),
            markdown: "Generated synthetic summary".into(),
            structured_json: serde_json::json!({"summary": "Synthetic", "generation": "untrusted"}),
            template_id: "general".into(),
            model: "test-model".into(),
            provider: SummaryProvider::Codex,
        };
        let ctx = context("agent-commit");
        for stale in [
            AgentSummaryCommit {
                expected_lifecycle_revision: "older-lifecycle".into(),
                ..result.clone()
            },
            AgentSummaryCommit {
                expected_source_id: "older-source".into(),
                ..result.clone()
            },
            AgentSummaryCommit {
                expected_summary_revision: "older-summary".into(),
                ..result.clone()
            },
        ] {
            assert!(matches!(
                store.service_commit_agent_summary(&ctx, &stale),
                Err(DataError::Conflict { .. })
            ));
            assert_eq!(store.service_summary_state(&id).unwrap().revision, original.revision);
        }
        store
            .service_replace_transcript(
                &context("agent-replacement"),
                &id,
                &transcript(&store, &id, "Replacement after the provider request"),
            )
            .unwrap();
        assert!(matches!(
            store.service_commit_agent_summary(&ctx, &result),
            Err(DataError::Conflict { .. })
        ));
        assert_eq!(
            store.service_read_summary(&id).unwrap().current.unwrap().markdown,
            "Prior summary",
        );

        // A newly generated response can use the new observed snapshot.
        let input = store.service_transcript_state(&id).unwrap();
        result.expected_transcript_revision = input.revision.clone();
        result.expected_source_id = input.current_source_id.unwrap();
        result.expected_summary_revision = store.service_summary_state(&id).unwrap().revision;
        let saved = store.service_commit_agent_summary(&ctx, &result).unwrap();
        assert_eq!(store.service_commit_agent_summary(&ctx, &result).unwrap(), saved);
        let snapshot = store.service_read_summary(&id).unwrap();
        assert_eq!(snapshot.state.revision, saved.revision);
        assert_eq!(snapshot.state.origin, "external_agent");
        assert!(!snapshot.state.auto_replace);
        assert_eq!(snapshot.state.source_label.as_deref(), Some("Codex"));
        assert_eq!(snapshot.state.source_revision.as_deref(), Some(input.revision.as_str()));
        let current = snapshot.current.unwrap();
        assert_eq!(current.markdown, result.markdown);
        assert_eq!(current.model, "test-model");
        assert_eq!(current.template_id, "general");
        assert_eq!(current.structured["generation"]["agent"], "codex");
        assert_eq!(store.service_summary_history(&id, 100).unwrap().len(), 1);
        result.markdown.push_str(" with edited content");
        assert!(matches!(
            store.service_commit_agent_summary(&ctx, &result),
            Err(DataError::IdempotencyConflict)
        ));
    }

    #[test]
    fn summary_history_protection_delete_suppression_and_aba_survive_reopen() {
        let (temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("import"),
                &id,
                &transcript(&store, &id, "Transcript"),
            )
            .unwrap();
        let absent = store.service_summary_state(&id).unwrap();
        let first = write_summary(&store, &id, SummaryWriteMode::Create, "First summary");
        store
            .service_write_summary(&context("first"), &id, &first)
            .unwrap();
        store
            .service_write_summary(
                &context("second"),
                &id,
                &write_summary(&store, &id, SummaryWriteMode::Replace, "Second summary"),
            )
            .unwrap();
        let history = store.service_summary_history(&id, 100).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].summary.markdown, "First summary");
        assert!(history[0].source_revision.is_some());
        assert_eq!(history[0].origin, "external_agent");
        assert!(store
            .conn
            .lock()
            .execute("UPDATE service_summary_history SET markdown='tamper'", [])
            .is_err());
        let current = store.service_summary_state(&id).unwrap();
        store
            .service_delete_summary(&context("delete"), &id, &current.revision)
            .unwrap();
        drop(store);
        let reopened = Store::open(&crate::database_path(temp.path())).unwrap();
        let state = reopened.service_summary_state(&id).unwrap();
        assert_eq!(state.document_state, DocumentState::Deleted);
        assert!(state.automatic_generation_suppressed);
        assert_eq!(state.deletion_epoch, 1);
        assert_ne!(state.revision, absent.revision);
        assert!(reopened
            .service_summary_history(&id, 100)
            .unwrap()
            .is_empty());
        assert!(reopened
            .service_read_summary(&id)
            .unwrap()
            .current
            .is_none());
        assert!(matches!(
            reopened.service_write_summary(&context("stale-create"), &id, &first),
            Err(DataError::Conflict { .. })
        ));
        let generated = GeneratedSummary {
            expected_summary_revision: state.revision.clone(),
            expected_transcript_revision: reopened.service_transcript_state(&id).unwrap().revision,
            markdown: "Late auto result".into(),
            structured_json: serde_json::Value::Null,
            template_id: "general".into(),
            model: "test-model".into(),
        };
        assert!(matches!(
            reopened.service_commit_generated_summary(&context("suppressed-auto"), &id, &generated),
            Err(DataError::Forbidden)
        ));
        reopened
            .service_write_summary(
                &context("explicit-restore"),
                &id,
                &write_summary(
                    &reopened,
                    &id,
                    SummaryWriteMode::Create,
                    "Restored explicitly",
                ),
            )
            .unwrap();
        assert!(
            !reopened
                .service_summary_state(&id)
                .unwrap()
                .automatic_generation_suppressed
        );
    }

    #[test]
    fn generated_summary_checks_input_target_and_manual_policy() {
        let (_temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("import"),
                &id,
                &transcript(&store, &id, "Initial transcript"),
            )
            .unwrap();
        let generated = GeneratedSummary {
            expected_summary_revision: store.service_summary_state(&id).unwrap().revision,
            expected_transcript_revision: store.service_transcript_state(&id).unwrap().revision,
            markdown: "Configured result".into(),
            structured_json: serde_json::json!({"summary":"Configured result"}),
            template_id: "general".into(),
            model: "test-model".into(),
        };
        store
            .service_replace_transcript(
                &context("replace"),
                &id,
                &transcript(&store, &id, "Changed transcript"),
            )
            .unwrap();
        assert!(matches!(
            store.service_commit_generated_summary(&context("stale-input"), &id, &generated),
            Err(DataError::Conflict { .. })
        ));
        let mut fresh = generated.clone();
        fresh.expected_transcript_revision = store.service_transcript_state(&id).unwrap().revision;
        store
            .service_commit_generated_summary(&context("fresh-auto"), &id, &fresh)
            .unwrap();
        let automatic = store.service_summary_state(&id).unwrap();
        assert_eq!(automatic.origin, "configured_generation");
        assert!(automatic.auto_replace);
        store
            .service_write_summary(
                &context("manual"),
                &id,
                &write_summary(
                    &store,
                    &id,
                    SummaryWriteMode::Replace,
                    "Protected agent summary",
                ),
            )
            .unwrap();
        fresh.expected_summary_revision = store.service_summary_state(&id).unwrap().revision;
        assert!(matches!(
            store.service_commit_generated_summary(&context("manual-protected"), &id, &fresh),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_commit_generated_summary(&context("stale-target"), &id, &generated),
            Err(DataError::Conflict { .. })
        ));
        assert_eq!(
            store
                .service_read_summary(&id)
                .unwrap()
                .current
                .unwrap()
                .markdown,
            "Protected agent summary"
        );
    }

    #[test]
    fn lost_response_retry_is_exact_and_reused_key_conflicts() {
        let (_temp, store, id) = fixture("text");
        let input = transcript(&store, &id, "One source");
        let first = store
            .service_import_transcript(&context("lost-response"), &id, &input)
            .unwrap();
        let retry = store
            .service_import_transcript(&context("lost-response"), &id, &input)
            .unwrap();
        assert_eq!(first, retry);
        let mut changed = input.clone();
        changed.segments[0].text = "Different source".into();
        assert!(matches!(
            store.service_import_transcript(&context("lost-response"), &id, &changed),
            Err(DataError::IdempotencyConflict)
        ));
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_transcript_sources", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(store.service_changes(0, 100).unwrap().len(), 1);
    }

    #[test]
    fn late_failure_rolls_back_source_projection_indexes_and_receipt() {
        let (_temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("initial"),
                &id,
                &transcript(&store, &id, "Original"),
            )
            .unwrap();
        let before = store.segments(&id).unwrap();
        let state = store.service_transcript_state(&id).unwrap();
        let cache_keys = voice_query_keys(&id);
        let retained_keys = [
            format!("stt:{id}"),
            format!("stt_previous:{id}"),
            format!("highlights:{id}"),
        ];
        for key in cache_keys.iter().chain(&retained_keys) {
            store.set_setting(key, &"rollback observation").unwrap();
        }
        store.conn.lock().execute_batch("CREATE TRIGGER fail_transcript_change BEFORE INSERT ON service_changes WHEN new.entity_kind='transcript' BEGIN SELECT RAISE(ABORT,'injected commit failure');END;").unwrap();
        assert!(matches!(
            store.service_replace_transcript(
                &context("failing"),
                &id,
                &transcript(&store, &id, "Must rollback")
            ),
            Err(DataError::Sqlite(_))
        ));
        assert_eq!(store.segments(&id).unwrap(), before);
        assert_eq!(store.service_transcript_state(&id).unwrap(), state);
        for key in cache_keys.iter().chain(&retained_keys) {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("rollback observation")
            );
        }
        assert!(matches!(
            store.service_delete_transcript(&context("failing-delete"), &id, &state.revision),
            Err(DataError::Sqlite(_))
        ));
        assert_eq!(store.segments(&id).unwrap(), before);
        assert_eq!(store.service_transcript_state(&id).unwrap(), state);
        for key in cache_keys.iter().chain(&retained_keys) {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("rollback observation")
            );
        }
        let conn = store.conn.lock();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM service_transcript_sources", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM service_idempotency WHERE key IN ('failing','failing-delete')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(conn);
        assert!(store
            .search_chunks("Original", Some(&id), 10)
            .unwrap()
            .iter()
            .any(|h| h.text.contains("Original")));
        assert!(store
            .search_chunks("rollback", Some(&id), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn transcript_pages_pin_revisions_and_recording_mutations_are_busy() {
        let (_temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("import"),
                &id,
                &transcript(&store, &id, "Page first"),
            )
            .unwrap();
        let first = store
            .service_read_transcript(
                &id,
                &TranscriptRead {
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first.next_offset, Some(1));
        store
            .service_set_transcript_corrections(
                &context("edit"),
                &id,
                &TranscriptCorrections {
                    expected_revision: first.state.revision.clone(),
                    corrections: vec![TranscriptCorrection {
                        segment_id: first.segments[0].id,
                        text: Some("Edited".into()),
                    }],
                },
            )
            .unwrap();
        assert!(matches!(
            store.service_read_transcript(
                &id,
                &TranscriptRead {
                    offset: 1,
                    expected_revision: Some(first.state.revision),
                    ..Default::default()
                }
            ),
            Err(DataError::Conflict { .. })
        ));
        let expected = store.service_transcript_state(&id).unwrap().revision;
        store
            .conn
            .lock()
            .execute("UPDATE meetings SET status='recording' WHERE id=?1", [&id])
            .unwrap();
        assert!(matches!(
            store.service_delete_transcript(&context("busy-delete"), &id, &expected),
            Err(DataError::Busy)
        ));
        assert_eq!(store.segments(&id).unwrap().len(), 2);
    }

    #[test]
    fn ordinary_edits_keep_shares_and_permanent_deletes_revoke_only_dependencies() {
        let (_temp, store, id) = fixture("text");
        store
            .service_import_transcript(
                &context("import"),
                &id,
                &transcript(&store, &id, "Original"),
            )
            .unwrap();
        let transcript_share = store.create_share(&id, "meeting", None, None).unwrap();
        let summary_share = store.create_share(&id, "meeting", None, None).unwrap();
        let legacy_live = store.create_share(&id, "meeting", None, None).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO service_publication_dependencies VALUES(?1,'transcript',?2,'snapshot-revision')",params![transcript_share.token,id]).unwrap();
            conn.execute("INSERT INTO service_publication_dependencies VALUES(?1,'summary',?2,'snapshot-revision')",params![summary_share.token,id]).unwrap();
        }
        let generated = GeneratedSummary {
            expected_summary_revision: store.service_summary_state(&id).unwrap().revision,
            expected_transcript_revision: store.service_transcript_state(&id).unwrap().revision,
            markdown: "Configured result".into(),
            structured_json: serde_json::Value::Null,
            template_id: "general".into(),
            model: "test-model".into(),
        };
        store
            .service_commit_generated_summary(&context("automatic"), &id, &generated)
            .unwrap();
        store
            .service_write_summary(
                &context("summary-edit"),
                &id,
                &write_summary(&store, &id, SummaryWriteMode::Replace, "Edited summary"),
            )
            .unwrap();
        store
            .service_replace_transcript(
                &context("replace"),
                &id,
                &transcript(&store, &id, "Replacement"),
            )
            .unwrap();
        let current = store
            .service_read_transcript(&id, &TranscriptRead::default())
            .unwrap();
        store
            .service_set_transcript_corrections(
                &context("correction"),
                &id,
                &TranscriptCorrections {
                    expected_revision: current.state.revision,
                    corrections: vec![TranscriptCorrection {
                        segment_id: current.segments[0].id,
                        text: Some("Edited overlay".into()),
                    }],
                },
            )
            .unwrap();
        assert!(store
            .list_shares()
            .unwrap()
            .iter()
            .all(|share| share.enabled));
        let summary_rev = store.service_summary_state(&id).unwrap().revision;
        store
            .service_delete_summary(&context("delete-summary"), &id, &summary_rev)
            .unwrap();
        let shares = store.list_shares().unwrap();
        assert!(
            !shares
                .iter()
                .find(|s| s.token == summary_share.token)
                .unwrap()
                .enabled
        );
        assert!(
            shares
                .iter()
                .find(|s| s.token == transcript_share.token)
                .unwrap()
                .enabled
        );
        assert!(
            shares
                .iter()
                .find(|s| s.token == legacy_live.token)
                .unwrap()
                .enabled
        );
        let transcript_rev = store.service_transcript_state(&id).unwrap().revision;
        store
            .service_delete_transcript(&context("delete-transcript"), &id, &transcript_rev)
            .unwrap();
        let shares = store.list_shares().unwrap();
        assert!(
            !shares
                .iter()
                .find(|s| s.token == transcript_share.token)
                .unwrap()
                .enabled
        );
        assert!(
            shares
                .iter()
                .find(|s| s.token == legacy_live.token)
                .unwrap()
                .enabled
        );
    }
}

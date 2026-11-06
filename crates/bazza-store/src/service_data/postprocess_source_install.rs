//! Install the worker's terminal Local ASR result within the caller's fenced
//! commit. Capture evidence and archived source bytes stay attached to the
//! original source. Only the active legacy projection is replaced.

use super::{
    consistency, revision, search, CaptureProvider, DataError, DataResult, PostProcessJob,
    PostProcessStage,
};
use bazza_domain::local_asr::{LocalAsrBody, LocalAsrOutcome};
use chrono::Utc;
use rusqlite::{params, Transaction};
use serde::Serialize;

const MAX_SEGMENT_ID: i64 = 9_007_199_254_740_991;

pub(super) struct InstalledLocalSource {
    pub revision: String,
    pub source_id: String,
    pub caption_count: u32,
}

#[derive(Serialize)]
struct RawSegment<'a> {
    id: i64,
    start_ms: i64,
    end_ms: i64,
    speaker: Option<&'a str>,
    text: &'a str,
}

#[derive(Serialize)]
struct FinalCaption<'a> {
    start_ms: i64,
    end_ms: i64,
    text: &'a str,
    is_final: bool,
}

#[derive(Serialize)]
struct LocalTranscript<'a> {
    provider: &'static str,
    model: &'a str,
    language: Option<&'a str>,
    status: &'static str,
    error: Option<&'static str>,
    captions: Vec<FinalCaption<'a>>,
}

pub(super) fn install_on(
    tx: &Transaction<'_>,
    job: &PostProcessJob,
    body: &LocalAsrBody,
    actor: &str,
) -> DataResult<InstalledLocalSource> {
    if job.provider.provider != CaptureProvider::Local || job.stage != PostProcessStage::Captured {
        return Err(DataError::Forbidden);
    }
    if !body.valid() {
        return Err(DataError::Invalid("local ASR body"));
    }
    let id = job.meeting_id.to_string();
    let (outcome, status, error) = match body.outcome {
        LocalAsrOutcome::Complete => ("complete", "complete", None),
        LocalAsrOutcome::NoModel => (
            "no_model",
            "incomplete",
            Some("No local speech model downloaded"),
        ),
    };
    let transcript = LocalTranscript {
        provider: "local",
        model: &body.model,
        language: body.language.as_deref(),
        status,
        error,
        captions: body
            .captions
            .iter()
            .map(|caption| FinalCaption {
                start_ms: caption.start_ms,
                end_ms: caption.end_ms,
                text: &caption.text,
                is_final: true,
            })
            .collect(),
    };
    // Finalize has already written a local capture projection. Require it to
    // still be local so this transition cannot overwrite a Soniox selection.
    // stt_previous belongs to the native explicit re-transcribe path.
    if tx.execute(
        "UPDATE settings SET value=?2 WHERE key=?1
         AND CASE WHEN json_valid(value) THEN json_extract(value,'$.provider') END='local'",
        params![format!("stt:{id}"), serde_json::to_string(&transcript)?],
    )? != 1
    {
        return Err(DataError::Forbidden);
    }

    let old_last: Option<i64> = tx.query_row(
        "SELECT max(id) FROM segments WHERE meeting_id=?1",
        [&id],
        |row| row.get(0),
    )?;
    let mut raw = Vec::with_capacity(body.captions.len());
    {
        // Insert before retiring the old projection, so a replacement cannot
        // immediately reuse IDs still referenced by the capture caption map.
        let mut insert = tx
            .prepare("INSERT INTO segments(meeting_id,start_ms,end_ms,text) VALUES(?1,?2,?3,?4)")?;
        for caption in &body.captions {
            insert.execute(params![id, caption.start_ms, caption.end_ms, caption.text])?;
            let segment_id = tx.last_insert_rowid();
            if !(1..=MAX_SEGMENT_ID).contains(&segment_id)
                || old_last.is_some_and(|last| segment_id <= last)
            {
                return Err(DataError::Invalid("local ASR segment identity"));
            }
            raw.push(RawSegment {
                id: segment_id,
                start_ms: caption.start_ms,
                end_ms: caption.end_ms,
                speaker: None,
                text: &caption.text,
            });
        }
    }
    if let Some(last) = old_last {
        tx.execute(
            "DELETE FROM segments WHERE meeting_id=?1 AND id<=?2",
            params![id, last],
        )?;
    }
    let source_id = revision();
    let revision = revision();
    tx.execute(
        "INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,language,raw_json,created_at)
         VALUES(?1,?2,'local_asr','local',?3,?4,?5)",
        params![source_id, id, body.language, serde_json::to_string(&raw)?, Utc::now()],
    )?;
    let caption_count = body.captions.len() as u32;
    tx.execute(
        "INSERT INTO service_post_process_local_sources(source_id,job_id,model,outcome,caption_count)
         VALUES(?1,?2,?3,?4,?5)",
        params![source_id, job.job_id, body.model, outcome, caption_count],
    )?;
    if tx.execute(
        "UPDATE service_transcript_state
         SET revision=?2,current_source_id=?3,document_state='present'
         WHERE meeting_id=?1 AND revision=?4 AND current_source_id IS ?5
         AND document_state IN ('absent','present')",
        params![
            id,
            revision,
            source_id,
            job.input.transcript_revision,
            job.input.source_id
        ],
    )? != 1
    {
        return Err(DataError::Forbidden);
    }
    tx.execute("DELETE FROM meeting_speakers WHERE meeting_id=?1", [&id])?;
    crate::settings::clear_meeting_voice_queries_on(tx, &id)?;
    crate::ai::rebuild_chunks_on(tx, &id)?;
    search::rebuild_meeting_on(tx, &id)?;
    consistency::change(tx, "transcript", &id, &revision, actor)?;

    Ok(InstalledLocalSource {
        revision,
        source_id,
        caption_count,
    })
}

#[cfg(test)]
#[path = "postprocess_source_install_tests.rs"]
mod tests;

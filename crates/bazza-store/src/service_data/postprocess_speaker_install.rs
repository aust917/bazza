//! Install anonymous labels against one fenced, immutable transcript source.

use super::{
    consistency, revision, search, CaptureProvider, DataError, DataResult, PostProcessJob,
    PostProcessStage,
};
use bazza_domain::{
    local_asr::LocalAsrOutcome,
    speaker_result::{SpeakerResultBody, SpeakerSkipReason, MAX_SPEAKER_ASSIGNMENTS},
};
use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::collections::BTreeMap;

pub(super) struct InstalledSpeakerResult {
    pub revision: String,
    pub changed_segments: u32,
}

const JS_SAFE: i64 = 9_007_199_254_740_991;

#[derive(PartialEq, Eq)]
struct SegmentIdentity {
    id: i64,
    start_ms: i64,
    end_ms: i64,
}

struct Segment {
    identity: SegmentIdentity,
    label: Option<String>,
}

fn segment_row(row: &Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        identity: SegmentIdentity {
            id: row.get(0)?,
            start_ms: row.get(1)?,
            end_ms: row.get(2)?,
        },
        label: row.get(3)?,
    })
}

pub(super) fn install_on(
    tx: &Transaction<'_>,
    job: &PostProcessJob,
    body: &SpeakerResultBody,
    actor: &str,
) -> DataResult<InstalledSpeakerResult> {
    let local_outcome = match (&job.stage, job.provider.provider) {
        (PostProcessStage::LocalAsr { outcome }, CaptureProvider::Local) => Some(outcome),
        (PostProcessStage::Captured, CaptureProvider::Soniox)
        | (PostProcessStage::CapturedLive, CaptureProvider::Local) => None,
        _ => return Err(DataError::Forbidden),
    };
    if !body.valid() {
        return Err(DataError::Invalid("speaker result body"));
    }
    let meeting = job.meeting_id.to_string();
    let source = job.input.source_id.as_deref().ok_or(DataError::Forbidden)?;
    let count: i64 = if let Some(outcome) = local_outcome {
        tx.query_row(
            "SELECT l.caption_count FROM service_transcript_state t
             JOIN service_transcript_sources s ON s.id=t.current_source_id AND s.meeting_id=t.meeting_id
             JOIN service_post_process_local_sources l ON l.source_id=s.id
             WHERE t.meeting_id=?1 AND t.revision=?2 AND t.document_state='present'
               AND s.id=?3 AND s.origin='local_asr' AND l.job_id=?4 AND l.outcome=?5
               AND json_type(s.raw_json)='array' AND json_array_length(s.raw_json)=l.caption_count",
            params![
                meeting,
                job.input.transcript_revision,
                source,
                job.job_id,
                match outcome {
                    LocalAsrOutcome::Complete => "complete",
                    LocalAsrOutcome::NoModel => "no_model",
                }
            ],
            |row| row.get(0),
        )
        .optional()?
    } else {
        tx.query_row(
            "SELECT c.last_confirmed_sequence FROM service_transcript_state t
             JOIN service_transcript_sources s ON s.id=t.current_source_id AND s.meeting_id=t.meeting_id
             JOIN service_capture_leases c ON c.meeting_id=t.meeting_id
             WHERE t.meeting_id=?1 AND t.revision=?2 AND t.document_state='present'
               AND s.id=?3 AND s.origin='captured' AND s.source_label=?8 AND s.language IS ?7
               AND c.id=?4 AND c.generation=?5 AND c.job_id=?6 AND c.state='finalized'
               AND json_type(s.raw_json)='array' AND json_array_length(s.raw_json)=c.last_confirmed_sequence",
            params![
                meeting, job.input.transcript_revision, source, job.input.capture_id.to_string(),
                job.input.capture_generation.to_string(), job.job_id, job.provider.language,
                match job.provider.provider {
                    CaptureProvider::Local => "local_live",
                    CaptureProvider::Soniox => "soniox",
                },
            ],
            |row| row.get(0),
        )
        .optional()?
    }
    .ok_or(DataError::Forbidden)?;
    if !(0..=MAX_SPEAKER_ASSIGNMENTS as i64).contains(&count) {
        return Err(DataError::Forbidden);
    }
    if matches!(
        body,
        SpeakerResultBody::Skipped {
            reason: SpeakerSkipReason::EmptyTranscript
        }
    ) && count != 0
    {
        return Err(DataError::Invalid("speaker skip reason"));
    }

    // Compare the immutable ASR order to the projection's insertion order,
    // which is by ID even when the captions' timestamps are out of order.
    let canonical = tx
        .prepare(
            "WITH source_rows AS (
                SELECT j.key AS ordinal, json_extract(j.value,'$.id') AS id,
                    json_extract(j.value,'$.start_ms') AS start_ms,
                    json_extract(j.value,'$.end_ms') AS end_ms,
                    CASE WHEN p.segment_key IS NOT NULL THEN p.label ELSE json_extract(j.value,'$.speaker') END AS label
                FROM service_transcript_sources s, json_each(s.raw_json) j
                LEFT JOIN service_transcript_speaker_overlays p
                  ON p.source_id=s.id AND p.segment_key=CAST(json_extract(j.value,'$.id') AS TEXT)
                WHERE s.id=?1
             )
             SELECT id,start_ms,end_ms,CASE WHEN length(CAST(label AS BLOB))>256 THEN char(0) ELSE label END
             FROM source_rows ORDER BY ordinal LIMIT ?2",
        )?
        .query_map(params![source, count + 1], segment_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let legacy = tx
        .prepare(
            "SELECT id,start_ms,end_ms,CASE WHEN length(CAST(speaker AS BLOB))>256 THEN char(0) ELSE speaker END
             FROM segments WHERE meeting_id=?1 ORDER BY id LIMIT ?2",
        )?
        .query_map(params![meeting, count + 1], segment_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if canonical.len() != count as usize
        || legacy.len() != canonical.len()
        || canonical.iter().zip(&legacy).any(|(raw, projected)| {
            let segment = &raw.identity;
            raw.identity != projected.identity
                || !(1..=JS_SAFE).contains(&segment.id)
                || segment.start_ms < 0
                || !(segment.start_ms..=JS_SAFE).contains(&segment.end_ms)
                || [&raw.label, &projected.label]
                    .into_iter()
                    .flatten()
                    .any(|label| label.contains('\0'))
        })
    {
        return Err(DataError::Forbidden);
    }
    if job.stage == PostProcessStage::CapturedLive
        && matches!(body, SpeakerResultBody::Applied { .. })
        && canonical.iter().chain(&legacy).any(|row| row.label.is_some())
    {
        return Err(DataError::Forbidden);
    }
    if local_outcome.is_none() && matches!(body, SpeakerResultBody::Skipped { .. }) {
        if canonical
            .iter()
            .zip(&legacy)
            .any(|(raw, projected)| raw.label != projected.label)
        {
            return Err(DataError::Forbidden);
        }
        // Missing optional diarization leaves the captured provider labels,
        // linked speaker metadata and retrieval projections as they were.
        return Ok(InstalledSpeakerResult {
            revision: job.input.transcript_revision.clone(),
            changed_segments: 0,
        });
    }
    let confirmed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM meeting_speakers WHERE meeting_id=?1 AND speaker_id IS NOT NULL)",
        [&meeting],
        |row| row.get(0),
    )?;
    if confirmed {
        return Err(DataError::Forbidden);
    }
    let mut names = BTreeMap::new();
    let labels = match body {
        SpeakerResultBody::Applied { assignments } => {
            if assignments.len() != canonical.len() {
                return Err(DataError::Invalid("speaker assignment coverage"));
            }
            assignments
                .iter()
                .zip(&canonical)
                .map(|(assignment, segment)| {
                    let segment = &segment.identity;
                    if assignment.segment_id != segment.id
                        || (assignment.cluster.is_some() && segment.start_ms == segment.end_ms)
                    {
                        return Err(DataError::Invalid("speaker assignment segment"));
                    }
                    Ok(assignment.cluster.map(|cluster| {
                        let next = names.len() + 1;
                        names
                            .entry(cluster)
                            .or_insert_with(|| format!("Speaker {next}"))
                            .clone()
                    }))
                })
                .collect::<DataResult<Vec<_>>>()?
        }
        SpeakerResultBody::Skipped { .. } => vec![None; canonical.len()],
    };

    let mut changed = 0;
    {
        let mut overlay = tx.prepare(
            "INSERT INTO service_transcript_speaker_overlays(source_id,segment_key,label)
             VALUES(?1,?2,?3) ON CONFLICT(source_id,segment_key) DO UPDATE SET label=excluded.label",
        )?;
        let mut update =
            tx.prepare("UPDATE segments SET speaker=?3 WHERE meeting_id=?1 AND id=?2")?;
        for ((raw, projected), label) in canonical.iter().zip(&legacy).zip(&labels) {
            let id = raw.identity.id;
            if raw.label != *label {
                overlay.execute(params![source, id.to_string(), label])?;
            }
            if projected.label != *label && update.execute(params![meeting, id, label])? != 1 {
                return Err(DataError::Forbidden);
            }
            if raw.label != *label || projected.label != *label {
                changed += 1;
            }
        }
    }
    tx.execute(
        "DELETE FROM meeting_speakers WHERE meeting_id=?1 AND speaker_id IS NULL",
        [&meeting],
    )?;
    {
        let mut insert =
            tx.prepare("INSERT INTO meeting_speakers(meeting_id,label) VALUES(?1,?2)")?;
        for label in names.values() {
            insert.execute(params![meeting, label])?;
        }
    }
    crate::settings::clear_meeting_voice_queries_on(tx, &meeting)?;
    let new_revision = if changed == 0 {
        job.input.transcript_revision.clone()
    } else {
        let revision = revision();
        if tx.execute(
            "UPDATE service_transcript_state SET revision=?2
             WHERE meeting_id=?1 AND revision=?3 AND current_source_id=?4 AND document_state='present'",
            params![meeting, revision, job.input.transcript_revision, source],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        crate::ai::rebuild_chunks_on(tx, &meeting)?;
        search::rebuild_meeting_on(tx, &meeting)?;
        consistency::change(tx, "transcript", &meeting, &revision, actor)?;
        revision
    };
    Ok(InstalledSpeakerResult {
        revision: new_revision,
        changed_segments: changed,
    })
}

#[cfg(test)]
#[path = "postprocess_speaker_install_tests.rs"]
mod tests;

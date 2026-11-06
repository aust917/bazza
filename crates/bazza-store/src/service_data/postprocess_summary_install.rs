//! Publish a configured summary under the current post-Ready worker claim.
use super::{
    consistency, documents, postprocess, DataError, DataResult, PostProcessJob, PostProcessStage,
};
use bazza_domain::{local_asr::LocalAsrOutcome, worker_summary::WorkerGeneratedSummary};
use rusqlite::{params, OptionalExtension, Transaction};

pub(super) struct InstalledWorkerSummary {
    pub revision: String,
    pub change_sequence: i64,
}

pub(super) fn install_on(
    tx: &Transaction<'_>,
    job: &PostProcessJob,
    body: WorkerGeneratedSummary,
    actor: &str,
) -> DataResult<InstalledWorkerSummary> {
    if !matches!(
        job.stage,
        PostProcessStage::Ready {
            local_asr: LocalAsrOutcome::Complete,
            cleanup: true,
            ..
        }
    ) {
        return Err(DataError::Forbidden);
    }
    if !body.valid() {
        return Err(DataError::Invalid("generated summary result"));
    }
    let id = job.meeting_id.to_string();
    let source = job.input.source_id.as_deref().ok_or(DataError::Forbidden)?;
    // Native automatic generation only runs when ASR yielded segments. The
    // source lineage and current legacy projection must both have content.
    let has_content: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM service_transcript_sources t
            JOIN meetings m ON m.id=t.meeting_id
            JOIN service_meeting_state s ON s.meeting_id=m.id
            WHERE t.id=?2 AND m.id=?3 AND m.status='ready'
              AND s.lifecycle_revision=?4 AND s.tombstoned=0
              AND ((t.origin='local_asr' AND EXISTS (
                    SELECT 1 FROM service_post_process_local_sources l
                    WHERE l.job_id=?1 AND l.source_id=t.id AND l.outcome='complete' AND l.caption_count>0))
                OR (t.origin='combined' AND EXISTS (
                    SELECT 1 FROM settings reg
                    WHERE reg.key='transcript-version-publication:'||length(m.id)||':'||m.id
                      AND CASE WHEN json_valid(reg.value) THEN json_extract(reg.value,'$.job_id') END=?1
                      AND CASE WHEN json_valid(reg.value) THEN json_extract(reg.value,'$.source_id') END=t.id
                      AND CASE WHEN json_valid(reg.value) THEN json_extract(reg.value,'$.effective_id') END=t.source_label))
                OR (t.origin='captured' AND t.source_label='local_live' AND EXISTS (
                    SELECT 1 FROM service_capture_leases c
                    WHERE c.job_id=?1 AND c.meeting_id=m.id AND c.state='finalized' AND c.last_confirmed_sequence>0))))
         AND EXISTS(SELECT 1 FROM segments WHERE meeting_id=?3)",
        params![job.job_id, source, id, job.input.lifecycle_revision],
        |row| row.get(0),
    )?;
    if !has_content {
        return Err(DataError::Forbidden);
    }
    let (template_revision, template_generation): (String, String) = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(s.revision AS BLOB))<=128 THEN s.revision END,
                    CASE WHEN length(CAST(s.generation AS BLOB))<=128 THEN s.generation END
             FROM service_template_state s JOIN templates t ON t.id=s.id
             WHERE s.id=?1 AND s.state='present' AND s.builtin=(t.builtin!=0)",
            [&body.template_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(DataError::NotFound)?;
    postprocess::rev(&template_revision)?;
    postprocess::rev(&template_generation)?;
    if template_revision != body.expected_template_revision
        || template_generation != body.expected_template_generation
    {
        return Err(DataError::Conflict {
            current_revision: template_revision,
        });
    }

    let revision = documents::install_generated_summary_on(
        tx,
        &id,
        &documents::GeneratedSummary {
            expected_summary_revision: body.expected_summary_revision,
            expected_transcript_revision: job.input.transcript_revision.clone(),
            markdown: body.markdown,
            structured_json: body.structured_json,
            template_id: body.template_id,
            model: body.model,
        },
        Some(source),
    )?;
    let change_sequence = consistency::change(tx, "summary", &id, &revision, actor)?;
    Ok(InstalledWorkerSummary {
        revision,
        change_sequence,
    })
}

#[cfg(test)]
#[path = "postprocess_summary_install_tests.rs"]
mod tests;

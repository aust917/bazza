//! Service-owned input lineage; immutable capture provenance is retained apart
//! from current revisions. Initialization is an explicit writer, never a read.
use super::{postprocess, *};
use bazza_domain::local_asr::LocalAsrOutcome;
use bazza_domain::speaker_result::SpeakerResultOutcome;
use chrono::Utc;
use rusqlite::{params, Transaction};

struct Anchor {
    fingerprint: String,
    lifecycle: String,
    transcript: String,
    source: Option<String>,
    document_state: String,
}

pub(super) struct Checkpoint {
    pub revision: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub source_id: Option<String>,
    pub document_state: String,
    pub phase: String,
    pub state: String,
}

pub(super) fn initialize_on(tx: &Transaction<'_>, job_id: &str) -> DataResult<()> {
    let value = postprocess::strict_capture_on(tx, job_id)?;
    tx.execute(
        "INSERT INTO service_post_process_anchors(job_id,meeting_id,capture_fingerprint,root_lifecycle_revision,root_transcript_revision,root_source_id,root_document_state) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![job_id,value.meeting_id.to_string(),value.capture_fingerprint,value.root_lifecycle_revision,value.root_transcript_revision,value.source_id,value.document_state],
    )?;
    tx.execute(
        "INSERT INTO service_post_process_checkpoints(job_id,revision,lifecycle_revision,transcript_revision,source_id,document_state,phase,state) VALUES(?1,?2,?3,?4,?5,?6,'captured','active')",
        params![job_id,revision(),value.lifecycle_revision,value.transcript_revision,value.source_id,value.document_state],
    )?;
    Ok(())
}

pub(super) fn candidate_on(
    conn: &Connection,
    value: postprocess::CaptureContext,
) -> DataResult<PostProcessJob> {
    let row: Option<(Anchor, Checkpoint)> = conn.query_row(
        "SELECT CASE WHEN length(CAST(a.capture_fingerprint AS BLOB))<=64 THEN a.capture_fingerprint END,CASE WHEN length(CAST(a.root_lifecycle_revision AS BLOB))<=128 THEN a.root_lifecycle_revision END,CASE WHEN length(CAST(a.root_transcript_revision AS BLOB))<=128 THEN a.root_transcript_revision END,CASE WHEN length(CAST(p.revision AS BLOB))<=128 THEN p.revision END,CAST(substr(CAST(a.root_source_id AS BLOB),1,129) AS TEXT),a.root_document_state,CASE WHEN length(CAST(p.lifecycle_revision AS BLOB))<=128 THEN p.lifecycle_revision END,CASE WHEN length(CAST(p.transcript_revision AS BLOB))<=128 THEN p.transcript_revision END,p.phase,p.state,CAST(substr(CAST(p.source_id AS BLOB),1,129) AS TEXT),p.document_state FROM service_post_process_anchors a JOIN service_post_process_checkpoints p ON p.job_id=a.job_id WHERE a.job_id=?1 AND a.meeting_id=?2",
        params![value.job_id,value.meeting_id.to_string()],
        |r| Ok((Anchor {
            fingerprint:r.get(0)?,lifecycle:r.get(1)?,transcript:r.get(2)?,source:r.get(4)?,document_state:r.get(5)?,
        }, Checkpoint {
            revision:r.get(3)?,lifecycle_revision:r.get(6)?,transcript_revision:r.get(7)?,phase:r.get(8)?,state:r.get(9)?,source_id:r.get(10)?,document_state:r.get(11)?,
        })),
    ).optional()?;
    let Some((anchor, checkpoint)) = row else {
        return Err(DataError::Forbidden);
    };
    let ready = matches!(checkpoint.phase.as_str(), "ready" | "ready_cleanup");
    let lifecycle_matches = if ready {
        value.meeting_status == "ready" && checkpoint.lifecycle_revision != anchor.lifecycle
    } else {
        value.meeting_status == "processing" && checkpoint.lifecycle_revision == anchor.lifecycle
    };
    if anchor.fingerprint != value.capture_fingerprint
        || anchor.lifecycle != value.root_lifecycle_revision
        || anchor.transcript != value.root_transcript_revision
        || checkpoint.state != "active"
        || !lifecycle_matches
        || (checkpoint.phase == "captured" && checkpoint.transcript_revision != anchor.transcript)
    {
        return Err(DataError::Forbidden);
    }
    postprocess::rev(&checkpoint.revision)?;
    postprocess::rev(&checkpoint.lifecycle_revision)?;
    postprocess::rev(&checkpoint.transcript_revision)?;
    check_revision(&value.lifecycle_revision, &checkpoint.lifecycle_revision)?;
    check_revision(&value.transcript_revision, &checkpoint.transcript_revision)?;
    if value.source_id != checkpoint.source_id || value.document_state != checkpoint.document_state
    {
        return Err(DataError::Forbidden);
    }
    postprocess::captured_source_on(
        conn,
        value.meeting_id,
        anchor.source.as_deref(),
        value.last_sequence,
        &anchor.document_state,
    )?;
    let original_source =
        checkpoint.source_id == anchor.source && checkpoint.document_state == anchor.document_state;
    let retained_live = original_source
        && value.provider.provider == CaptureProvider::Local
        && postprocess::retained_live_source_on(
            conn,
            value.meeting_id,
            checkpoint.source_id.as_deref(),
        )?;
    let combined = checkpoint.source_id.as_deref().map(|source| {
        crate::transcript_versions::registered_combined_on(conn, &value.meeting_id.to_string(), &value.job_id, source)
    }).transpose()?.unwrap_or(false);
    let stage = match checkpoint.phase.as_str() {
        phase @ ("ready" | "ready_cleanup") if !original_source && combined => {
            let source = checkpoint.source_id.as_deref().ok_or(DataError::Forbidden)?;
            PostProcessStage::Ready {
                local_asr: LocalAsrOutcome::Complete,
                speakers: SpeakerResultOutcome::Skipped { reason: bazza_domain::speaker_result::SpeakerSkipReason::Disabled },
                cleanup: phase == "ready_cleanup",
                generated_summary_revision: if phase == "ready_cleanup" {
                    generated_summary_revision_on(conn, &value.job_id, source, &checkpoint.transcript_revision)?
                } else { None },
            }
        }
        "captured" if retained_live => PostProcessStage::CapturedLive,
        "captured" if original_source => PostProcessStage::Captured,
        "automatic_cleanup" if original_source => {
            PostProcessStage::AutomaticCleanup { local_asr: None }
        }
        phase @ ("speakers" | "ready" | "ready_cleanup")
            if original_source
                && (value.provider.provider == CaptureProvider::Soniox || retained_live) =>
        {
            let source = checkpoint
                .source_id
                .as_deref()
                .ok_or(DataError::Forbidden)?;
            let speakers = speaker_outcome_on(conn, &value.job_id, source)?;
            let cleanup = phase == "ready_cleanup";
            let generated_summary_revision = if cleanup {
                generated_summary_revision_on(
                    conn,
                    &value.job_id,
                    source,
                    &checkpoint.transcript_revision,
                )?
            } else {
                None
            };
            match (value.provider.provider, ready) {
                (CaptureProvider::Soniox, true) => PostProcessStage::SonioxReady {
                    speakers,
                    cleanup,
                    generated_summary_revision,
                },
                (CaptureProvider::Soniox, false) => {
                    PostProcessStage::SonioxSpeakers { outcome: speakers }
                }
                (CaptureProvider::Local, true) => PostProcessStage::Ready {
                    local_asr: LocalAsrOutcome::Complete,
                    speakers,
                    cleanup,
                    generated_summary_revision,
                },
                (CaptureProvider::Local, false) => PostProcessStage::Speakers {
                    local_asr: LocalAsrOutcome::Complete,
                    outcome: speakers,
                },
            }
        }
        phase @ ("local_asr" | "automatic_cleanup" | "speakers" | "ready" | "ready_cleanup")
            if !original_source
                && value.provider.provider == CaptureProvider::Local
                && checkpoint.document_state == "present"
                && checkpoint.source_id != anchor.source =>
        {
            let source = checkpoint
                .source_id
                .as_deref()
                .ok_or(DataError::Forbidden)?;
            safe_id(source)?;
            let outcome: String = conn
                .query_row(
                    "SELECT l.outcome FROM service_post_process_local_sources l
                 JOIN service_transcript_sources t ON t.id=l.source_id
                 WHERE l.job_id=?1 AND l.source_id=?2 AND t.meeting_id=?3
                   AND t.origin='local_asr' AND t.source_label='local'",
                    params![value.job_id, source, value.meeting_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(DataError::Forbidden)?;
            let outcome = match outcome.as_str() {
                "complete" => LocalAsrOutcome::Complete,
                "no_model" => LocalAsrOutcome::NoModel,
                _ => return Err(DataError::Forbidden),
            };
            match phase {
                "local_asr" => PostProcessStage::LocalAsr { outcome },
                "automatic_cleanup" => PostProcessStage::AutomaticCleanup {
                    local_asr: Some(outcome),
                },
                _ => {
                    let speakers = speaker_outcome_on(conn, &value.job_id, source)?;
                    if ready {
                        PostProcessStage::Ready {
                            local_asr: outcome,
                            speakers,
                            cleanup: phase == "ready_cleanup",
                            generated_summary_revision: if phase == "ready_cleanup" {
                                generated_summary_revision_on(
                                    conn,
                                    &value.job_id,
                                    source,
                                    &checkpoint.transcript_revision,
                                )?
                            } else {
                                None
                            },
                        }
                    } else {
                        PostProcessStage::Speakers {
                            local_asr: outcome,
                            outcome: speakers,
                        }
                    }
                }
            }
        }
        _ => return Err(DataError::Forbidden),
    };
    Ok(PostProcessJob {
        job_id: value.job_id,
        meeting_id: value.meeting_id,
        revision: value.job_revision,
        input: PostProcessSourceFence {
            capture_id: value.capture_id,
            capture_generation: value.capture_generation,
            checkpoint_revision: checkpoint.revision,
            lifecycle_revision: checkpoint.lifecycle_revision,
            transcript_revision: checkpoint.transcript_revision,
            source_id: checkpoint.source_id,
        },
        provider: value.provider,
        stage,
    })
}

fn speaker_outcome_on(
    conn: &Connection,
    job: &str,
    source: &str,
) -> DataResult<SpeakerResultOutcome> {
    let body: String = conn
        .query_row(
            "SELECT outcome_json FROM service_post_process_speaker_results
         WHERE job_id=?1 AND source_id=?2 AND length(CAST(outcome_json AS BLOB))<=128",
            params![job, source],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(DataError::Forbidden)?;
    Ok(serde_json::from_str(&body)?)
}

fn generated_summary_revision_on(
    conn: &Connection,
    job: &str,
    source: &str,
    transcript_revision: &str,
) -> DataResult<Option<String>> {
    let row: Option<(bool, Option<String>)> = conn
        .query_row(
            "SELECT source_id=?2 AND transcript_revision=?3,
                CASE WHEN length(CAST(summary_revision AS BLOB))<=128 THEN summary_revision END
         FROM service_post_process_generated_summaries WHERE job_id=?1",
            params![job, source, transcript_revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((true, Some(revision))) => {
            postprocess::rev(&revision)?;
            Ok(Some(revision))
        }
        _ => Err(DataError::Forbidden),
    }
}

/// Called only by migration 16, inside its DDL/user_version transaction. Invalid
/// old candidates remain unavailable; no unrelated malformed job blocks upgrade.
pub(crate) fn migrate_on(tx: &Transaction<'_>) -> DataResult<()> {
    let mut after = 0_i64;
    loop {
        let rows = {
            let mut statement = tx.prepare("SELECT rowid,CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END FROM service_jobs WHERE kind='processing' AND executor='gui_worker' AND mandatory=0 AND rowid>?1 ORDER BY rowid LIMIT 100")?;
            let rows = statement
                .query_map([after], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if rows.is_empty() {
            break;
        }
        for (rowid, id) in rows {
            after = rowid;
            if let Some(id) = id {
                if safe_id(&id).is_ok() {
                    match postprocess::strict_capture_on(tx, &id) {
                        Ok(_) => initialize_on(tx, &id)?,
                        Err(error) if malformed_candidate(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }
    tx.execute("UPDATE service_worker_job_claims SET state='retired' WHERE state='active' AND job_id IN (SELECT id FROM service_jobs WHERE kind='processing' AND executor='gui_worker' AND mandatory=0)",[])?;
    let mut after = 0_i64;
    loop {
        let rows = {
            let mut statement = tx.prepare("SELECT rowid,CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END FROM service_jobs WHERE kind='processing' AND executor='gui_worker' AND mandatory=0 AND state='running' AND rowid>?1 ORDER BY rowid LIMIT 100")?;
            let rows = statement
                .query_map([after], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if rows.is_empty() {
            break;
        }
        for (rowid, id) in rows {
            after = rowid;
            let rev = revision();
            tx.execute("UPDATE service_jobs SET state='interrupted',revision=?2,claim_token=NULL,lease_until=NULL,updated_at=?3 WHERE rowid=?1",params![rowid,rev,Utc::now()])?;
            if let Some(id) = id.filter(|id| safe_id(id).is_ok()) {
                consistency::change(tx, "job", &id, &rev, "service")?;
            }
        }
    }
    Ok(())
}

fn malformed_candidate(error: &DataError) -> bool {
    match error {
        DataError::NotFound
        | DataError::Invalid(_)
        | DataError::Forbidden
        | DataError::Conflict { .. }
        | DataError::Json(_) => true,
        DataError::Sqlite(error) => matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..)
                | rusqlite::Error::InvalidColumnType(..)
        ),
        _ => false,
    }
}

pub(super) fn invalidate_on(tx: &Transaction<'_>, meeting_id: &str) -> DataResult<()> {
    super::postprocess_cleanup::invalidate_on(tx, meeting_id)?;
    tx.execute("UPDATE service_post_process_checkpoints SET state='invalidated',revision=?2 WHERE state='active' AND job_id IN (SELECT job_id FROM service_post_process_anchors WHERE meeting_id=?1)",params![meeting_id,revision()])?;
    tx.execute("UPDATE service_worker_job_claims SET state='retired' WHERE state='active' AND job_id IN (SELECT job_id FROM service_post_process_anchors WHERE meeting_id=?1)",[meeting_id])?;
    Ok(())
}

#[cfg(test)]
#[path = "postprocess_checkpoint_tests.rs"]
mod tests;

//! Terminal completion of a bounded fixture job after Ready.
use super::{postprocess, *};
use crate::Store;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TOOL: &str = "complete_fixture_post_process";
const MAX_RECEIPT_BYTES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteFixturePostProcessRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureCleanupOutcome {
    Accepted,
    SkippedEmptyTranscript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureExcludedStage {
    OutsideFixtureScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureSummaryOutcome {
    Committed { revision: String },
    OutsideFixtureScope {},
}

/// A historical terminal receipt carries no job lease or execution authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteFixturePostProcessReceipt {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub input: PostProcessSourceFence,
    pub cleanup: FixtureCleanupOutcome,
    pub embeddings: FixtureExcludedStage,
    pub summary: FixtureSummaryOutcome,
    pub webhook: FixtureExcludedStage,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

#[derive(Serialize)]
struct CompletionOutcome<'a> {
    cleanup: FixtureCleanupOutcome,
    embeddings: FixtureExcludedStage,
    summary: &'a FixtureSummaryOutcome,
    webhook: FixtureExcludedStage,
}

fn validate_receipt(
    request: &CompleteFixturePostProcessRequest,
    receipt: &CompleteFixturePostProcessReceipt,
    until: DateTime<Utc>,
) -> DataResult<()> {
    let old = &request.lease;
    postprocess::rev(&receipt.job_revision)?;
    if let FixtureSummaryOutcome::Committed { revision } = &receipt.summary {
        postprocess::rev(revision)?;
    }
    if receipt.operation_id != request.operation_id
        || receipt.claim_id != old.claim_id
        || receipt.worker_session_id != request.proof.session_id
        || receipt.library_epoch != request.proof.library_epoch
        || receipt.job_id != old.job_id
        || receipt.meeting_id != old.meeting_id
        || receipt.job_revision == old.job_revision
        || receipt.input != old.input
        || receipt.change_sequence <= old.change_sequence
        || receipt.change_sequence > 9_007_199_254_740_991
        || receipt.dedup_until != until
        || until < old.expires_at
        || until > Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS + 5)
    {
        return Err(DataError::Invalid("post-process completion receipt"));
    }
    postprocess::fingerprint(receipt, MAX_RECEIPT_BYTES)?;
    Ok(())
}

fn outcomes_on(
    tx: &Transaction<'_>,
    lease: &PostProcessJobLease,
) -> DataResult<(FixtureCleanupOutcome, FixtureSummaryOutcome)> {
    let outside_scope = FixtureSummaryOutcome::OutsideFixtureScope {};
    match postprocess::candidate_on(tx, &lease.job_id)?.stage {
        PostProcessStage::Ready {
            cleanup: true,
            generated_summary_revision,
            ..
        } | PostProcessStage::SonioxReady {
            cleanup: true,
            generated_summary_revision,
            ..
        } => Ok((
            FixtureCleanupOutcome::Accepted,
            generated_summary_revision.map_or(outside_scope, |revision| {
                FixtureSummaryOutcome::Committed { revision }
            }),
        )),
        PostProcessStage::Ready { cleanup: false, .. } => {
            // A persisted zero count alone cannot prove absence of work.
            // Verify both projections against the currently fenced source.
            let empty: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM service_post_process_local_sources l
                    JOIN service_transcript_sources s ON s.id=l.source_id
                    WHERE l.job_id=?1 AND l.source_id=?2 AND s.meeting_id=?3
                      AND l.caption_count=0 AND json_type(s.raw_json)='array'
                      AND json_array_length(s.raw_json)=0
                      AND NOT EXISTS(SELECT 1 FROM segments WHERE meeting_id=?3)
                 )",
                params![
                    lease.job_id,
                    lease.input.source_id,
                    lease.meeting_id.to_string()
                ],
                |row| row.get(0),
            )?;
            if empty {
                Ok((FixtureCleanupOutcome::SkippedEmptyTranscript, outside_scope))
            } else {
                Err(DataError::Forbidden)
            }
        }
        PostProcessStage::SonioxReady { cleanup: false, .. } => {
            let empty: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM service_transcript_sources s
                    WHERE s.id=?1 AND s.meeting_id=?2 AND s.origin='captured'
                      AND s.source_label='soniox' AND s.raw_json='[]'
                      AND NOT EXISTS(SELECT 1 FROM segments WHERE meeting_id=?2)
                )",
                params![lease.input.source_id, lease.meeting_id.to_string()],
                |row| row.get(0),
            )?;
            if !empty {
                return Err(DataError::Forbidden);
            }
            Ok((FixtureCleanupOutcome::SkippedEmptyTranscript, outside_scope))
        }
        _ => Err(DataError::Forbidden),
    }
}

fn retire_on(tx: &Transaction<'_>, lease: &PostProcessJobLease) -> DataResult<()> {
    if tx.execute(
        "UPDATE service_post_process_checkpoints SET state='completed',revision=?2
         WHERE job_id=?1 AND revision=?3 AND state='active' AND phase IN ('ready','ready_cleanup')",
        params![lease.job_id, revision(), lease.input.checkpoint_revision],
    )? != 1
    {
        return Err(DataError::Forbidden);
    }
    if tx.execute(
        "UPDATE service_worker_job_claims SET state='retired'
         WHERE claim_id=?1 AND job_id=?2 AND state='active'",
        params![lease.claim_id.to_string(), lease.job_id],
    )? != 1
    {
        return Err(DataError::Forbidden);
    }
    tx.execute(
        "UPDATE service_post_process_cleanup_chunks SET text=NULL
         WHERE upload_id IN (
             SELECT id FROM service_post_process_cleanup_uploads WHERE job_id=?1 AND state='open'
         )",
        [&lease.job_id],
    )?;
    tx.execute(
        "UPDATE service_post_process_cleanup_uploads SET state='invalidated'
         WHERE job_id=?1 AND state='open'",
        [&lease.job_id],
    )?;
    Ok(())
}

impl Store {
    pub fn service_complete_fixture_post_process(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &CompleteFixturePostProcessRequest,
    ) -> DataResult<CompleteFixturePostProcessReceipt> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        postprocess::worker_on(&tx, peer, epoch, &request.proof)?;
        let lease = &request.lease;
        postprocess::lease_valid(lease, &request.proof)?;
        if request.operation_id.is_nil() || lease.input.source_id.is_none() {
            return Err(DataError::Invalid("post-process completion request"));
        }
        request.fence.validate()?;
        let hash = postprocess::fingerprint(request, 4096)?;
        let actor = format!("worker:{}", request.proof.session_id);
        let key = request.operation_id.to_string();
        let saved: Option<(String, Option<String>, DateTime<Utc>)> = tx.query_row(
            "SELECT arguments_hash,CASE WHEN length(CAST(outcome_json AS BLOB))<=4096 THEN outcome_json END,expires_at
             FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
            params![actor, TOOL, key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        if let Some((saved_hash, body, until)) = saved {
            if until > Utc::now() {
                if saved_hash != hash {
                    return Err(DataError::IdempotencyConflict);
                }
                let receipt = serde_json::from_str(
                    body.as_deref()
                        .ok_or(DataError::Invalid("completion receipt budget"))?,
                )?;
                validate_receipt(request, &receipt, until)?;
                return Ok(receipt);
            }
            tx.execute(
                "DELETE FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
                params![actor, TOOL, key],
            )?;
        }
        let token = postprocess::check_binding(&tx, &request.proof, lease, &request.fence)?;
        let expires_at: DateTime<Utc> = tx.query_row(
            "SELECT lease_until FROM service_jobs WHERE id=?1",
            [&lease.job_id],
            |row| row.get(0),
        )?;
        if expires_at != lease.expires_at {
            return Err(DataError::Forbidden);
        }
        let (cleanup, summary) = outcomes_on(&tx, lease)?;
        let finished = execution::finish_job_on(
            &tx,
            &lease.job_id,
            &token,
            &lease.job_revision,
            None,
            Some(&actor),
        )?;
        if finished.state != "succeeded" {
            return Err(DataError::Forbidden);
        }
        // finish_job_on publishes the terminal job change before returning.
        let change_sequence: i64 = tx.query_row(
            "SELECT sequence FROM service_changes WHERE sequence=last_insert_rowid()
             AND entity_kind='job' AND entity_id=?1 AND revision=?2",
            params![finished.id, finished.revision],
            |row| row.get(0),
        )?;
        retire_on(&tx, lease)?;
        let completed_at = Utc::now();
        let receipt = CompleteFixturePostProcessReceipt {
            operation_id: request.operation_id,
            claim_id: lease.claim_id,
            worker_session_id: request.proof.session_id,
            library_epoch: request.proof.library_epoch,
            job_id: finished.id,
            meeting_id: lease.meeting_id,
            job_revision: finished.revision,
            input: lease.input.clone(),
            cleanup,
            embeddings: FixtureExcludedStage::OutsideFixtureScope,
            summary,
            webhook: FixtureExcludedStage::OutsideFixtureScope,
            change_sequence,
            dedup_until: completed_at + Duration::seconds(DEDUP_RETENTION_SECONDS),
        };
        validate_receipt(request, &receipt, receipt.dedup_until)?;
        let outcome = CompletionOutcome {
            cleanup: receipt.cleanup,
            embeddings: receipt.embeddings,
            summary: &receipt.summary,
            webhook: receipt.webhook,
        };
        tx.execute(
            "INSERT INTO service_post_process_completions(job_id,outcome_json,completed_at)
             VALUES(?1,?2,?3)",
            params![
                receipt.job_id,
                serde_json::to_string(&outcome)?,
                completed_at
            ],
        )?;
        tx.execute(
            "INSERT INTO service_idempotency(principal,tool,key,arguments_hash,outcome_json,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![actor, TOOL, key, hash, serde_json::to_string(&receipt)?, receipt.dedup_until],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}

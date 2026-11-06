//! A title-only registered-worker result. No job completion or source mutation.
use super::{postprocess, *};
use crate::Store;
use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TOOL: &str = "post_process_auto_title";
const MAX_TITLE_BYTES: usize = 512;
const MAX_REQUEST: usize = 65_024;
const MAX_RECEIPT: usize = 4096;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitPostProcessAutoTitleRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
    pub expected_metadata_revision: String,
    pub title: String,
}

impl std::fmt::Debug for CommitPostProcessAutoTitleRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitPostProcessAutoTitleRequest")
            .field("proof", &self.proof)
            .field("lease", &self.lease)
            .field("fence", &self.fence)
            .field("operation_id", &self.operation_id)
            .field(
                "expected_metadata_revision",
                &self.expected_metadata_revision,
            )
            .field("title_bytes", &self.title.len())
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessAutoTitleReceipt {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub metadata_revision: String,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

fn shape(request: &CommitPostProcessAutoTitleRequest) -> DataResult<String> {
    if request.operation_id.is_nil()
        || request.proof.session_id.is_nil()
        || request.proof.library_epoch.is_nil()
        || request.title.trim().is_empty()
    {
        return Err(DataError::Invalid("post-process auto-title request"));
    }
    bounded_text(&request.title, 1, MAX_TITLE_BYTES)?;
    postprocess::rev(&request.expected_metadata_revision)?;
    postprocess::lease_valid(&request.lease, &request.proof)?;
    request.fence.validate()?;
    postprocess::fingerprint(request, MAX_REQUEST)
}

fn receipt_valid(
    receipt: &PostProcessAutoTitleReceipt,
    request: &CommitPostProcessAutoTitleRequest,
) -> DataResult<()> {
    postprocess::rev(&receipt.metadata_revision)?;
    if receipt.operation_id != request.operation_id
        || receipt.claim_id != request.lease.claim_id
        || receipt.worker_session_id != request.proof.session_id
        || receipt.library_epoch != request.proof.library_epoch
        || receipt.job_id != request.lease.job_id
        || receipt.meeting_id != request.lease.meeting_id
        || receipt.metadata_revision == request.expected_metadata_revision
        || receipt.change_sequence <= request.lease.change_sequence
        || receipt.change_sequence > 9_007_199_254_740_991
        || receipt.dedup_until.timestamp_millis() <= 0
    {
        return Err(DataError::Invalid("post-process auto-title receipt"));
    }
    postprocess::fingerprint(receipt, MAX_RECEIPT)?;
    Ok(())
}

impl Store {
    pub fn service_commit_post_process_auto_title(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &CommitPostProcessAutoTitleRequest,
    ) -> DataResult<PostProcessAutoTitleReceipt> {
        let hash = shape(request)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Historical outcomes remain private to a currently authenticated
        // worker. They carry no authority to refresh or reuse a retired claim.
        let worker = postprocess::worker_on(&tx, peer, epoch, &request.proof)?;
        let principal = format!("worker:{}", worker.session_id);
        let key = request.operation_id.to_string();
        let saved: Option<(String, Option<String>, DateTime<Utc>)> = tx
            .query_row(
                "SELECT arguments_hash,CASE WHEN length(CAST(outcome_json AS BLOB))<=4096 THEN outcome_json END,expires_at FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
                params![principal, TOOL, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((saved_hash, body, until)) = saved {
            if until > Utc::now() {
                if saved_hash != hash {
                    return Err(DataError::IdempotencyConflict);
                }
                let receipt: PostProcessAutoTitleReceipt = serde_json::from_str(
                    body.as_deref()
                        .ok_or(DataError::Invalid("post-process auto-title receipt budget"))?,
                )?;
                receipt_valid(&receipt, request)?;
                if receipt.dedup_until != until {
                    return Err(DataError::Invalid("post-process auto-title retention"));
                }
                return Ok(receipt);
            }
            tx.execute(
                "DELETE FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
                params![principal, TOOL, key],
            )?;
        }
        postprocess::check_binding(&tx, &request.proof, &request.lease, &request.fence)?;
        let id = request.lease.meeting_id.to_string();
        let (actual, automatic): (String, i64) = tx.query_row(
            "SELECT CASE WHEN length(CAST(s.metadata_revision AS BLOB))<=128 THEN s.metadata_revision END,m.title_auto FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id WHERE m.id=?1 AND m.status IN ('processing','ready') AND s.tombstoned=0",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        check_revision(&actual, &request.expected_metadata_revision)?;
        if automatic != 1 {
            return Err(DataError::Forbidden);
        }
        // Match legacy set_title(..., false): an automatic title does not pin
        // title_auto=0. A later explicit user rename still wins permanently.
        if tx.execute(
            "UPDATE meetings SET title=?2 WHERE id=?1 AND title_auto=1 AND status IN ('processing','ready')",
            params![id, request.title],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        let metadata = revision();
        if tx.execute(
            "UPDATE service_meeting_state SET metadata_revision=?2 WHERE meeting_id=?1 AND metadata_revision=?3 AND tombstoned=0",
            params![id, metadata, request.expected_metadata_revision],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        search::refresh_title_on(&tx, &id, &metadata, &request.title)?;
        let sequence = consistency::change(&tx, "meeting", &id, &metadata, &principal)?;
        let receipt = PostProcessAutoTitleReceipt {
            operation_id: request.operation_id,
            claim_id: request.lease.claim_id,
            worker_session_id: worker.session_id,
            library_epoch: epoch,
            job_id: request.lease.job_id.clone(),
            meeting_id: request.lease.meeting_id,
            metadata_revision: metadata,
            change_sequence: sequence,
            dedup_until: Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS),
        };
        receipt_valid(&receipt, request)?;
        tx.execute(
            "INSERT INTO service_idempotency(principal,tool,key,arguments_hash,outcome_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![principal, TOOL, key, hash, serde_json::to_string(&receipt)?, receipt.dedup_until],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "postprocess_result_tests.rs"]
mod tests;

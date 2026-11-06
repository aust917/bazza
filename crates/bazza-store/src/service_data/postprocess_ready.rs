//! Ready is a lifecycle checkpoint. Optional work retains its existing claim.
use super::{postprocess, *};
use crate::Store;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TOOL: &str = "post_process_ready";
const MAX_RECEIPT_BYTES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessReadyRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub operation_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessReadyReceipt {
    pub lease: PostProcessJobLease,
}

fn validate_receipt(
    request: &PostProcessReadyRequest,
    receipt: &PostProcessReadyReceipt,
    until: DateTime<Utc>,
) -> DataResult<()> {
    let old = &request.lease;
    let new = &receipt.lease;
    postprocess::lease_valid(new, &request.proof)?;
    if !postprocess::same_claim_rotation(old, new, request.operation_id)
        || new.input.lifecycle_revision == old.input.lifecycle_revision
        || new.input.transcript_revision != old.input.transcript_revision
        || new.input.source_id != old.input.source_id
        || new.dedup_until != until
        || new.dedup_until > Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS + 5)
    {
        return Err(DataError::Invalid("post-process ready receipt"));
    }
    postprocess::fingerprint(receipt, MAX_RECEIPT_BYTES)?;
    Ok(())
}

impl Store {
    pub fn service_mark_post_process_ready(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessReadyRequest,
    ) -> DataResult<PostProcessReadyReceipt> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        postprocess::worker_on(&tx, peer, epoch, &request.proof)?;
        postprocess::lease_valid(&request.lease, &request.proof)?;
        if request.operation_id.is_nil() || request.lease.input.source_id.is_none() {
            return Err(DataError::Invalid("post-process ready request"));
        }
        request.fence.validate()?;
        let hash = postprocess::fingerprint(request, 4096)?;
        let actor = format!("worker:{}", request.proof.session_id);
        let key = request.operation_id.to_string();
        let saved: Option<(String, Option<String>, DateTime<Utc>)> = tx.query_row(
            "SELECT arguments_hash,CASE WHEN length(CAST(outcome_json AS BLOB))<=4096 THEN outcome_json END,expires_at
             FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
            params![actor, TOOL, key],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        if let Some((saved_hash, body, until)) = saved {
            if until > Utc::now() {
                if saved_hash != hash {
                    return Err(DataError::IdempotencyConflict);
                }
                let receipt = serde_json::from_str(
                    body.as_deref()
                        .ok_or(DataError::Invalid("ready receipt budget"))?,
                )?;
                validate_receipt(request, &receipt, until)?;
                return Ok(receipt);
            }
            tx.execute(
                "DELETE FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
                params![actor, TOOL, key],
            )?;
        }
        let token =
            postprocess::check_binding(&tx, &request.proof, &request.lease, &request.fence)?;
        if !super::postprocess_video::screen_settled_on(&tx, &request.lease)? {
            return Err(DataError::Forbidden);
        }
        let meeting = request.lease.meeting_id.to_string();
        if tx.execute(
            "UPDATE meetings SET status='ready' WHERE id=?1 AND status='processing'
             AND EXISTS(SELECT 1 FROM service_post_process_checkpoints
                        WHERE job_id=?2 AND revision=?3 AND phase='speakers' AND state='active')",
            params![
                meeting,
                request.lease.job_id,
                request.lease.input.checkpoint_revision
            ],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        let lifecycle = revision();
        if tx.execute(
            "UPDATE service_meeting_state SET lifecycle_revision=?2
             WHERE meeting_id=?1 AND lifecycle_revision=?3 AND tombstoned=0",
            params![meeting, lifecycle, request.lease.input.lifecycle_revision],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        consistency::change(&tx, "meeting_lifecycle", &meeting, &lifecycle, &actor)?;
        let lease = postprocess::advance_result_on(
            &tx,
            &request.proof,
            &request.lease,
            &token,
            postprocess::InstalledResult::Ready {
                lifecycle_revision: &lifecycle,
            },
            request.operation_id,
        )?;
        let receipt = PostProcessReadyReceipt { lease };
        validate_receipt(request, &receipt, receipt.lease.dedup_until)?;
        tx.execute(
            "INSERT INTO service_idempotency(principal,tool,key,arguments_hash,outcome_json,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![actor, TOOL, key, hash, serde_json::to_string(&receipt)?, receipt.lease.dedup_until],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}

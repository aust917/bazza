use super::*;
use crate::Store;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEDUP_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Trusted caller identity. Never accept the principal from untrusted tool input.
#[derive(Clone, Debug)]
pub struct MutationContext {
    pub principal: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationOutcome {
    pub entity_id: String,
    pub revision: String,
    pub change_sequence: i64,
    pub job_id: Option<String>,
    pub dedup_until: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub sequence: i64,
    pub entity_kind: String,
    pub entity_id: String,
    pub revision: String,
    pub actor: String,
}

pub(super) fn change(
    tx: &Transaction<'_>,
    kind: &str,
    id: &str,
    rev: &str,
    actor: &str,
) -> DataResult<i64> {
    tx.execute("INSERT INTO service_changes(entity_kind,entity_id,revision,actor,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![kind,id,rev,actor,Utc::now()])?;
    Ok(tx.last_insert_rowid())
}

pub(super) fn outcome(
    tx: &Transaction<'_>,
    kind: &str,
    id: &str,
    rev: &str,
    ctx: &MutationContext,
) -> DataResult<MutationOutcome> {
    Ok(MutationOutcome {
        entity_id: id.into(),
        revision: rev.into(),
        change_sequence: change(tx, kind, id, rev, &ctx.principal)?,
        job_id: None,
        dedup_until: Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS),
    })
}

pub(super) fn mutate<T: Serialize>(
    store: &Store,
    ctx: &MutationContext,
    tool: &str,
    payload: &T,
    action: impl FnOnce(&Transaction<'_>) -> DataResult<MutationOutcome>,
) -> DataResult<MutationOutcome> {
    bounded_text(&ctx.principal, 1, 128)?;
    bounded_text(&ctx.idempotency_key, 1, 128)?;
    if !ctx
        .idempotency_key
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
    {
        return Err(DataError::Invalid("idempotency key"));
    }
    // Check size with a counting writer before allocating normalized copies.
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= 1_048_576)
                .ok_or_else(|| std::io::Error::other("request budget"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(0), payload).map_err(|_| DataError::Invalid("request budget"))?;
    // serde_json Value maps have sorted keys; hashes ignore struct/map key order.
    let bytes = serde_json::to_vec(&serde_json::to_value(payload)?)?;
    if bytes.len() > 1_048_576 {
        return Err(DataError::Invalid("request budget"));
    }
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut conn = store.conn.lock();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let cached:Option<(String,String,DateTime<Utc>)>=tx.query_row(
        "SELECT arguments_hash,outcome_json,expires_at FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
        params![ctx.principal,tool,ctx.idempotency_key],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((saved, body, expires)) = cached {
        if expires > Utc::now() {
            if saved != hash {
                return Err(DataError::IdempotencyConflict);
            }
            return Ok(serde_json::from_str(&body)?);
        }
        tx.execute(
            "DELETE FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
            params![ctx.principal, tool, ctx.idempotency_key],
        )?;
    }
    let result = action(&tx)?;
    tx.execute(
        "INSERT INTO service_idempotency VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            ctx.principal,
            tool,
            ctx.idempotency_key,
            hash,
            serde_json::to_string(&result)?,
            result.dedup_until
        ],
    )?;
    tx.commit()?;
    Ok(result)
}

impl Store {
    pub fn service_changes(&self, after: i64, limit: usize) -> DataResult<Vec<Change>> {
        if after < 0 || !(1..=100).contains(&limit) {
            return Err(DataError::Invalid("change page"));
        }
        let conn = self.conn.lock();
        let mut st=conn.prepare("SELECT sequence,entity_kind,entity_id,revision,actor FROM service_changes WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let rows = st
            .query_map(params![after, limit as i64], |r| {
                Ok(Change {
                    sequence: r.get(0)?,
                    entity_kind: r.get(1)?,
                    entity_id: r.get(2)?,
                    revision: r.get(3)?,
                    actor: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
}

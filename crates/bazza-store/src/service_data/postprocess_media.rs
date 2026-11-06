//! Database-only admission for captured media. Core owns file and owner checks.
use super::{postprocess, *};
use crate::Store;
use bazza_domain::{capture_screen::RawScreenEvidence, capture_stt::SonioxCaptureOutcome};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessMediaRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub owner_nonce: Uuid,
    pub expected_provider: CaptureSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostProcessMediaContext {
    pub library_scope: String,
    pub capture_worker_session_id: Uuid,
    pub last_confirmed_sequence: u64,
    pub provider: CaptureSelection,
    pub raw_screen: Option<RawScreenEvidence>,
    pub soniox_outcome: Option<SonioxCaptureOutcome>,
    pub video_record: Option<PostProcessVideoRecord>,
    pub expires_at: DateTime<Utc>,
}

impl Store {
    pub fn service_post_process_media_context(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessMediaRequest,
    ) -> DataResult<PostProcessMediaContext> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        post_process_media_context_on(&tx, peer, epoch, request)
    }
}

pub(super) fn post_process_media_context_on(
    conn: &rusqlite::Connection,
    peer: u32,
    epoch: Uuid,
    request: &PostProcessMediaRequest,
) -> DataResult<PostProcessMediaContext> {
    // Authenticate before inspecting the target job or capture rows. This
    // read has no replay receipt, registration renewal or other write.
    postprocess::worker_on(conn, peer, epoch, &request.proof)?;
    postprocess::lease_valid(&request.lease, &request.proof)?;
    if request.owner_nonce.is_nil() {
        return Err(DataError::Invalid("post-process media owner"));
    }
    if request.lease.expires_at <= Utc::now() {
        return Err(DataError::Forbidden);
    }
    workers::selection(&request.expected_provider)?;
    request.fence.validate()?;
    postprocess::fingerprint(request, POST_PROCESS_MAX_DATA_BYTES as usize)?;
    postprocess::check_binding(conn, &request.proof, &request.lease, &request.fence)?;
    let candidate = postprocess::candidate_on(conn, &request.lease.job_id)?;
    if candidate.provider != request.expected_provider {
        return Err(DataError::Forbidden);
    }
    let capture = &request.lease.input;
    let row: Option<(Option<String>, i64, Option<DateTime<Utc>>)> = conn
            .query_row(
                "SELECT CASE WHEN length(CAST(c.worker_session_id AS BLOB))<=128 THEN c.worker_session_id END,c.last_confirmed_sequence,CASE WHEN length(CAST(j.lease_until AS BLOB))<=128 THEN j.lease_until END FROM service_capture_leases c JOIN service_jobs j ON j.id=c.job_id WHERE c.id=?1 AND c.meeting_id=?2 AND c.generation=?3 AND c.state='finalized' AND c.job_id=?4 AND j.state='running'",
                params![
                    capture.capture_id.to_string(),
                    request.lease.meeting_id.to_string(),
                    capture.capture_generation.to_string(),
                    request.lease.job_id,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
    let (capture_worker, last_sequence, expires_at) = row.ok_or(DataError::Forbidden)?;
    let capture_worker_session_id = capture_worker
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
        .ok_or(DataError::Invalid("post-process capture worker"))?;
    let last_confirmed_sequence = u64::try_from(last_sequence)
        .ok()
        .filter(|value| *value <= 10_000)
        .ok_or(DataError::Invalid("post-process confirmed sequence"))?;
    let expires_at = expires_at.ok_or(DataError::Forbidden)?;
    if expires_at != request.lease.expires_at || expires_at <= Utc::now() {
        return Err(DataError::Forbidden);
    }
    let (library_scope, _) = postprocess::snapshot_on(conn)?;
    let raw_screen: Option<String> = conn
            .query_row(
                "SELECT CASE WHEN length(CAST(evidence_json AS BLOB))<=1024 THEN evidence_json ELSE '{}' END
                 FROM service_capture_terminal_screen WHERE capture_id=?1",
                [capture.capture_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
    let raw_screen: Option<RawScreenEvidence> = raw_screen
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;
    if raw_screen.as_ref().is_some_and(|raw| !raw.valid()) {
        return Err(DataError::Invalid("post-process raw screen evidence"));
    }
    let soniox_outcome: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN length(CAST(outcome_json AS BLOB)) BETWEEN 1 AND 512
                            THEN outcome_json ELSE '{}' END
                 FROM service_capture_terminal_stt WHERE capture_id=?1",
            [capture.capture_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let soniox_outcome = soniox_outcome
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;
    Ok(PostProcessMediaContext {
        library_scope,
        capture_worker_session_id,
        last_confirmed_sequence,
        provider: candidate.provider,
        raw_screen,
        soniox_outcome,
        video_record: super::postprocess_video::record_on(conn, &request.lease)?,
        expires_at,
    })
}

#[cfg(test)]
#[path = "postprocess_media_tests.rs"]
mod tests;

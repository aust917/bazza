//! Conservative idle retention, never worker authorization or a writer-drain
//! proof. Waiting jobs and registrations alone do not retain the service.
use super::{DataError, DataResult};
use crate::Store;
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

impl Store {
    /// Check at the actual idle boundary. A stale source can conservatively
    /// retain the host until the earlier job/worker expiry; writes still require
    /// the separate full proof, token hash, revision and source-fence checks.
    pub fn service_has_live_post_process_claims(&self, current_epoch: Uuid) -> DataResult<bool> {
        if current_epoch.is_nil() {
            return Err(DataError::Invalid("post-process liveness epoch"));
        }
        let conn = self.conn.lock();
        Ok(conn.query_row(
            "SELECT EXISTS(
              SELECT 1 FROM service_worker_job_claims b
              JOIN service_jobs j ON j.id=b.job_id
              JOIN service_worker_sessions w ON w.id=b.worker_session_id
              WHERE b.state='active' AND b.library_epoch=?1
                AND j.kind='processing' AND j.executor='gui_worker'
                AND j.state='running' AND j.mandatory=0 AND j.cancel_requested=0
                AND j.claim_token IS NOT NULL AND length(j.claim_token)>0
                AND j.lease_until>?2
                AND w.library_epoch=?1 AND w.peer_pid>0 AND w.revoked=0
                AND w.expires_at>?2
                AND EXISTS(
                  SELECT 1 FROM json_each(
                    CASE WHEN length(CAST(w.capabilities_json AS BLOB))<=128
                      THEN CASE WHEN json_valid(w.capabilities_json)
                        THEN CASE WHEN json_type(w.capabilities_json)='array'
                          THEN w.capabilities_json ELSE '[]' END
                        ELSE '[]' END
                      ELSE '[]' END
                  ) cap WHERE cap.type='text' AND cap.value='post_process'
                )
            )",
            params![current_epoch.to_string(), Utc::now()],
            |row| row.get(0),
        )?)
    }
}

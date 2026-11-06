//! Durable service executors. Files are staged under unique IDs and become
//! visible only after a second, transactional generation/claim check.
use super::*;
use crate::Store;
use bazza_media_gate::{MediaGateError, MediaPurgeGuard};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Row, Transaction};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    CalendarRefresh,
    SharePreparation,
    TranscriptImport,
    MeetingDeletion,
    ContentDeletion,
    IndexRebuild,
    Capture,
    Processing,
}
impl JobKind {
    fn name(self) -> &'static str {
        match self {
            Self::CalendarRefresh => "calendar_refresh",
            Self::SharePreparation => "share_preparation",
            Self::TranscriptImport => "transcript_import",
            Self::MeetingDeletion => "meeting_deletion",
            Self::ContentDeletion => "content_deletion",
            Self::IndexRebuild => "index_rebuild",
            Self::Capture => "capture",
            Self::Processing => "processing",
        }
    }
    fn mandatory(self) -> bool {
        matches!(
            self,
            Self::Capture | Self::MeetingDeletion | Self::ContentDeletion
        )
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobExecutor {
    Service,
    GuiWorker,
}
impl JobExecutor {
    fn name(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::GuiWorker => "gui_worker",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewJob {
    pub kind: JobKind,
    pub entity_id: Option<String>,
    pub executor: JobExecutor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurableJob {
    pub id: String,
    pub kind: String,
    pub entity_id: Option<String>,
    pub executor: String,
    pub state: String,
    pub revision: String,
    pub actor: String,
    pub cancel_requested: bool,
    pub mandatory: bool,
    pub attempts: i64,
    pub error_code: Option<String>,
}
#[derive(Clone, Debug)]
pub struct JobClaim {
    pub job: DurableJob,
    pub token: String,
    pub lease_until: DateTime<Utc>,
}
const JOB_COLUMNS:&str="id,kind,entity_id,executor,state,revision,actor,cancel_requested,mandatory,attempts,error_code";
fn job_row(r: &Row<'_>) -> rusqlite::Result<DurableJob> {
    Ok(DurableJob {
        id: r.get(0)?,
        kind: r.get(1)?,
        entity_id: r.get(2)?,
        executor: r.get(3)?,
        state: r.get(4)?,
        revision: r.get(5)?,
        actor: r.get(6)?,
        cancel_requested: r.get(7)?,
        mandatory: r.get(8)?,
        attempts: r.get(9)?,
        error_code: r.get(10)?,
    })
}
pub(super) fn job_on(conn: &Connection, id: &str) -> DataResult<DurableJob> {
    conn.query_row(
        &format!("SELECT {JOB_COLUMNS} FROM service_jobs WHERE id=?1"),
        [id],
        job_row,
    )
    .optional()?
    .ok_or(DataError::NotFound)
}
pub(super) fn insert_job(
    tx: &Transaction<'_>,
    kind: &str,
    entity_id: Option<&str>,
    executor: &str,
    mandatory: bool,
    actor: &str,
    payload: &str,
) -> DataResult<String> {
    let id = revision();
    let rev = revision();
    let now = Utc::now();
    tx.execute("INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,mandatory,payload_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
        params![id,kind,entity_id,executor,if executor=="gui_worker" {"waiting_for_worker"} else {"queued"},rev,actor,mandatory,payload,now])?;
    Ok(id)
}

pub(super) fn check_claim(
    conn: &Connection,
    id: &str,
    token: &str,
    expected: Option<&str>,
) -> DataResult<DurableJob> {
    let job = job_on(conn, id)?;
    if let Some(expected) = expected {
        check_revision(&job.revision, expected)?;
    }
    let (saved, until): (Option<String>, Option<DateTime<Utc>>) = conn.query_row(
        "SELECT claim_token,lease_until FROM service_jobs WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if job.state != "running"
        || saved.as_deref() != Some(token)
        || until.is_none_or(|v| v <= Utc::now())
    {
        return Err(DataError::Forbidden);
    }
    Ok(job)
}

/// Transaction-local executor primitives. Registered-worker callers must check
/// their own proof, capability, fences and bounded expiry in this same tx.
pub(super) fn claim_job_on(
    tx: &Transaction<'_>,
    id: &str,
    executor: JobExecutor,
    expected: &str,
    until: DateTime<Utc>,
    reclaim_running: bool,
    actor: Option<&str>,
) -> DataResult<JobClaim> {
    let job = job_on(tx, id)?;
    check_revision(&job.revision, expected)?;
    if job.kind == "capture" || job.executor != executor.name() {
        return Err(DataError::Forbidden);
    }
    let lease: Option<DateTime<Utc>> = tx.query_row(
        "SELECT lease_until FROM service_jobs WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if !(matches!(
        job.state.as_str(),
        "queued" | "waiting_for_worker" | "interrupted"
    ) || (job.state == "running"
        && (reclaim_running || lease.is_some_and(|v| v <= Utc::now()))))
    {
        return Err(DataError::Busy);
    }
    if job.cancel_requested {
        return Err(DataError::Forbidden);
    }
    let token = revision();
    let rev = revision();
    if tx.execute("UPDATE service_jobs SET state='running',revision=?2,claim_token=?3,lease_until=?4,attempts=attempts+1,updated_at=?5 WHERE id=?1 AND revision=?6",params![id,rev,token,until,Utc::now(),expected])? != 1 {
        return Err(DataError::Busy);
    }
    // A trusted legacy executor reclaim rotates the internal token too. Its
    // old registered-worker fence must not inherit that new claim authority.
    tx.execute(
        "UPDATE service_worker_job_claims SET state='retired' WHERE job_id=?1 AND state='active'",
        [id],
    )?;
    change(tx, "job", id, &rev, actor.unwrap_or(&job.actor))?;
    Ok(JobClaim {
        job: job_on(tx, id)?,
        token,
        lease_until: until,
    })
}

pub(super) fn renew_job_on(
    tx: &Transaction<'_>,
    id: &str,
    token: &str,
    expected: &str,
    until: DateTime<Utc>,
    actor: Option<&str>,
) -> DataResult<JobClaim> {
    let job = check_claim(tx, id, token, Some(expected))?;
    if job.cancel_requested {
        return Err(DataError::Forbidden);
    }
    let rev = revision();
    if tx.execute("UPDATE service_jobs SET revision=?2,lease_until=?3,updated_at=?4 WHERE id=?1 AND revision=?5",params![id,rev,until,Utc::now(),expected])? != 1 {
        return Err(DataError::Busy);
    }
    change(tx, "job", id, &rev, actor.unwrap_or(&job.actor))?;
    Ok(JobClaim {
        job: job_on(tx, id)?,
        token: token.into(),
        lease_until: until,
    })
}

pub(super) fn finish_job_on(
    tx: &Transaction<'_>,
    id: &str,
    token: &str,
    expected: &str,
    error_code: Option<&str>,
    actor: Option<&str>,
) -> DataResult<DurableJob> {
    if let Some(code) = error_code {
        validate_error_code(code)?;
    }
    let job = check_claim(tx, id, token, Some(expected))?;
    if job.mandatory {
        return Err(DataError::Forbidden);
    }
    let state = if job.cancel_requested {
        "cancelled"
    } else if error_code.is_some() {
        "failed"
    } else {
        "succeeded"
    };
    let rev = revision();
    if tx.execute("UPDATE service_jobs SET state=?2,revision=?3,error_code=?4,claim_token=NULL,lease_until=NULL,updated_at=?5 WHERE id=?1 AND revision=?6",params![id,state,rev,error_code,Utc::now(),expected])? != 1 {
        return Err(DataError::Busy);
    }
    change(tx, "job", id, &rev, actor.unwrap_or(&job.actor))?;
    job_on(tx, id)
}

impl Store {
    pub fn service_enqueue_job(
        &self,
        ctx: &MutationContext,
        new: &NewJob,
    ) -> DataResult<MutationOutcome> {
        if let Some(id) = &new.entity_id {
            safe_id(id)?;
        }
        // Mandatory deletion jobs have a stronger, entity-specific entry point.
        if matches!(
            new.kind,
            JobKind::MeetingDeletion | JobKind::ContentDeletion | JobKind::Capture
        ) {
            return Err(DataError::Forbidden);
        }
        mutate(self, ctx, "enqueue_job", new, |tx| {
            if let Some(id) = &new.entity_id {
                live_meeting(tx, id)?;
            }
            let id = insert_job(
                tx,
                new.kind.name(),
                new.entity_id.as_deref(),
                new.executor.name(),
                new.kind.mandatory(),
                &ctx.principal,
                "{}",
            )?;
            let job = job_on(tx, &id)?;
            let mut out = outcome(tx, "job", &id, &job.revision, ctx)?;
            out.job_id = Some(id);
            Ok(out)
        })
    }

    pub fn service_job(&self, id: &str) -> DataResult<DurableJob> {
        safe_id(id)?;
        job_on(&self.conn.lock(), id)
    }

    /// Internal executor operation. The token changes on every reclaim; an old
    /// worker cannot complete or publish after another worker has claimed it.
    pub fn service_claim_job(
        &self,
        id: &str,
        executor: JobExecutor,
        expected: &str,
        lease_seconds: i64,
    ) -> DataResult<JobClaim> {
        if !(1..=300).contains(&lease_seconds) {
            return Err(DataError::Invalid("lease duration"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let until = Utc::now() + Duration::seconds(lease_seconds);
        let result = claim_job_on(&tx, id, executor, expected, until, false, None)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn service_renew_job(
        &self,
        id: &str,
        token: &str,
        expected: &str,
        lease_seconds: i64,
    ) -> DataResult<JobClaim> {
        if !(1..=300).contains(&lease_seconds) {
            return Err(DataError::Invalid("lease duration"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let until = Utc::now() + Duration::seconds(lease_seconds);
        let result = renew_job_on(&tx, id, token, expected, until, None)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn service_finish_job(
        &self,
        id: &str,
        token: &str,
        expected: &str,
        error_code: Option<&str>,
    ) -> DataResult<DurableJob> {
        if let Some(code) = error_code {
            validate_error_code(code)?;
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let finished = finish_job_on(&tx, id, token, expected, error_code, None)?;
        tx.commit()?;
        Ok(finished)
    }

    pub fn service_cancel_job(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        mutate(self, ctx, "cancel_job", &(id, expected), |tx| {
            let job = job_on(tx, id)?;
            check_revision(&job.revision, expected)?;
            if job.mandatory || job.kind == "capture" {
                return Err(DataError::Forbidden);
            }
            if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
                return Err(DataError::Busy);
            }
            let rev = revision();
            let state = if job.state == "running" {
                "running"
            } else {
                "cancelled"
            };
            tx.execute("UPDATE service_jobs SET cancel_requested=1,state=?2,revision=?3,updated_at=?4 WHERE id=?1",params![id,state,rev,Utc::now()])?;
            outcome(tx, "job", id, &rev, ctx)
        })
    }

    pub fn service_recover_expired_jobs(&self) -> DataResult<usize> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let jobs = {
            let mut st=tx.prepare(&format!("SELECT {JOB_COLUMNS} FROM service_jobs WHERE state='running' AND lease_until<=?1 AND kind!='capture'"))?;
            let rows = st
                .query_map([Utc::now()], job_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for job in &jobs {
            let rev = revision();
            tx.execute("UPDATE service_jobs SET state=?2,revision=?3,claim_token=NULL,lease_until=NULL,updated_at=?4 WHERE id=?1",params![job.id,if job.cancel_requested && !job.mandatory {"cancelled"} else {"interrupted"},rev,Utc::now()])?;
            change(&tx, "job", &job.id, &rev, "service")?;
        }
        tx.commit()?;
        Ok(jobs.len())
    }
}

fn validate_error_code(code: &str) -> DataResult<()> {
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err(DataError::Invalid("error code"));
    }
    Ok(())
}

/// Disable reads immediately in the same transaction as a content mutation.
pub(super) fn invalidate_publications_on(
    tx: &Transaction<'_>,
    kind: &str,
    id: &str,
) -> DataResult<()> {
    tx.execute("UPDATE shares SET enabled=0 WHERE token IN (SELECT share_token FROM service_publication_dependencies WHERE source_kind=?1 AND entity_id=?2)",params![kind,id])?;
    if kind == "meeting" {
        tx.execute("UPDATE shares SET enabled=0 WHERE meeting_id=?1", [id])?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct PurgePayload {
    paths: Vec<String>,
}

pub(super) fn queue_deletion_on(
    tx: &Transaction<'_>,
    meeting_id: &str,
    scope: &str,
    generation: &str,
    actor: &str,
) -> DataResult<String> {
    safe_id(meeting_id)?;
    if !matches!(scope, "meeting" | "transcript" | "summary" | "note") {
        return Err(DataError::Invalid("deletion scope"));
    }
    let mut paths = Vec::new();
    if scope == "meeting" {
        // IDs issued by Bazza for actual recordings are UUIDs. Never treat an
        // arbitrary legacy/import ID as a filesystem path.
        if uuid::Uuid::parse_str(meeting_id).is_err() {
            return Err(DataError::Invalid("media meeting identifier"));
        }
        paths.push(format!("recordings/{meeting_id}"));
        let tokens = {
            let mut st = tx.prepare("SELECT token FROM shares WHERE meeting_id=?1")?;
            let rows = st
                .query_map([meeting_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for token in tokens {
            if safe_id(&token).is_ok() {
                for ext in ["mp3", "mp4"] {
                    paths.push(format!("clips/{token}.{ext}"));
                }
            }
        }
    }
    // Immutable publication candidates are uniquely named, so a retry can never
    // delete a new generation that appeared after this transaction.
    let stages = {
        let mut st=tx.prepare("SELECT id,relative_path FROM service_staging WHERE meeting_id=?1 AND (?2='meeting' OR target_kind=?2) AND state IN ('prepared','ready','published')")?;
        let rows = st
            .query_map(params![meeting_id, scope], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, path) in stages {
        if safe_stage_path(&path).is_ok() && uuid::Uuid::parse_str(&id).is_ok() {
            // A crash may have renamed a ready candidate before committing its
            // published path. Both UUID-owned locations belong to this stage.
            paths.push(format!(".bazza-service-staging/{id}"));
            paths.push(format!(".bazza-service-publications/{id}"));
        }
        tx.execute(
            "UPDATE service_staging SET state='discarded' WHERE id=?1",
            [id],
        )?;
    }
    let payload = serde_json::to_string(&PurgePayload { paths })?;
    let job = insert_job(
        tx,
        if scope == "meeting" {
            "meeting_deletion"
        } else {
            "content_deletion"
        },
        Some(meeting_id),
        "service",
        true,
        actor,
        &payload,
    )?;
    let created = job_on(tx, &job)?;
    change(tx, "job", &job, &created.revision, actor)?;
    tx.execute("INSERT INTO service_deletions(id,meeting_id,scope,generation,job_id,state,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6)",params![revision(),meeting_id,scope,generation,job,Utc::now()])?;
    Ok(job)
}

impl Store {
    pub fn service_delete_meeting(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected_lifecycle_revision: &str,
    ) -> DataResult<MutationOutcome> {
        mutate(
            self,
            ctx,
            "delete_meeting",
            &(id, expected_lifecycle_revision),
            |tx| {
                live_meeting(tx, id)?;
                let old: String = tx.query_row(
                    "SELECT lifecycle_revision FROM service_meeting_state WHERE meeting_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                check_revision(&old, expected_lifecycle_revision)?;
                super::postprocess_checkpoint::invalidate_on(tx, id)?;
                let rev = revision();
                tx.execute("UPDATE service_meeting_state SET tombstoned=1,lifecycle_revision=?2 WHERE meeting_id=?1",params![id,rev])?;
                invalidate_publications_on(tx, "meeting", id)?;
                let job = queue_deletion_on(tx, id, "meeting", &rev, &ctx.principal)?;
                tx.execute(
                    "DELETE FROM service_search_documents WHERE meeting_id=?1",
                    [id],
                )?;
                tx.execute(
                    "UPDATE service_search_state SET generation=generation+1 WHERE singleton=1",
                    [],
                )?;
                tx.execute("DELETE FROM service_notes WHERE meeting_id=?1", [id])?;
                tx.execute("DELETE FROM segments WHERE meeting_id=?1", [id])?;
                tx.execute("DELETE FROM chunks WHERE meeting_id=?1", [id])?;
                tx.execute(
                    "DELETE FROM service_transcript_sources WHERE meeting_id=?1",
                    [id],
                )?;
                tx.execute("DELETE FROM summaries WHERE meeting_id=?1", [id])?;
                tx.execute(
                    "DELETE FROM service_summary_history WHERE meeting_id=?1",
                    [id],
                )?;
                tx.execute(
                    "DELETE FROM settings WHERE key IN (?1,?2,?3)",
                    params![
                        format!("stt:{id}"),
                        format!("stt_previous:{id}"),
                        format!("highlights:{id}")
                    ],
                )?;
                crate::settings::clear_meeting_voice_queries_on(tx, id)?;
                crate::transcript_versions::delete_versions_on(tx, id)?;
                super::chats::erase_meeting_references_on(tx, id)?;
                let mut out = outcome(tx, "meeting_lifecycle", id, &rev, ctx)?;
                out.job_id = Some(job);
                Ok(out)
            },
        )
    }

    /// Select one queued mandatory meeting purge without reading any paths.
    /// The monitor advances the cursor even if this job fails to run.
    pub fn service_next_pending_meeting_purge(
        &self,
        after: Option<&str>,
    ) -> DataResult<Option<String>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT j.id FROM service_jobs j
                 WHERE j.id>?1 AND length(CAST(j.id AS BLOB))=36
                   AND j.kind='meeting_deletion' AND j.executor='service'
                   AND j.state='queued' AND j.mandatory=1 AND j.cancel_requested=0
                   AND (j.error_code IS NULL
                        OR (j.error_code='PURGE_RETRY' AND j.updated_at<=?2))
                   AND EXISTS (SELECT 1 FROM service_deletions d
                               WHERE d.job_id=j.id AND d.meeting_id=j.entity_id
                                 AND d.scope='meeting' AND d.state='pending')
                 ORDER BY j.id LIMIT 1",
                params![after.unwrap_or_default(), Utc::now() - Duration::seconds(2)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Retryable file cleanup. No database mutex is held during filesystem I/O.
    /// The persistent OS lock serializes duplicate executors across processes.
    pub fn service_run_purge(&self, job_id: &str) -> DataResult<bool> {
        uuid::Uuid::parse_str(job_id).map_err(|_| DataError::Invalid("job identifier"))?;
        let root = self.root().canonicalize()?;
        let locks = private_directory(&root, ".bazza-service-purge")?;
        let lock_path = locks.join(format!("{job_id}.lock"));
        reject_link_if_present(&lock_path)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        file.try_lock().map_err(|_| DataError::Busy)?;
        let (meeting, scope, generation, payload, _state) = {
            let conn = self.conn.lock();
            let row:(String,String,String,String,String)=conn.query_row("SELECT d.meeting_id,d.scope,d.generation,j.payload_json,d.state FROM service_deletions d JOIN service_jobs j ON j.id=d.job_id WHERE d.job_id=?1",[job_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(DataError::NotFound)?;
            if row.4 == "complete" {
                return Ok(true);
            }
            if row.1 == "meeting" {
                check_tombstone(&conn, &row.0, &row.2)?;
            }
            row
        };
        let paths: PurgePayload = serde_json::from_str(&payload)?;
        // A whole-meeting purge must exclude every cooperating media reader
        // before removing even the first recording, clip or staged candidate.
        // The database mutex above has been released before gate I/O. Keep the
        // guard until the physical cleanup and final database commit finish.
        let _media_gate = if scope == "meeting" {
            // UUID path spellings can alias on Windows. Match the typed media
            // reader's canonical UUID gate while retaining the original SQL ID
            // and recorded paths everywhere else in this executor.
            let gate_id = uuid::Uuid::parse_str(&meeting)
                .map_err(|_| DataError::Invalid("media meeting identifier"))?
                .to_string();
            match MediaPurgeGuard::acquire(&root, &gate_id) {
                Ok(guard) => Some(guard),
                Err(error) => {
                    let error = match error {
                        MediaGateError::Busy => DataError::Busy,
                        MediaGateError::Invalid(_) => DataError::Invalid("media purge gate"),
                        MediaGateError::Io(error) => DataError::Io(error),
                    };
                    return retry_purge(self, job_id, error);
                }
            }
        } else {
            None
        };
        for relative in &paths.paths {
            let target = purge_path(&root, relative)?;
            // Serialize cleanup with a worker that was already writing when
            // the tombstone committed. Recheck visibility happens under this
            // same per-stage lock before any subsequent write or rename.
            let _stage_lock = if safe_stage_path(relative).is_ok() {
                let (_, id) = relative.split_once('/').ok_or(DataError::Forbidden)?;
                Some(stage_lock(&root, id)?)
            } else {
                None
            };
            if let Err(error) = remove_checked(&root, &target) {
                return retry_purge(self, job_id, error);
            }
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        if scope == "meeting" {
            check_tombstone(&tx, &meeting, &generation)?;
            tx.execute("DELETE FROM meetings WHERE id=?1", [&meeting])?;
        }
        tx.execute(
            "UPDATE service_deletions SET state='complete' WHERE job_id=?1",
            [job_id],
        )?;
        let rev = revision();
        tx.execute("UPDATE service_jobs SET state='succeeded',revision=?2,error_code=NULL,claim_token=NULL,lease_until=NULL,updated_at=?3 WHERE id=?1",params![job_id,rev,Utc::now()])?;
        change(&tx, "job", job_id, &rev, "service")?;
        tx.commit()?;
        Ok(true)
    }
}

fn retry_purge(store: &Store, job_id: &str, error: DataError) -> DataResult<bool> {
    let mut conn = store.conn.lock();
    let tx = conn.transaction()?;
    let rev = revision();
    tx.execute("UPDATE service_jobs SET state='queued',revision=?2,error_code='PURGE_RETRY',updated_at=?3 WHERE id=?1",params![job_id,rev,Utc::now()])?;
    change(&tx, "job", job_id, &rev, "service")?;
    tx.commit()?;
    match error {
        DataError::Io(_) | DataError::Busy => Ok(false),
        other => Err(other),
    }
}

fn check_tombstone(conn: &Connection, id: &str, generation: &str) -> DataResult<()> {
    let (rev, dead): (String, bool) = conn
        .query_row(
            "SELECT lifecycle_revision,tombstoned FROM service_meeting_state WHERE meeting_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(DataError::NotFound)?;
    check_revision(&rev, generation)?;
    if !dead {
        return Err(DataError::Forbidden);
    }
    Ok(())
}
fn reject_link(path: &Path) -> DataResult<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(DataError::Forbidden);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(DataError::Forbidden);
        }
    }
    Ok(meta)
}
fn reject_link_if_present(path: &Path) -> DataResult<()> {
    match reject_link(path) {
        Ok(_) => Ok(()),
        Err(DataError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
fn private_directory(root: &Path, name: &str) -> DataResult<PathBuf> {
    let path = root.join(name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    if !reject_link(&path)?.is_dir() || !path.canonicalize()?.starts_with(root) {
        return Err(DataError::Forbidden);
    }
    Ok(path)
}
fn stage_lock(root: &Path, id: &str) -> DataResult<fs::File> {
    uuid::Uuid::parse_str(id).map_err(|_| DataError::Invalid("stage identifier"))?;
    let locks = private_directory(root, ".bazza-service-purge")?;
    let path = locks.join(format!("stage-{id}.lock"));
    reject_link_if_present(&path)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock().map_err(|_| DataError::Busy)?;
    Ok(file)
}
fn safe_stage_path(value: &str) -> DataResult<()> {
    let Some((parent, name)) = value.split_once('/') else {
        return Err(DataError::Invalid("stage path"));
    };
    if !matches!(
        parent,
        ".bazza-service-staging" | ".bazza-service-publications"
    ) || uuid::Uuid::parse_str(name).is_err()
    {
        return Err(DataError::Invalid("stage path"));
    }
    Ok(())
}
fn purge_path(root: &Path, relative: &str) -> DataResult<PathBuf> {
    if safe_stage_path(relative).is_err() {
        let Some((parent, name)) = relative.split_once('/') else {
            return Err(DataError::Invalid("purge path"));
        };
        match parent {
            "recordings" => {
                uuid::Uuid::parse_str(name).map_err(|_| DataError::Invalid("recording path"))?;
            }
            "clips" => {
                let Some((token, ext)) = name.rsplit_once('.') else {
                    return Err(DataError::Invalid("clip path"));
                };
                safe_id(token)?;
                if !matches!(ext, "mp3" | "mp4") {
                    return Err(DataError::Invalid("clip extension"));
                }
            }
            _ => return Err(DataError::Invalid("purge path")),
        }
    }
    let target = root.join(relative);
    if !target.starts_with(root) {
        return Err(DataError::Forbidden);
    }
    Ok(target)
}
fn remove_checked(root: &Path, path: &Path) -> DataResult<()> {
    if !path.starts_with(root) || path == root {
        return Err(DataError::Forbidden);
    }
    // Validate every ancestor as well as descendants. remove_dir_all is never
    // invoked on a computed, unchecked target or a reparse/symlink tree.
    if let Some(parent) = path.parent() {
        if parent != root {
            match reject_link(parent) {
                Err(DataError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                value => {
                    value?;
                }
            }
        }
    }
    let meta = match reject_link(path) {
        Err(DataError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        v => v?,
    };
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_checked(root, &entry?.path())?;
        }
        fs::remove_dir(path)?;
    } else if meta.is_file() {
        fs::remove_file(path)?;
    } else {
        return Err(DataError::Forbidden);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StagingTarget {
    Meeting,
    Transcript,
    Summary,
}
impl StagingTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Meeting => "meeting",
            Self::Transcript => "transcript",
            Self::Summary => "summary",
        }
    }
}
#[derive(Clone, Debug)]
pub struct StagingRecord {
    pub id: String,
    pub job_id: String,
    pub meeting_id: String,
    pub target_kind: String,
    pub target_revision: String,
    pub relative_path: String,
    pub state: String,
}
fn stage_on(conn: &Connection, id: &str) -> DataResult<StagingRecord> {
    conn.query_row("SELECT id,job_id,meeting_id,target_kind,target_revision,relative_path,state FROM service_staging WHERE id=?1",[id],|r|Ok(StagingRecord{id:r.get(0)?,job_id:r.get(1)?,meeting_id:r.get(2)?,target_kind:r.get(3)?,target_revision:r.get(4)?,relative_path:r.get(5)?,state:r.get(6)?})).optional()?.ok_or(DataError::NotFound)
}
fn check_stage_target(conn: &Connection, stage: &StagingRecord) -> DataResult<()> {
    live_meeting(conn, &stage.meeting_id)?;
    let sql = match stage.target_kind.as_str() {
        "meeting" => "SELECT lifecycle_revision FROM service_meeting_state WHERE meeting_id=?1",
        "transcript" => "SELECT revision FROM service_transcript_state WHERE meeting_id=?1",
        "summary" => "SELECT revision FROM service_summary_state WHERE meeting_id=?1",
        _ => return Err(DataError::Invalid("stage target")),
    };
    let rev: String = conn
        .query_row(sql, [&stage.meeting_id], |r| r.get(0))
        .optional()?
        .ok_or(DataError::NotFound)?;
    check_revision(&rev, &stage.target_revision)
}

impl Store {
    /// The stage path is generated by the service. Workers never nominate a
    /// destination or publish directly into a recording/media directory.
    pub fn service_prepare_staging(
        &self,
        ctx: &MutationContext,
        job_id: &str,
        claim_token: &str,
        meeting_id: &str,
        target: StagingTarget,
        target_revision: &str,
    ) -> DataResult<MutationOutcome> {
        mutate(
            self,
            ctx,
            "prepare_staging",
            &(job_id, meeting_id, target, target_revision),
            |tx| {
                let job = check_claim(tx, job_id, claim_token, None)?;
                if job.cancel_requested {
                    return Err(DataError::Forbidden);
                }
                if job.entity_id.as_deref() != Some(meeting_id) {
                    return Err(DataError::Forbidden);
                }
                let id = revision();
                let path = format!(".bazza-service-staging/{id}");
                let stage = StagingRecord {
                    id: id.clone(),
                    job_id: job_id.into(),
                    meeting_id: meeting_id.into(),
                    target_kind: target.name().into(),
                    target_revision: target_revision.into(),
                    relative_path: path.clone(),
                    state: "prepared".into(),
                };
                check_stage_target(tx, &stage)?;
                tx.execute("INSERT INTO service_staging(id,job_id,meeting_id,target_kind,target_revision,relative_path,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7)",params![id,job_id,meeting_id,target.name(),target_revision,path,Utc::now()])?;
                outcome(tx, "staging", &id, target_revision, ctx)
            },
        )
    }
    pub fn service_staging(&self, id: &str) -> DataResult<StagingRecord> {
        stage_on(&self.conn.lock(), id)
    }

    /// Bounded write into a uniquely named candidate. It is not readable by a
    /// publication consumer until service_commit_staging succeeds.
    pub fn service_write_staging(
        &self,
        id: &str,
        claim_token: &str,
        bytes: &[u8],
    ) -> DataResult<()> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(DataError::Invalid("stage byte budget"));
        }
        let root = self.root().canonicalize()?;
        let _stage_lock = stage_lock(&root, id)?;
        let stage = {
            let conn = self.conn.lock();
            let stage = stage_on(&conn, id)?;
            let job = check_claim(&conn, &stage.job_id, claim_token, None)?;
            if job.cancel_requested || stage.state != "prepared" {
                return Err(DataError::Forbidden);
            }
            check_stage_target(&conn, &stage)?;
            stage
        };
        safe_stage_path(&stage.relative_path)?;
        private_directory(&root, ".bazza-service-staging")?;
        let path = root.join(&stage.relative_path);
        reject_link_if_present(&path)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = reject_link(&path)?;
                if !meta.is_file() {
                    return Err(DataError::Forbidden);
                }
                if meta.len() != bytes.len() as u64 || fs::read(&path)? != bytes {
                    // A process/write failure can leave a partial prepared
                    // candidate. This UUID path is unpublished and exclusively
                    // leased; replace its directory entry rather than truncate
                    // an inode that might have another hard link.
                    fs::remove_file(&path)?;
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    file.write_all(bytes)?;
                    file.sync_all()?;
                }
            }
            Err(e) => return Err(e.into()),
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let current = stage_on(&tx, id)?;
        let job = check_claim(&tx, &current.job_id, claim_token, None)?;
        if job.cancel_requested || current.state != "prepared" {
            return Err(DataError::Forbidden);
        }
        check_stage_target(&tx, &current)?;
        tx.execute("UPDATE service_staging SET state='ready' WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Rename is outside the database mutex. Only the final transactional check
    /// exposes the immutable artifact; a stale candidate remains inaccessible.
    /// Retry after a crash between rename and commit recognizes the same UUID.
    pub fn service_commit_staging(&self, id: &str, claim_token: &str) -> DataResult<StagingRecord> {
        uuid::Uuid::parse_str(id).map_err(|_| DataError::Invalid("stage identifier"))?;
        let root = self.root().canonicalize()?;
        let _stage_lock = stage_lock(&root, id)?;
        let stage = {
            let conn = self.conn.lock();
            let stage = stage_on(&conn, id)?;
            let job = check_claim(&conn, &stage.job_id, claim_token, None)?;
            if job.cancel_requested {
                return Err(DataError::Forbidden);
            }
            check_stage_target(&conn, &stage)?;
            if !matches!(stage.state.as_str(), "ready" | "published") {
                return Err(DataError::Forbidden);
            }
            stage
        };
        private_directory(&root, ".bazza-service-publications")?;
        let published = format!(".bazza-service-publications/{id}");
        let destination = root.join(&published);
        if stage.state != "published" {
            safe_stage_path(&stage.relative_path)?;
            // Validate the fixed source directory, including recovery after a
            // rename. A regular leaf beneath a junction is still outside our
            // ownership and must never be moved into a publication.
            private_directory(&root, ".bazza-service-staging")?;
            let source = root.join(&stage.relative_path);
            if source.try_exists()? {
                reject_link(&source)?;
                if destination.try_exists()? {
                    return Err(DataError::Busy);
                }
                fs::rename(&source, &destination)?;
            } else if !reject_link(&destination)?.is_file() {
                return Err(DataError::Forbidden);
            }
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let current = stage_on(&tx, id)?;
        let job = check_claim(&tx, &current.job_id, claim_token, None)?;
        if job.cancel_requested || !matches!(current.state.as_str(), "ready" | "published") {
            return Err(DataError::Forbidden);
        }
        check_stage_target(&tx, &current)?;
        tx.execute(
            "UPDATE service_staging SET state='published',relative_path=?2 WHERE id=?1",
            params![id, published],
        )?;
        let final_stage = stage_on(&tx, id)?;
        tx.commit()?;
        Ok(final_stage)
    }

    /// Consumers must resolve through this check, never by guessing the private
    /// publication directory. Invalidated/old-generation files are not visible.
    pub fn service_publication(&self, id: &str) -> DataResult<StagingRecord> {
        let conn = self.conn.lock();
        let stage = stage_on(&conn, id)?;
        if stage.state != "published" {
            return Err(DataError::NotFound);
        }
        check_stage_target(&conn, &stage)?;
        Ok(stage)
    }

    /// Service-owned mandatory cleanup, including candidates left by a crashed
    /// rename. It first removes visibility, then retries physical cleanup.
    pub fn service_discard_staging(&self, id: &str) -> DataResult<bool> {
        uuid::Uuid::parse_str(id).map_err(|_| DataError::Invalid("stage identifier"))?;
        let root = self.root().canonicalize()?;
        let locks = private_directory(&root, ".bazza-service-purge")?;
        let lock_path = locks.join(format!("stage-{id}.lock"));
        reject_link_if_present(&lock_path)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock().map_err(|_| DataError::Busy)?;
        {
            let conn = self.conn.lock();
            stage_on(&conn, id)?;
            conn.execute(
                "UPDATE service_staging SET state='discarded' WHERE id=?1",
                [id],
            )?;
        }
        for directory in [".bazza-service-staging", ".bazza-service-publications"] {
            let target = root.join(directory).join(id);
            match remove_checked(&root, &target) {
                Ok(()) => {}
                Err(DataError::Io(_)) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedCalendarEvent {
    pub uid: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarCache {
    pub source_id: String,
    pub label: String,
    pub revision: String,
    pub freshness: String,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub events: Vec<CachedCalendarEvent>,
}

impl Store {
    pub fn service_create_calendar_source(
        &self,
        ctx: &MutationContext,
        label: &str,
    ) -> DataResult<MutationOutcome> {
        bounded_text(label, 1, 256)?;
        mutate(self, ctx, "create_calendar_source", &label, |tx| {
            let id = revision();
            let rev = revision();
            tx.execute(
                "INSERT INTO service_calendar_sources VALUES(?1,?2,?3,'never_synced',NULL,NULL)",
                params![id, label, rev],
            )?;
            outcome(tx, "calendar_source", &id, &rev, ctx)
        })
    }
    /// Parsed events enter as one bounded batch; parsing/network access is outside
    /// the store. A failed refresh preserves the last successful cache.
    pub fn service_replace_calendar_cache(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        events: &[CachedCalendarEvent],
    ) -> DataResult<MutationOutcome> {
        if events.len() > 10_000 {
            return Err(DataError::Invalid("calendar event budget"));
        }
        let mut keys = std::collections::HashSet::new();
        for e in events {
            bounded_text(&e.uid, 1, 512)?;
            bounded_text(&e.title, 0, 512)?;
            if e.end_at < e.start_at || !keys.insert((&e.uid, e.start_at)) {
                return Err(DataError::Invalid("calendar occurrence"));
            }
        }
        mutate(
            self,
            ctx,
            "replace_calendar_cache",
            &(id, expected, events),
            |tx| {
                let old: String = tx
                    .query_row(
                        "SELECT sync_revision FROM service_calendar_sources WHERE id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or(DataError::NotFound)?;
                check_revision(&old, expected)?;
                tx.execute(
                    "DELETE FROM service_calendar_occurrences WHERE source_id=?1",
                    [id],
                )?;
                for e in events {
                    tx.execute(
                        "INSERT INTO service_calendar_occurrences VALUES(?1,?2,?3,?4,?5)",
                        params![id, e.uid, e.start_at, e.end_at, e.title],
                    )?;
                }
                let rev = revision();
                tx.execute("UPDATE service_calendar_sources SET sync_revision=?2,freshness='fresh',last_success_at=?3,last_error_code=NULL WHERE id=?1",params![id,rev,Utc::now()])?;
                outcome(tx, "calendar_source", id, &rev, ctx)
            },
        )
    }
    pub fn service_fail_calendar_refresh(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        error_code: &str,
    ) -> DataResult<MutationOutcome> {
        validate_error_code(error_code)?;
        mutate(
            self,
            ctx,
            "fail_calendar_refresh",
            &(id, expected, error_code),
            |tx| {
                let old: String = tx
                    .query_row(
                        "SELECT sync_revision FROM service_calendar_sources WHERE id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or(DataError::NotFound)?;
                check_revision(&old, expected)?;
                let rev = revision();
                tx.execute("UPDATE service_calendar_sources SET sync_revision=?2,freshness='stale',last_error_code=?3 WHERE id=?1",params![id,rev,error_code])?;
                outcome(tx, "calendar_source", id, &rev, ctx)
            },
        )
    }
    pub fn service_calendar_cache(&self, id: &str) -> DataResult<CalendarCache> {
        let conn = self.conn.lock();
        let mut cache=conn.query_row("SELECT id,label,sync_revision,freshness,last_success_at,last_error_code FROM service_calendar_sources WHERE id=?1",[id],|r|Ok(CalendarCache{source_id:r.get(0)?,label:r.get(1)?,revision:r.get(2)?,freshness:r.get(3)?,last_success_at:r.get(4)?,last_error_code:r.get(5)?,events:Vec::new()})).optional()?.ok_or(DataError::NotFound)?;
        let mut st=conn.prepare("SELECT uid,start_at,end_at,title FROM service_calendar_occurrences WHERE source_id=?1 ORDER BY start_at,uid")?;
        cache.events = st
            .query_map([id], |r| {
                Ok(CachedCalendarEvent {
                    uid: r.get(0)?,
                    start_at: r.get(1)?,
                    end_at: r.get(2)?,
                    title: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx(key: &str) -> MutationContext {
        MutationContext {
            principal: "synthetic-actor".into(),
            idempotency_key: key.into(),
        }
    }
    fn fixture() -> (tempfile::TempDir, Store, String) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        let created = store
            .service_create_text_meeting(
                &ctx("meeting"),
                &NewTextMeeting {
                    title: "Execution fixture".into(),
                    started_at: Utc::now(),
                    participants: Vec::new(),
                    folder_id: None,
                    tag_ids: Vec::new(),
                    calendar_uid: None,
                    transcript: None,
                },
            )
            .unwrap();
        (temp, store, created.entity_id)
    }
    fn enqueue(store: &Store, id: &str, key: &str) -> DurableJob {
        let out = store
            .service_enqueue_job(
                &ctx(key),
                &NewJob {
                    kind: JobKind::SharePreparation,
                    entity_id: Some(id.into()),
                    executor: JobExecutor::Service,
                },
            )
            .unwrap();
        store.service_job(&out.entity_id).unwrap()
    }
    fn expire(store: &Store, id: &str) {
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_jobs SET lease_until=?2 WHERE id=?1",
                params![id, Utc::now() - Duration::seconds(1)],
            )
            .unwrap();
    }

    #[test]
    fn lease_reclaim_fences_old_worker_and_survives_reopen() {
        let (temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let old = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        assert!(matches!(
            store.service_claim_job(&job.id, JobExecutor::Service, &old.job.revision, 30),
            Err(DataError::Busy)
        ));
        expire(&store, &job.id);
        drop(store);
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        assert_eq!(store.service_recover_expired_jobs().unwrap(), 1);
        let waiting = store.service_job(&job.id).unwrap();
        assert_eq!(waiting.state, "interrupted");
        let new = store
            .service_claim_job(&job.id, JobExecutor::Service, &waiting.revision, 30)
            .unwrap();
        assert_ne!(old.token, new.token);
        assert_eq!(new.job.attempts, 2);
        assert!(matches!(
            store.service_finish_job(&job.id, &old.token, &new.job.revision, None),
            Err(DataError::Forbidden)
        ));
        assert_eq!(
            store
                .service_finish_job(&job.id, &new.token, &new.job.revision, None)
                .unwrap()
                .state,
            "succeeded"
        );
    }
    #[test]
    fn running_cancel_is_intent_until_worker_acknowledges() {
        let (_temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let cancelled = store
            .service_cancel_job(&ctx("cancel"), &job.id, &claim.job.revision)
            .unwrap();
        let intent = store.service_job(&job.id).unwrap();
        assert!(intent.cancel_requested);
        assert_eq!(intent.state, "running");
        assert!(matches!(
            store.service_finish_job(&job.id, &claim.token, &claim.job.revision, None),
            Err(DataError::Conflict { .. })
        ));
        assert_eq!(
            store
                .service_finish_job(&job.id, &claim.token, &cancelled.revision, None)
                .unwrap()
                .state,
            "cancelled"
        );
        assert_eq!(
            store
                .service_cancel_job(&ctx("cancel"), &job.id, &claim.job.revision)
                .unwrap(),
            cancelled
        );
    }
    #[test]
    fn lease_renewal_requires_live_claim_and_updates_checkpoint_revision() {
        let (_temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let renewed = store
            .service_renew_job(&job.id, &claim.token, &claim.job.revision, 60)
            .unwrap();
        assert_eq!(renewed.token, claim.token);
        assert_eq!(renewed.job.attempts, 1);
        assert_ne!(renewed.job.revision, claim.job.revision);
        assert!(matches!(
            store.service_renew_job(&job.id, &claim.token, &claim.job.revision, 60),
            Err(DataError::Conflict { .. })
        ));
        expire(&store, &job.id);
        assert!(matches!(
            store.service_renew_job(&job.id, &renewed.token, &renewed.job.revision, 60),
            Err(DataError::Forbidden)
        ));
    }
    #[test]
    fn gui_job_waits_and_wrong_executor_cannot_claim() {
        let (_temp, store, id) = fixture();
        let out = store
            .service_enqueue_job(
                &ctx("gui"),
                &NewJob {
                    kind: JobKind::Processing,
                    entity_id: Some(id),
                    executor: JobExecutor::GuiWorker,
                },
            )
            .unwrap();
        let job = store.service_job(&out.entity_id).unwrap();
        assert_eq!(job.state, "waiting_for_worker");
        assert!(matches!(
            store.service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30),
            Err(DataError::Forbidden)
        ));
        assert_eq!(
            store
                .service_claim_job(&job.id, JobExecutor::GuiWorker, &job.revision, 30)
                .unwrap()
                .job
                .state,
            "running"
        );
    }
    #[test]
    fn enqueue_has_one_change_and_mandatory_capture_cannot_be_cancelled() {
        let (_temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let count: i64 = store
            .conn
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM service_changes WHERE entity_kind='job' AND entity_id=?1",
                [&job.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_jobs SET kind='capture',mandatory=1 WHERE id=?1",
                [&job.id],
            )
            .unwrap();
        assert!(matches!(
            store.service_cancel_job(&ctx("cancel"), &job.id, &job.revision),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30),
            Err(DataError::Forbidden)
        ));
    }
    #[test]
    fn content_delete_revokes_only_explicit_snapshot_dependencies() {
        let (_temp, store, id) = fixture();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            for token in ["summary-snapshot", "transcript-snapshot", "legacy-live"] {
                tx.execute("INSERT INTO shares(token,meeting_id,kind,created_at) VALUES(?1,?2,'meeting',?3)",params![token,id,Utc::now()]).unwrap();
            }
            tx.execute("INSERT INTO service_publication_dependencies VALUES('summary-snapshot','summary',?1,'old-revision')",[&id]).unwrap();
            tx.execute("INSERT INTO service_publication_dependencies VALUES('transcript-snapshot','transcript',?1,'old-revision')",[&id]).unwrap();
            invalidate_publications_on(&tx, "summary", &id).unwrap();
            tx.commit().unwrap();
        }
        let shares = store.list_shares().unwrap();
        assert!(
            !shares
                .iter()
                .find(|s| s.token == "summary-snapshot")
                .unwrap()
                .enabled
        );
        assert!(
            shares
                .iter()
                .find(|s| s.token == "transcript-snapshot")
                .unwrap()
                .enabled
        );
        assert!(
            shares
                .iter()
                .find(|s| s.token == "legacy-live")
                .unwrap()
                .enabled
        );
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            invalidate_publications_on(&tx, "meeting", &id).unwrap();
            tx.commit().unwrap();
        }
        assert!(store.list_shares().unwrap().iter().all(|s| !s.enabled));
    }
    #[test]
    fn tombstone_revokes_immediately_and_purge_is_idempotent() {
        let (temp, store, id) = fixture();
        let removed_keys = [
            format!("stt:{id}"),
            format!("stt_previous:{id}"),
            format!("speaker_embeddings:{id}"),
            format!("speaker_embeddings:old-policy:{id}"),
            format!("speaker_embeddings:future-policy:{id}"),
            format!("highlights:{id}"),
        ];
        let retained_keys = [
            format!("speaker_embeddings:future-policy:other-{id}"),
            "highlights:other-meeting".into(),
            "private:provider_key".into(),
        ];
        for key in removed_keys.iter().chain(&retained_keys) {
            store.set_setting(key, &"synthetic stored value").unwrap();
        }
        let speaker = crate::Speaker {
            id: "saved-voice".into(),
            name: "Saved voice".into(),
            embedding: vec![1.0, 0.0],
        };
        store.upsert_speaker(&speaker).unwrap();
        let profiles = store.list_voice_profiles().unwrap();
        let samples = store.list_voice_samples(&speaker.id).unwrap();
        let dir = store.recordings_dir(&id);
        fs::write(dir.join("mix.wav"), b"synthetic media").unwrap();
        store.conn.lock().execute("INSERT INTO shares(token,meeting_id,kind,created_at) VALUES('synthetic-token',?1,'meeting',?2)",params![id,Utc::now()]).unwrap();
        let live = store.service_meeting(&id).unwrap().unwrap();
        let out = store
            .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
            .unwrap();
        assert!(store.service_meeting(&id).unwrap().is_none());
        for key in &removed_keys {
            assert!(store.get_setting::<String>(key).unwrap().is_none());
        }
        for key in &retained_keys {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("synthetic stored value")
            );
        }
        assert!(!store.list_shares().unwrap()[0].enabled);
        let job = store.service_job(out.job_id.as_deref().unwrap()).unwrap();
        assert!(job.mandatory);
        assert!(matches!(
            store.service_cancel_job(&ctx("cancel"), &job.id, &job.revision),
            Err(DataError::Forbidden)
        ));
        assert!(dir.exists());
        drop(store);
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        assert!(store.service_run_purge(&job.id).unwrap());
        assert!(!dir.exists());
        assert!(store.get_meeting(&id).unwrap().is_none());
        assert_eq!(store.list_voice_profiles().unwrap(), profiles);
        assert_eq!(store.list_voice_samples(&speaker.id).unwrap(), samples);
        assert!(store.service_run_purge(&job.id).unwrap());
        assert_eq!(
            store
                .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
                .unwrap(),
            out
        );
    }

    fn purge_media(store: &Store, root: &Path, id: &str) -> Vec<(PathBuf, Vec<u8>)> {
        let recordings = store.recordings_dir(id);
        fs::create_dir(recordings.join("tracks")).unwrap();
        let clips = root.join("clips");
        fs::create_dir(&clips).unwrap();
        let token = format!("purge-{id}");
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO shares(token,meeting_id,kind,created_at) VALUES(?1,?2,'meeting',?3)",
                params![token, id, Utc::now()],
            )
            .unwrap();
        let mut bytes = vec![
            (
                recordings.join("mix.wav"),
                b"original mixed recording".to_vec(),
            ),
            (
                recordings.join("tracks").join("mic.wav"),
                b"original microphone recording".to_vec(),
            ),
            (clips.join(format!("{token}.mp3")), b"audio clip".to_vec()),
            (clips.join(format!("{token}.mp4")), b"video clip".to_vec()),
        ];
        for (path, contents) in &bytes {
            fs::write(path, contents).unwrap();
        }
        let job = enqueue(store, id, "purge-stage-job");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let live = store.service_meeting(id).unwrap().unwrap();
        let stage = store
            .service_prepare_staging(
                &ctx("purge-stage"),
                &job.id,
                &claim.token,
                id,
                StagingTarget::Meeting,
                &live.lifecycle_revision,
            )
            .unwrap();
        store
            .service_write_staging(&stage.entity_id, &claim.token, b"staged candidate")
            .unwrap();
        let row = store.service_staging(&stage.entity_id).unwrap();
        bytes.push((root.join(row.relative_path), b"staged candidate".to_vec()));
        // Both UUID-owned locations are included in a whole-meeting purge,
        // including a publication left by a crash before its database commit.
        let publications = root.join(".bazza-service-publications");
        fs::create_dir(&publications).unwrap();
        let publication = publications.join(&stage.entity_id);
        fs::write(&publication, b"publication candidate").unwrap();
        bytes.push((publication, b"publication candidate".to_vec()));
        bytes
    }

    fn assert_purge_pending(store: &Store, id: &str, job_id: &str, bytes: &[(PathBuf, Vec<u8>)]) {
        assert!(store.service_meeting(id).unwrap().is_none());
        assert!(store.get_meeting(id).unwrap().is_some());
        for (path, contents) in bytes {
            assert_eq!(&fs::read(path).unwrap(), contents, "{}", path.display());
        }
        let job = store.service_job(job_id).unwrap();
        assert!(job.mandatory);
        assert_eq!(job.state, "queued");
        assert_eq!(job.error_code.as_deref(), Some("PURGE_RETRY"));
        assert_eq!(
            store
                .conn
                .lock()
                .query_row(
                    "SELECT state FROM service_deletions WHERE job_id=?1",
                    [job_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "pending"
        );
    }

    #[test]
    fn whole_meeting_purge_waits_for_last_media_reader_before_removing_any_bytes() {
        use bazza_media_gate::{MediaReadGuard, GATE_DIRECTORY};

        let (temp, store, id) = fixture();
        let bytes = purge_media(&store, temp.path(), &id);
        let reader = MediaReadGuard::acquire(temp.path(), &id).unwrap();
        let clone = reader.try_clone_file().unwrap();
        let gate = fs::read_dir(temp.path().join(GATE_DIRECTORY))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(!gate.starts_with(store.recordings_dir(&id)));
        let live = store.service_meeting(&id).unwrap().unwrap();
        let deletion = store
            .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
            .unwrap();
        let job_id = deletion.job_id.unwrap();
        assert!(!store.service_run_purge(&job_id).unwrap());
        assert_purge_pending(&store, &id, &job_id, &bytes);
        drop(reader);
        assert!(!store.service_run_purge(&job_id).unwrap());
        assert_purge_pending(&store, &id, &job_id, &bytes);
        drop(clone);
        assert!(store.service_run_purge(&job_id).unwrap());
        for (path, _) in &bytes {
            assert!(!path.exists(), "{}", path.display());
        }
        assert!(store.get_meeting(&id).unwrap().is_none());
        let job = store.service_job(&job_id).unwrap();
        assert_eq!(job.state, "succeeded");
        assert!(job.error_code.is_none());
        assert!(gate.is_file());
        assert_eq!(fs::metadata(gate).unwrap().len(), 0);
        assert!(store.service_run_purge(&job_id).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn uppercase_legacy_uuid_purge_respects_canonical_media_reader_gate() {
        use bazza_media_gate::MediaReadGuard;

        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        let mut meeting = crate::Meeting::new_recording("Uppercase legacy recording", None);
        meeting.id = "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE".into();
        meeting.status = crate::MeetingStatus::Ready;
        store.create_meeting(&meeting).unwrap();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            super::super::meetings::initialize_meeting_on(&tx, &meeting.id, "recorded").unwrap();
            tx.commit().unwrap();
        }
        let canonical_id = uuid::Uuid::parse_str(&meeting.id).unwrap().to_string();
        assert_ne!(meeting.id, canonical_id);
        let bytes = purge_media(&store, temp.path(), &meeting.id);
        // Establish a real filesystem alias, rather than just comparing keys.
        let lowercase_audio = temp
            .path()
            .join("recordings")
            .join(&canonical_id)
            .join("mix.wav");
        assert_eq!(
            fs::read(&lowercase_audio).unwrap(),
            b"original mixed recording"
        );
        let reader = MediaReadGuard::acquire(temp.path(), &canonical_id).unwrap();
        let live = store.service_meeting(&meeting.id).unwrap().unwrap();
        let deletion = store
            .service_delete_meeting(&ctx("delete"), &meeting.id, &live.lifecycle_revision)
            .unwrap();
        let job_id = deletion.job_id.unwrap();
        assert!(!store.service_run_purge(&job_id).unwrap());
        assert_purge_pending(&store, &meeting.id, &job_id, &bytes);
        drop(reader);
        assert!(store.service_run_purge(&job_id).unwrap());
        for (path, _) in &bytes {
            assert!(!path.exists(), "{}", path.display());
        }
        assert!(!lowercase_audio.exists());
        assert!(store.get_meeting(&meeting.id).unwrap().is_none());
    }

    #[test]
    fn malformed_media_gate_preserves_all_purge_bytes_until_explicit_fixture_repair() {
        use bazza_media_gate::{MediaReadGuard, GATE_DIRECTORY};

        let (temp, store, id) = fixture();
        let bytes = purge_media(&store, temp.path(), &id);
        drop(MediaReadGuard::acquire(temp.path(), &id).unwrap());
        let gate = fs::read_dir(temp.path().join(GATE_DIRECTORY))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(&gate, b"malformed gate retained verbatim").unwrap();
        let live = store.service_meeting(&id).unwrap().unwrap();
        let deletion = store
            .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
            .unwrap();
        let job_id = deletion.job_id.unwrap();
        assert!(matches!(
            store.service_run_purge(&job_id),
            Err(DataError::Invalid("media purge gate"))
        ));
        assert_purge_pending(&store, &id, &job_id, &bytes);
        assert_eq!(
            fs::read(&gate).unwrap(),
            b"malformed gate retained verbatim"
        );
        // Repair is an explicit test-fixture action, never purge behavior.
        fs::write(&gate, []).unwrap();
        assert!(store.service_run_purge(&job_id).unwrap());
        for (path, _) in &bytes {
            assert!(!path.exists(), "{}", path.display());
        }
        assert!(gate.is_file());
        assert_eq!(fs::metadata(gate).unwrap().len(), 0);
    }

    #[test]
    fn late_meeting_delete_failure_restores_scoped_settings_and_visibility() {
        let (_temp, store, id) = fixture();
        let keys = [
            format!("stt:{id}"),
            format!("stt_previous:{id}"),
            format!("speaker_embeddings:{id}"),
            format!("speaker_embeddings:policy:{id}"),
            format!("highlights:{id}"),
        ];
        for key in &keys {
            store.set_setting(key, &"rollback observation").unwrap();
        }
        let before = store.service_meeting(&id).unwrap().unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER fail_meeting_delete_change BEFORE INSERT ON service_changes WHEN new.entity_kind='meeting_lifecycle' BEGIN SELECT RAISE(ABORT,'injected deletion failure'); END;").unwrap();
        assert!(matches!(
            store.service_delete_meeting(&ctx("failing-delete"), &id, &before.lifecycle_revision),
            Err(DataError::Sqlite(_))
        ));
        assert_eq!(
            serde_json::to_value(store.service_meeting(&id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(before).unwrap()
        );
        for key in &keys {
            assert_eq!(
                store.get_setting::<String>(key).unwrap().as_deref(),
                Some("rollback observation")
            );
        }
        let conn = store.conn.lock();
        for sql in [
            "SELECT COUNT(*) FROM service_deletions",
            "SELECT COUNT(*) FROM service_jobs",
            "SELECT COUNT(*) FROM service_idempotency WHERE key='failing-delete'",
        ] {
            assert_eq!(
                conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap(),
                0
            );
        }
    }

    #[test]
    fn active_capture_delete_and_stale_generation_cannot_remove_media() {
        let (_temp, store, id) = fixture();
        let dir = store.recordings_dir(&id);
        fs::write(dir.join("mix.wav"), b"keep").unwrap();
        let live = store.service_meeting(&id).unwrap().unwrap();
        store
            .conn
            .lock()
            .execute("UPDATE meetings SET status='recording' WHERE id=?1", [&id])
            .unwrap();
        assert!(matches!(
            store.service_delete_meeting(&ctx("blocked"), &id, &live.lifecycle_revision),
            Err(DataError::Busy)
        ));
        store
            .conn
            .lock()
            .execute("UPDATE meetings SET status='ready' WHERE id=?1", [&id])
            .unwrap();
        let out = store
            .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
            .unwrap();
        store.conn.lock().execute("UPDATE service_meeting_state SET lifecycle_revision='different-generation' WHERE meeting_id=?1",[&id]).unwrap();
        assert!(matches!(
            store.service_run_purge(out.job_id.as_deref().unwrap()),
            Err(DataError::Conflict { .. })
        ));
        assert_eq!(fs::read(dir.join("mix.wav")).unwrap(), b"keep");
    }
    #[cfg(windows)]
    #[test]
    fn locked_media_retains_tombstone_and_retries_after_handle_closes() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, store, id) = fixture();
        let dir = store.recordings_dir(&id);
        let audio = dir.join("mix.wav");
        fs::write(&audio, b"locked synthetic").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&audio)
            .unwrap();
        let live = store.service_meeting(&id).unwrap().unwrap();
        let out = store
            .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
            .unwrap();
        let job = out.job_id.unwrap();
        assert!(!store.service_run_purge(&job).unwrap());
        assert_eq!(
            store.service_job(&job).unwrap().error_code.as_deref(),
            Some("PURGE_RETRY")
        );
        assert!(store.service_meeting(&id).unwrap().is_none());
        drop(held);
        assert!(store.service_run_purge(&job).unwrap());
        assert!(!audio.exists());
    }
    #[test]
    fn path_validation_rejects_traversal_and_foreign_artifacts() {
        let root = Path::new("C:/synthetic");
        for path in [
            "../foreign",
            "recordings/../../foreign",
            "recordings/not-a-uuid",
            "clips/token.exe",
            ".bazza-service-publications/../../foreign",
            "other/00000000-0000-4000-8000-000000000001",
        ] {
            assert!(purge_path(root, path).is_err(), "{path}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn purge_refuses_symlink_tree_without_touching_external_bytes() {
        use std::os::unix::fs::symlink;
        let (_temp, store, id) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"outside").unwrap();
        let dir = store.recordings_dir(&id);
        symlink(outside.path(), dir.join("link")).unwrap();
        let live = store.service_meeting(&id).unwrap().unwrap();
        let out = store
            .service_delete_meeting(&ctx("delete"), &id, &live.lifecycle_revision)
            .unwrap();
        assert!(matches!(
            store.service_run_purge(out.job_id.as_deref().unwrap()),
            Err(DataError::Forbidden)
        ));
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
        fs::remove_file(dir.join("link")).unwrap();
        assert!(store
            .service_run_purge(out.job_id.as_deref().unwrap())
            .unwrap());
    }
    #[test]
    fn staging_is_invisible_until_fenced_commit_and_recovers_after_rename() {
        let (temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let rev = store
            .service_meeting(&id)
            .unwrap()
            .unwrap()
            .lifecycle_revision;
        let stage = store
            .service_prepare_staging(
                &ctx("stage"),
                &job.id,
                &claim.token,
                &id,
                StagingTarget::Meeting,
                &rev,
            )
            .unwrap();
        assert!(store.service_publication(&stage.entity_id).is_err());
        store
            .service_write_staging(&stage.entity_id, &claim.token, b"immutable candidate")
            .unwrap();
        // Process interruption after atomic rename, before DB visibility commit.
        fs::create_dir(temp.path().join(".bazza-service-publications")).unwrap();
        let row = store.service_staging(&stage.entity_id).unwrap();
        fs::rename(
            temp.path().join(row.relative_path),
            temp.path()
                .join(".bazza-service-publications")
                .join(&stage.entity_id),
        )
        .unwrap();
        let published = store
            .service_commit_staging(&stage.entity_id, &claim.token)
            .unwrap();
        assert_eq!(published.state, "published");
        assert_eq!(
            fs::read(temp.path().join(&published.relative_path)).unwrap(),
            b"immutable candidate"
        );
        assert!(store.service_publication(&stage.entity_id).is_ok());
        assert!(store.service_discard_staging(&stage.entity_id).unwrap());
        assert!(store.service_publication(&stage.entity_id).is_err());
        assert!(!temp.path().join(published.relative_path).exists());
    }
    #[test]
    fn old_job_claim_and_changed_generation_cannot_publish() {
        let (_temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let rev = store
            .service_meeting(&id)
            .unwrap()
            .unwrap()
            .lifecycle_revision;
        let stage = store
            .service_prepare_staging(
                &ctx("stage"),
                &job.id,
                &claim.token,
                &id,
                StagingTarget::Meeting,
                &rev,
            )
            .unwrap();
        store
            .service_write_staging(&stage.entity_id, &claim.token, b"candidate")
            .unwrap();
        expire(&store, &job.id);
        store.service_recover_expired_jobs().unwrap();
        let waiting = store.service_job(&job.id).unwrap();
        let current = store
            .service_claim_job(&job.id, JobExecutor::Service, &waiting.revision, 30)
            .unwrap();
        assert!(matches!(
            store.service_commit_staging(&stage.entity_id, &claim.token),
            Err(DataError::Forbidden)
        ));
        store.conn.lock().execute("UPDATE service_meeting_state SET lifecycle_revision='new-generation' WHERE meeting_id=?1",[&id]).unwrap();
        assert!(matches!(
            store.service_commit_staging(&stage.entity_id, &current.token),
            Err(DataError::Conflict { .. })
        ));
        assert!(store.service_discard_staging(&stage.entity_id).unwrap());
    }
    #[test]
    fn partial_prepared_candidate_retries_full_write_without_published_overwrite() {
        let (temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let rev = store
            .service_meeting(&id)
            .unwrap()
            .unwrap()
            .lifecycle_revision;
        let stage = store
            .service_prepare_staging(
                &ctx("partial-stage"),
                &job.id,
                &claim.token,
                &id,
                StagingTarget::Meeting,
                &rev,
            )
            .unwrap();
        let row = store.service_staging(&stage.entity_id).unwrap();
        fs::create_dir(temp.path().join(".bazza-service-staging")).unwrap();
        let candidate = temp.path().join(&row.relative_path);
        // Deterministic interruption between file creation and completed write.
        fs::write(&candidate, b"sho").unwrap();
        assert_eq!(row.state, "prepared");
        let expected = b"complete immutable candidate";
        store
            .service_write_staging(&stage.entity_id, &claim.token, expected)
            .unwrap();
        assert_eq!(
            store.service_staging(&stage.entity_id).unwrap().state,
            "ready"
        );
        assert_eq!(fs::read(&candidate).unwrap(), expected);
        let published = store
            .service_commit_staging(&stage.entity_id, &claim.token)
            .unwrap();
        assert_eq!(
            fs::read(temp.path().join(&published.relative_path)).unwrap(),
            expected
        );
        assert!(matches!(
            store.service_write_staging(&stage.entity_id, &claim.token, b"overwrite published"),
            Err(DataError::Forbidden)
        ));
        assert_eq!(
            fs::read(temp.path().join(published.relative_path)).unwrap(),
            expected
        );
    }
    #[cfg(windows)]
    #[test]
    fn staging_commit_refuses_existing_parent_junction_and_preserves_external_file() {
        use std::os::windows::process::CommandExt;
        let (temp, store, id) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let rev = store
            .service_meeting(&id)
            .unwrap()
            .unwrap()
            .lifecycle_revision;
        let stage = store
            .service_prepare_staging(
                &ctx("junction-stage"),
                &job.id,
                &claim.token,
                &id,
                StagingTarget::Meeting,
                &rev,
            )
            .unwrap();
        let external = outside.path().join(&stage.entity_id);
        fs::write(&external, b"external bytes stay put").unwrap();
        let junction = temp.path().join(".bazza-service-staging");
        let output=std::process::Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:BAZZA_TEST_JUNCTION_PATH -Target $env:BAZZA_TEST_JUNCTION_TARGET | Out-Null"])
            .env("BAZZA_TEST_JUNCTION_PATH",&junction).env("BAZZA_TEST_JUNCTION_TARGET",outside.path()).creation_flags(0x08000000).output().unwrap();
        assert!(
            output.status.success(),
            "Windows junction fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_staging SET state='ready' WHERE id=?1",
                [&stage.entity_id],
            )
            .unwrap();
        assert!(matches!(
            store.service_commit_staging(&stage.entity_id, &claim.token),
            Err(DataError::Forbidden)
        ));
        assert_eq!(fs::read(&external).unwrap(), b"external bytes stay put");
        assert!(!temp
            .path()
            .join(".bazza-service-publications")
            .join(&stage.entity_id)
            .exists());
        // Removing a junction itself never recursively touches its target.
        fs::remove_dir(&junction).unwrap();
    }
    #[test]
    fn content_purge_scopes_stages_and_meeting_purge_includes_prepared_candidates() {
        let (temp, store, id) = fixture();
        let job = enqueue(&store, &id, "enqueue");
        let claim = store
            .service_claim_job(&job.id, JobExecutor::Service, &job.revision, 30)
            .unwrap();
        let meeting = store.service_meeting(&id).unwrap().unwrap();
        let summary_rev: String = store
            .conn
            .lock()
            .query_row(
                "SELECT revision FROM service_summary_state WHERE meeting_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        let transcript_rev: String = store
            .conn
            .lock()
            .query_row(
                "SELECT revision FROM service_transcript_state WHERE meeting_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        let summary = store
            .service_prepare_staging(
                &ctx("summary-stage"),
                &job.id,
                &claim.token,
                &id,
                StagingTarget::Summary,
                &summary_rev,
            )
            .unwrap();
        let transcript = store
            .service_prepare_staging(
                &ctx("transcript-stage"),
                &job.id,
                &claim.token,
                &id,
                StagingTarget::Transcript,
                &transcript_rev,
            )
            .unwrap();
        // Simulate crash after file creation but before prepared -> ready.
        fs::create_dir(temp.path().join(".bazza-service-staging")).unwrap();
        fs::write(
            temp.path()
                .join(".bazza-service-staging")
                .join(&summary.entity_id),
            b"old summary candidate",
        )
        .unwrap();
        let purge = {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            let purge = queue_deletion_on(&tx, &id, "summary", &summary_rev, "service").unwrap();
            tx.commit().unwrap();
            purge
        };
        assert_eq!(
            store.service_staging(&summary.entity_id).unwrap().state,
            "discarded"
        );
        assert_eq!(
            store.service_staging(&transcript.entity_id).unwrap().state,
            "prepared"
        );
        let reader = bazza_media_gate::MediaReadGuard::acquire(temp.path(), &id).unwrap();
        assert!(store.service_run_purge(&purge).unwrap());
        assert!(!temp
            .path()
            .join(".bazza-service-staging")
            .join(&summary.entity_id)
            .exists());
        drop(reader);
        store
            .service_write_staging(&transcript.entity_id, &claim.token, b"transcript candidate")
            .unwrap();
        let deletion = store
            .service_delete_meeting(&ctx("delete"), &id, &meeting.lifecycle_revision)
            .unwrap();
        assert!(store
            .service_run_purge(deletion.job_id.as_deref().unwrap())
            .unwrap());
        assert!(!temp
            .path()
            .join(".bazza-service-staging")
            .join(&transcript.entity_id)
            .exists());
        assert!(store
            .service_write_staging(&transcript.entity_id, &claim.token, b"late resurrection")
            .is_err());
    }
    #[test]
    fn calendar_failure_retains_last_success_and_source_updates_are_atomic() {
        let (temp, store, _id) = fixture();
        let source = store
            .service_create_calendar_source(&ctx("source"), "Synthetic calendar")
            .unwrap();
        let start = Utc::now();
        let event = CachedCalendarEvent {
            uid: "repeatable-uid".into(),
            start_at: start,
            end_at: start + Duration::hours(1),
            title: "Cached meeting".into(),
        };
        let synced = store
            .service_replace_calendar_cache(
                &ctx("sync"),
                &source.entity_id,
                &source.revision,
                std::slice::from_ref(&event),
            )
            .unwrap();
        let failed = store
            .service_fail_calendar_refresh(
                &ctx("failed"),
                &source.entity_id,
                &synced.revision,
                "OFFLINE",
            )
            .unwrap();
        let cache = store.service_calendar_cache(&source.entity_id).unwrap();
        assert_eq!(cache.freshness, "stale");
        assert_eq!(cache.events.len(), 1);
        assert_eq!(cache.events[0].title, "Cached meeting");
        assert!(cache.last_success_at.is_some());
        assert!(matches!(
            store.service_replace_calendar_cache(
                &ctx("duplicate"),
                &source.entity_id,
                &failed.revision,
                &[event.clone(), event]
            ),
            Err(DataError::Invalid(_))
        ));
        assert_eq!(
            store
                .service_calendar_cache(&source.entity_id)
                .unwrap()
                .revision,
            failed.revision
        );
        drop(store);
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        assert_eq!(
            store
                .service_calendar_cache(&source.entity_id)
                .unwrap()
                .events
                .len(),
            1
        );
    }
}

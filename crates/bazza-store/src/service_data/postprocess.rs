//! Registered post-process admission and fenced input stage transitions.
use super::*;
use crate::Store;
use bazza_domain::local_asr::LocalAsrOutcome;
use bazza_domain::speaker_result::SpeakerResultOutcome;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const POST_PROCESS_MAX_DATA_BYTES: u32 = 65_024;
pub const POST_PROCESS_MAX_ITEMS: u16 = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessSourceFence {
    pub capture_id: Uuid,
    pub capture_generation: Uuid,
    pub checkpoint_revision: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostProcessStage {
    Captured,
    CapturedLive,
    LocalAsr {
        outcome: LocalAsrOutcome,
    },
    AutomaticCleanup {
        local_asr: Option<LocalAsrOutcome>,
    },
    Speakers {
        local_asr: LocalAsrOutcome,
        outcome: SpeakerResultOutcome,
    },
    SonioxSpeakers {
        outcome: SpeakerResultOutcome,
    },
    SonioxReady {
        speakers: SpeakerResultOutcome,
        cleanup: bool,
        generated_summary_revision: Option<String>,
    },
    Ready {
        local_asr: LocalAsrOutcome,
        speakers: SpeakerResultOutcome,
        cleanup: bool,
        generated_summary_revision: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJob {
    pub job_id: String,
    pub meeting_id: Uuid,
    pub revision: String,
    pub input: PostProcessSourceFence,
    pub provider: CaptureSelection,
    pub stage: PostProcessStage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobCursor {
    pub library_scope: String,
    pub library_epoch: Uuid,
    pub worker_session_id: Uuid,
    pub snapshot_sequence: i64,
    pub eligible_at: DateTime<Utc>,
    pub after_job_id: String,
    pub query_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobsRequest {
    pub proof: WorkerProof,
    pub cursor: Option<PostProcessJobCursor>,
    pub limit: u16,
    pub max_bytes: u32,
}

impl PostProcessJobsRequest {
    pub fn new(proof: WorkerProof) -> Self {
        Self {
            proof,
            cursor: None,
            limit: 20,
            max_bytes: POST_PROCESS_MAX_DATA_BYTES,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobPage {
    pub library_scope: String,
    pub snapshot_sequence: i64,
    pub eligible_at: DateTime<Utc>,
    pub items: Vec<PostProcessJob>,
    pub next: Option<PostProcessJobCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimPostProcessJobRequest {
    pub proof: WorkerProof,
    /// Original descriptor from the bounded list; never refresh on conflict.
    pub job: PostProcessJob,
    pub claim_id: Uuid,
    pub operation_id: Uuid,
    pub fence: WorkerSecret,
    pub ttl_seconds: i64,
}

/// An immutable operation receipt, not a promise that its lease is still live.
/// A replay never refreshes this revision or expiry. Every write reauthenticates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessJobLease {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub input: PostProcessSourceFence,
    pub expires_at: DateTime<Utc>,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

/// Lease authority and the bounded content cursor are checked together on every
/// page. A transcript request alone grants no worker access to another source.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessTranscriptRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub page: DesktopTranscriptRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewPostProcessJobRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub operation_id: Uuid,
    pub fence: WorkerSecret,
    pub ttl_seconds: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PostProcessFailureCode {
    PostProcessFailed,
    PostProcessInputUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailPostProcessJobRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub operation_id: Uuid,
    pub fence: WorkerSecret,
    pub error_code: PostProcessFailureCode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessFailureReceipt {
    pub operation_id: Uuid,
    pub claim_id: Uuid,
    pub worker_session_id: Uuid,
    pub library_epoch: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub job_revision: String,
    pub lifecycle_revision: String,
    pub error_code: PostProcessFailureCode,
    pub change_sequence: i64,
    pub dedup_until: DateTime<Utc>,
}

const MAX_RECEIPT: usize = 4096;
const MAX_OPERATION: usize = 8192;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureJobPayload {
    capture_id: Uuid,
    capture_generation: Uuid,
    transcript_revision: String,
    provider: ClosedSelection,
    required_capabilities: Vec<WorkerCapability>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClosedSelection {
    provider: CaptureProvider,
    model: Option<String>,
    language: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalCaptureReceipt {
    entity_id: String,
    revision: String,
    change_sequence: i64,
    job_id: Option<String>,
    dedup_until: DateTime<Utc>,
}

struct ContextRow {
    meeting_id: String,
    revision: String,
    capture_id: String,
    generation: String,
    provider: Option<String>,
    receipt: Option<String>,
    payload: Option<String>,
    lifecycle: String,
    transcript: String,
    source: Option<String>,
    document_state: String,
    last_sequence: i64,
    tombstoned: bool,
    status: String,
    capture_worker: Option<String>,
    capture_epoch: Option<String>,
}

pub(super) struct CaptureContext {
    pub job_id: String,
    pub job_revision: String,
    pub meeting_id: Uuid,
    pub capture_id: Uuid,
    pub capture_generation: Uuid,
    pub root_lifecycle_revision: String,
    pub root_transcript_revision: String,
    pub capture_fingerprint: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub source_id: Option<String>,
    pub document_state: String,
    pub meeting_status: String,
    pub last_sequence: i64,
    pub provider: CaptureSelection,
}

// Every body is bounded in SQLite before Rust allocation. No raw source,
// caption/provider projection, media path, title, actor or generic job payload
// is returned to a worker. Candidate JSON is parsed into a closed schema.
const CONTEXT_COLUMNS: &str = "
 CASE WHEN length(CAST(j.entity_id AS BLOB))<=128 THEN j.entity_id END,
 CASE WHEN length(CAST(j.revision AS BLOB))<=128 THEN j.revision END,
 CASE WHEN length(CAST(c.id AS BLOB))<=128 THEN c.id END,
 CASE WHEN length(CAST(c.generation AS BLOB))<=128 THEN c.generation END,
 CASE WHEN length(CAST(c.provider_json AS BLOB))<=1024 THEN c.provider_json END,
 CASE WHEN length(CAST(c.outcome_json AS BLOB))<=2048 THEN c.outcome_json END,
 CASE WHEN length(CAST(j.payload_json AS BLOB))<=4096 THEN j.payload_json END,
 CASE WHEN length(CAST(s.lifecycle_revision AS BLOB))<=128 THEN s.lifecycle_revision END,
 CASE WHEN length(CAST(t.revision AS BLOB))<=128 THEN t.revision END,
 CAST(substr(CAST(t.current_source_id AS BLOB),1,129) AS TEXT),
 t.document_state,c.last_confirmed_sequence,s.tombstoned,m.status,
 CASE WHEN length(CAST(c.worker_session_id AS BLOB))<=128 THEN c.worker_session_id END,
 CASE WHEN length(CAST(c.library_epoch AS BLOB))<=128 THEN c.library_epoch END";
const CONTEXT_FROM: &str = "FROM service_jobs j
 JOIN service_capture_leases c ON c.job_id=j.id AND c.meeting_id=j.entity_id
 JOIN meetings m ON m.id=j.entity_id
 JOIN service_meeting_state s ON s.meeting_id=m.id
 JOIN service_transcript_state t ON t.meeting_id=m.id
 WHERE j.kind='processing' AND j.executor='gui_worker' AND j.mandatory=0
 AND c.state='finalized' AND m.status IN ('processing','ready') AND s.tombstoned=0";

fn uid(value: Uuid) -> DataResult<()> {
    if value.is_nil() {
        Err(DataError::Invalid("post-process identifier"))
    } else {
        Ok(())
    }
}
pub(super) fn rev(value: &str) -> DataResult<()> {
    bounded_text(value, 1, 128)?;
    if !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
    {
        return Err(DataError::Invalid("post-process revision"));
    }
    Ok(())
}
fn source_valid(value: &PostProcessSourceFence) -> DataResult<()> {
    uid(value.capture_id)?;
    uid(value.capture_generation)?;
    rev(&value.checkpoint_revision)?;
    rev(&value.lifecycle_revision)?;
    rev(&value.transcript_revision)?;
    if let Some(id) = &value.source_id {
        safe_id(id)?;
    }
    Ok(())
}
fn job_valid(value: &PostProcessJob) -> DataResult<()> {
    safe_id(&value.job_id)?;
    uid(value.meeting_id)?;
    rev(&value.revision)?;
    source_valid(&value.input)?;
    workers::selection(&value.provider)
}
pub(super) fn lease_valid(value: &PostProcessJobLease, proof: &WorkerProof) -> DataResult<()> {
    uid(value.claim_id)?;
    uid(value.operation_id)?;
    safe_id(&value.job_id)?;
    uid(value.meeting_id)?;
    rev(&value.job_revision)?;
    source_valid(&value.input)?;
    if value.worker_session_id != proof.session_id
        || value.library_epoch != proof.library_epoch
        || value.change_sequence <= 0
        || value.dedup_until < value.expires_at
    {
        return Err(DataError::Forbidden);
    }
    Ok(())
}
pub(super) fn worker_on(
    conn: &Connection,
    peer: u32,
    epoch: Uuid,
    proof: &WorkerProof,
) -> DataResult<RegisteredWorker> {
    let worker = workers::authenticate(conn, peer, epoch, proof, false)?;
    if !worker.capabilities.contains(&WorkerCapability::PostProcess) {
        return Err(DataError::Forbidden);
    }
    Ok(worker)
}
pub(super) fn fingerprint<T: Serialize>(value: &T, limit: usize) -> DataResult<String> {
    struct Budget {
        hash: Sha256,
        used: usize,
        limit: usize,
    }
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|used| *used <= self.limit)
                .ok_or_else(|| std::io::Error::other("post-process byte budget"))?;
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Budget {
        hash: Sha256::new(),
        used: 0,
        limit,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| DataError::Invalid("post-process byte budget"))?;
    Ok(format!("{:x}", writer.hash.finalize()))
}
fn token_hash(token: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("post-process-token-v1:{token}").as_bytes())
    )
}
fn input_hash(job: &PostProcessJob) -> DataResult<String> {
    fingerprint(&(job.meeting_id, &job.input, &job.provider), MAX_RECEIPT)
}
pub(super) fn snapshot_on(conn: &Connection) -> DataResult<(String, i64)> {
    let scope: String = conn.query_row(
        "SELECT cursor_scope FROM service_search_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if scope.len() != 32
        || !scope
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(DataError::Invalid("post-process scope"));
    }
    let sequence: i64 = conn.query_row("SELECT MAX(COALESCE((SELECT MAX(sequence) FROM service_changes),0),COALESCE((SELECT seq FROM sqlite_sequence WHERE name='service_changes'),0))", [], |r| r.get(0))?;
    if !(0..=9_007_199_254_740_991).contains(&sequence) {
        return Err(DataError::Invalid("post-process sequence"));
    }
    Ok((scope, sequence))
}
pub(super) fn capture_context_on(conn: &Connection, id: &str) -> DataResult<CaptureContext> {
    let sql = format!("SELECT {CONTEXT_COLUMNS} {CONTEXT_FROM} AND j.id=?1");
    let raw = conn
        .query_row(&sql, [id], |r| {
            Ok(ContextRow {
                meeting_id: r.get(0)?,
                revision: r.get(1)?,
                capture_id: r.get(2)?,
                generation: r.get(3)?,
                provider: r.get(4)?,
                receipt: r.get(5)?,
                payload: r.get(6)?,
                lifecycle: r.get(7)?,
                transcript: r.get(8)?,
                source: r.get(9)?,
                document_state: r.get(10)?,
                last_sequence: r.get(11)?,
                tombstoned: r.get(12)?,
                status: r.get(13)?,
                capture_worker: r.get(14)?,
                capture_epoch: r.get(15)?,
            })
        })
        .optional()?
        .ok_or(DataError::Forbidden)?;
    if raw.tombstoned || !matches!(raw.status.as_str(), "processing" | "ready") {
        return Err(DataError::Forbidden);
    }
    let decode = || DataError::Invalid("post-process capture payload");
    let payload: CaptureJobPayload =
        serde_json::from_str(raw.payload.as_deref().ok_or_else(decode)?)?;
    let selection: ClosedSelection =
        serde_json::from_str(raw.provider.as_deref().ok_or_else(decode)?)?;
    let receipt: FinalCaptureReceipt =
        serde_json::from_str(raw.receipt.as_deref().ok_or_else(decode)?)?;
    let parse = |id: &str| {
        Uuid::parse_str(id).map_err(|_| DataError::Invalid("post-process capture identity"))
    };
    let meeting = parse(&raw.meeting_id)?;
    let capture = parse(&raw.capture_id)?;
    let generation = parse(&raw.generation)?;
    let capture_worker = raw
        .capture_worker
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
        .ok_or(DataError::Invalid("post-process capture worker"))?;
    let capture_epoch = raw
        .capture_epoch
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
        .ok_or(DataError::Invalid("post-process capture epoch"))?;
    for value in [meeting, capture, generation, capture_worker, capture_epoch] {
        uid(value)?;
    }
    if payload.capture_id != capture
        || payload.capture_generation != generation
        || payload.provider != selection
        || payload.required_capabilities != [WorkerCapability::PostProcess]
        || receipt.entity_id != raw.meeting_id
        || receipt.job_id.as_deref() != Some(id)
        || receipt.change_sequence <= 0
        || receipt.dedup_until.timestamp_millis() <= 0
    {
        return Err(DataError::Forbidden);
    }
    if !(0..=10_000).contains(&raw.last_sequence) {
        return Err(DataError::Invalid("post-process confirmed sequence"));
    }
    rev(&receipt.revision)?;
    rev(&payload.transcript_revision)?;
    rev(&raw.revision)?;
    rev(&raw.lifecycle)?;
    rev(&raw.transcript)?;
    let provider = CaptureSelection {
        provider: selection.provider,
        model: selection.model,
        language: selection.language,
    };
    workers::selection(&provider)?;
    let capture_fingerprint = fingerprint(
        &(
            id,
            &raw.meeting_id,
            &raw.capture_id,
            &raw.generation,
            &raw.capture_worker,
            &raw.capture_epoch,
            &raw.provider,
            &raw.receipt,
            &raw.payload,
            raw.last_sequence,
        ),
        16_384,
    )?;
    Ok(CaptureContext {
        job_id: id.into(),
        job_revision: raw.revision,
        meeting_id: meeting,
        capture_id: capture,
        capture_generation: generation,
        root_lifecycle_revision: receipt.revision,
        root_transcript_revision: payload.transcript_revision,
        capture_fingerprint,
        lifecycle_revision: raw.lifecycle,
        transcript_revision: raw.transcript,
        source_id: raw.source,
        document_state: raw.document_state,
        meeting_status: raw.status,
        last_sequence: raw.last_sequence,
        provider,
    })
}

pub(super) fn captured_source_on(
    conn: &Connection,
    meeting: Uuid,
    source: Option<&str>,
    last_sequence: i64,
    document_state: &str,
) -> DataResult<()> {
    match (source, last_sequence, document_state) {
        (None, 0, "absent") => {}
        (Some(source), count, "present") if count > 0 => {
            safe_id(source)?;
            let owns: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM service_transcript_sources WHERE id=?1 AND meeting_id=?2 AND origin='captured')", params![source,meeting.to_string()], |r| r.get(0))?;
            if !owns {
                return Err(DataError::Forbidden);
            }
        }
        (Some(source), 0, "present") => {
            safe_id(source)?;
            let owns: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM service_transcript_sources
                 WHERE id=?1 AND meeting_id=?2 AND origin='captured'
                 AND source_label='soniox' AND raw_json='[]')",
                params![source, meeting.to_string()],
                |row| row.get(0),
            )?;
            if !owns {
                return Err(DataError::Forbidden);
            }
        }
        _ => return Err(DataError::Forbidden),
    }
    Ok(())
}

pub(super) fn retained_live_source_on(
    conn: &Connection,
    meeting: Uuid,
    source: Option<&str>,
) -> DataResult<bool> {
    let Some(source) = source else {
        return Ok(false);
    };
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM service_transcript_sources
          WHERE id=?1 AND meeting_id=?2 AND origin='captured' AND source_label='local_live')",
        params![source, meeting.to_string()],
        |row| row.get(0),
    )?)
}

/// The schema-15 predicate, used only by explicit migration/finalize writers.
/// A read never adopts current state into a missing checkpoint.
pub(super) fn strict_capture_on(conn: &Connection, id: &str) -> DataResult<CaptureContext> {
    let value = capture_context_on(conn, id)?;
    if value.meeting_status != "processing" {
        return Err(DataError::Forbidden);
    }
    check_revision(&value.lifecycle_revision, &value.root_lifecycle_revision)?;
    check_revision(&value.transcript_revision, &value.root_transcript_revision)?;
    captured_source_on(
        conn,
        value.meeting_id,
        value.source_id.as_deref(),
        value.last_sequence,
        &value.document_state,
    )?;
    Ok(value)
}

pub(super) fn candidate_on(conn: &Connection, id: &str) -> DataResult<PostProcessJob> {
    postprocess_checkpoint::candidate_on(conn, capture_context_on(conn, id)?)
}

#[derive(Debug)]
struct Binding {
    session: String,
    epoch: String,
    fence: String,
    token: String,
    input: String,
    state: String,
}
fn binding_on(conn: &Connection, claim: Uuid, job: &str) -> DataResult<Binding> {
    conn.query_row("SELECT worker_session_id,library_epoch,fence_hash,token_hash,input_hash,state FROM service_worker_job_claims WHERE claim_id=?1 AND job_id=?2",params![claim.to_string(),job],|r|Ok(Binding{session:r.get(0)?,epoch:r.get(1)?,fence:r.get(2)?,token:r.get(3)?,input:r.get(4)?,state:r.get(5)?})).optional()?.ok_or(DataError::Forbidden)
}
pub(super) fn check_binding(
    conn: &Connection,
    proof: &WorkerProof,
    lease: &PostProcessJobLease,
    fence: &WorkerSecret,
) -> DataResult<String> {
    let binding = binding_on(conn, lease.claim_id, &lease.job_id)?;
    let token: Option<String> = conn.query_row(
        "SELECT claim_token FROM service_jobs WHERE id=?1",
        [&lease.job_id],
        |r| r.get(0),
    )?;
    if binding.state != "active"
        || binding.session != proof.session_id.to_string()
        || binding.epoch != proof.library_epoch.to_string()
        || binding.fence != fence.hash()?
        || token
            .as_ref()
            .is_none_or(|token| token_hash(token) != binding.token)
    {
        return Err(DataError::Forbidden);
    }
    let current = candidate_on(conn, &lease.job_id)?;
    if current.meeting_id != lease.meeting_id
        || current.input != lease.input
        || input_hash(&current)? != binding.input
    {
        return Err(DataError::Forbidden);
    }
    let token = token.ok_or(DataError::Forbidden)?;
    let job = execution::check_claim(conn, &lease.job_id, &token, Some(&lease.job_revision))?;
    if job.cancel_requested {
        return Err(DataError::Forbidden);
    }
    Ok(token)
}

pub(super) enum InstalledResult<'a> {
    Cleanup {
        transcript_revision: &'a str,
    },
    LocalSource {
        transcript_revision: &'a str,
        source: &'a str,
    },
    Speakers {
        transcript_revision: &'a str,
    },
    Ready {
        lifecycle_revision: &'a str,
    },
}

pub(super) fn same_claim_rotation(
    old: &PostProcessJobLease,
    new: &PostProcessJobLease,
    operation: Uuid,
) -> bool {
    new.operation_id == operation
        && new.claim_id == old.claim_id
        && new.worker_session_id == old.worker_session_id
        && new.library_epoch == old.library_epoch
        && new.job_id == old.job_id
        && new.meeting_id == old.meeting_id
        && new.job_revision != old.job_revision
        && new.input.capture_id == old.input.capture_id
        && new.input.capture_generation == old.input.capture_generation
        && new.input.checkpoint_revision != old.input.checkpoint_revision
        && new.expires_at == old.expires_at
        && (old.change_sequence < new.change_sequence
            && new.change_sequence <= 9_007_199_254_740_991)
}

/// Rotate ownership for a closed result after the caller installs it within
/// this same transaction. The earlier binding was checked before installation.
pub(super) fn advance_result_on(
    tx: &Transaction<'_>,
    proof: &WorkerProof,
    lease: &PostProcessJobLease,
    token: &str,
    result: InstalledResult<'_>,
    operation: Uuid,
) -> DataResult<PostProcessJobLease> {
    uid(operation)?;
    lease_valid(lease, proof)?;
    let binding = binding_on(tx, lease.claim_id, &lease.job_id)?;
    if binding.state != "active"
        || binding.session != proof.session_id.to_string()
        || binding.epoch != proof.library_epoch.to_string()
        || binding.token != token_hash(token)
    {
        return Err(DataError::Forbidden);
    }
    let job = execution::check_claim(tx, &lease.job_id, token, Some(&lease.job_revision))?;
    if job.cancel_requested {
        return Err(DataError::Forbidden);
    }
    let expires_at: DateTime<Utc> = tx.query_row(
        "SELECT lease_until FROM service_jobs WHERE id=?1",
        [&lease.job_id],
        |row| row.get(0),
    )?;
    if expires_at != lease.expires_at {
        return Err(DataError::Forbidden);
    }
    let previous_phase: String = tx
        .query_row(
            "SELECT phase FROM service_post_process_checkpoints WHERE job_id=?1",
            [&lease.job_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(DataError::Forbidden)?;
    let mut source = lease.input.source_id.as_deref();
    let mut lifecycle = lease.input.lifecycle_revision.as_str();
    let (phase, transcript_revision) = match (previous_phase.as_str(), result) {
        (
            "captured" | "local_asr",
            InstalledResult::Cleanup {
                transcript_revision,
            },
        ) => ("automatic_cleanup", transcript_revision),
        (
            "ready",
            InstalledResult::Cleanup {
                transcript_revision,
            },
        ) => ("ready_cleanup", transcript_revision),
        (
            "captured",
            InstalledResult::LocalSource {
                transcript_revision,
                source: new_source,
            },
        ) => {
            safe_id(new_source)?;
            if source == Some(new_source)
                || retained_live_source_on(tx, lease.meeting_id, source)?
                || lease.input.transcript_revision == transcript_revision
            {
                return Err(DataError::Forbidden);
            }
            source = Some(new_source);
            ("local_asr", transcript_revision)
        }
        (
            phase @ ("local_asr" | "captured"),
            InstalledResult::Speakers {
                transcript_revision,
            },
        ) if source.is_some()
            && (phase == "local_asr"
                || capture_context_on(tx, &lease.job_id)?.provider.provider
                    == CaptureProvider::Soniox
                || retained_live_source_on(tx, lease.meeting_id, source)?) =>
        {
            ("speakers", transcript_revision)
        }
        ("speakers", InstalledResult::Ready { lifecycle_revision })
            if source.is_some() && lifecycle_revision != lifecycle =>
        {
            rev(lifecycle_revision)?;
            lifecycle = lifecycle_revision;
            ("ready", lease.input.transcript_revision.as_str())
        }
        _ => return Err(DataError::Forbidden),
    };
    rev(transcript_revision)?;
    let checkpoint = revision();
    let document_state = if source.is_some() {
        "present"
    } else {
        "absent"
    };
    if tx.execute(
        "UPDATE service_post_process_checkpoints SET revision=?2,transcript_revision=?3,
         phase=?8,source_id=?9,document_state=?10,lifecycle_revision=?11
         WHERE job_id=?1 AND revision=?4 AND transcript_revision=?5 AND lifecycle_revision=?6
           AND source_id IS ?7 AND state='active' AND phase=?12",
        params![
            lease.job_id,
            checkpoint,
            transcript_revision,
            lease.input.checkpoint_revision,
            lease.input.transcript_revision,
            lease.input.lifecycle_revision,
            lease.input.source_id,
            phase,
            source,
            document_state,
            lifecycle,
            previous_phase,
        ],
    )? != 1
    {
        return Err(DataError::Forbidden);
    }
    let job_revision = revision();
    let new_token = revision();
    if tx.execute("UPDATE service_jobs SET revision=?2,claim_token=?3,updated_at=?4 WHERE id=?1 AND revision=?5 AND state='running' AND cancel_requested=0",params![lease.job_id,job_revision,new_token,Utc::now(),lease.job_revision])?!=1 {return Err(DataError::Forbidden);}
    let current = candidate_on(tx, &lease.job_id)?;
    if tx.execute("UPDATE service_worker_job_claims SET token_hash=?2,input_hash=?3 WHERE claim_id=?1 AND state='active' AND token_hash=?4",params![lease.claim_id.to_string(),token_hash(&new_token),input_hash(&current)?,binding.token])?!=1 {return Err(DataError::Forbidden);}
    let sequence = consistency::change(
        tx,
        "job",
        &lease.job_id,
        &job_revision,
        &format!("worker:{}", proof.session_id),
    )?;
    let result = PostProcessJobLease {
        operation_id: operation,
        claim_id: lease.claim_id,
        worker_session_id: proof.session_id,
        library_epoch: proof.library_epoch,
        job_id: lease.job_id.clone(),
        meeting_id: lease.meeting_id,
        job_revision,
        input: current.input,
        expires_at,
        change_sequence: sequence,
        dedup_until: Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS),
    };
    lease_valid(&result, proof)?;
    Ok(result)
}
fn replay<T: DeserializeOwned>(
    conn: &Connection,
    proof: &WorkerProof,
    operation: Uuid,
    kind: &str,
    hash: &str,
) -> DataResult<Option<T>> {
    let row:Option<(String,String,String,String,DateTime<Utc>)>=conn.query_row("SELECT library_epoch,operation,arguments_hash,CASE WHEN length(CAST(outcome_json AS BLOB))<=4096 THEN outcome_json END,expires_at FROM service_worker_job_receipts WHERE worker_session_id=?1 AND operation_id=?2",params![proof.session_id.to_string(),operation.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((epoch, saved_kind, saved_hash, body, until)) = row else {
        return Ok(None);
    };
    if epoch != proof.library_epoch.to_string() || until <= Utc::now() {
        return Err(DataError::Forbidden);
    }
    if saved_kind != kind || saved_hash != hash {
        return Err(DataError::IdempotencyConflict);
    }
    Ok(Some(serde_json::from_str(&body)?))
}
fn receipt<T: Serialize>(
    tx: &Transaction<'_>,
    proof: &WorkerProof,
    operation: Uuid,
    kind: &str,
    hash: &str,
    value: &T,
    until: DateTime<Utc>,
) -> DataResult<()> {
    fingerprint(value, MAX_RECEIPT)?;
    tx.execute("INSERT INTO service_worker_job_receipts(worker_session_id,operation_id,library_epoch,operation,arguments_hash,outcome_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![proof.session_id.to_string(),operation.to_string(),proof.library_epoch.to_string(),kind,hash,serde_json::to_string(value)?,until])?;
    Ok(())
}
fn expiry(worker: &RegisteredWorker, seconds: i64) -> DataResult<DateTime<Utc>> {
    if !(5..=300).contains(&seconds) {
        return Err(DataError::Invalid("post-process TTL"));
    }
    let now = Utc::now();
    let until = (now + Duration::seconds(seconds)).min(worker.expires_at);
    if until <= now {
        return Err(DataError::Forbidden);
    }
    Ok(until)
}
fn current_sequence(conn: &Connection) -> DataResult<i64> {
    Ok(snapshot_on(conn)?.1)
}

impl Store {
    pub fn service_post_process_transcript(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessTranscriptRequest,
    ) -> DataResult<DesktopTranscriptPage> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        worker_on(&tx, peer, epoch, &request.proof)?;
        lease_valid(&request.lease, &request.proof)?;
        check_binding(&tx, &request.proof, &request.lease, &request.fence)?;
        let input = &request.lease.input;
        let meeting = request.lease.meeting_id.to_string();
        if request.page.meeting_id != meeting
            || request.page.expected_revision != input.transcript_revision
            || request
                .page
                .source_id
                .as_ref()
                .is_some_and(|id| Some(id) != input.source_id.as_ref())
            || request.page.cursor.as_ref().is_some_and(|cursor| {
                cursor.meeting_id != meeting
                    || cursor.revision != input.transcript_revision
                    || cursor.current_source_id != input.source_id
                    || cursor.source_id != input.source_id
            })
        {
            return Err(DataError::Forbidden);
        }
        let page = content::transcript_on(&tx, &request.page)?;
        let expected_state = if input.source_id.is_some() {
            DocumentState::Present
        } else {
            DocumentState::Absent
        };
        if page.state.document_state != expected_state
            || page.state.current_source_id != input.source_id
            || page.source_id != input.source_id
        {
            return Err(DataError::Forbidden);
        }
        Ok(page)
    }

    pub fn service_post_process_jobs(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessJobsRequest,
    ) -> DataResult<PostProcessJobPage> {
        if !(1..=POST_PROCESS_MAX_ITEMS).contains(&request.limit)
            || !(1024..=POST_PROCESS_MAX_DATA_BYTES).contains(&request.max_bytes)
        {
            return Err(DataError::Invalid("post-process page"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        worker_on(&tx, peer, epoch, &request.proof)?;
        let (scope, sequence) = snapshot_on(&tx)?;
        let now = Utc::now();
        let eligible = request
            .cursor
            .as_ref()
            .map_or(now, |cursor| cursor.eligible_at);
        let query_hash = fingerprint(
            &(
                scope.as_str(),
                epoch,
                request.proof.session_id,
                sequence,
                eligible,
                request.limit,
                request.max_bytes,
            ),
            MAX_RECEIPT,
        )?;
        let after = if let Some(cursor) = &request.cursor {
            safe_id(&cursor.after_job_id)?;
            if cursor.library_scope != scope
                || cursor.library_epoch != epoch
                || cursor.worker_session_id != request.proof.session_id
                || cursor.snapshot_sequence != sequence
                || cursor.eligible_at > now
                || cursor.eligible_at.timestamp_millis() < 0
                || cursor.query_hash != query_hash
            {
                return Err(DataError::StaleCursor);
            }
            cursor.after_job_id.as_str()
        } else {
            ""
        };
        // A fixed eligibility cutoff prevents time alone changing membership
        // between pages. Source/lifecycle changes invalidate the high-watermark.
        let combined_registration = "EXISTS(SELECT 1 FROM settings reg JOIN service_transcript_sources src ON src.id=p.source_id
            WHERE reg.key='transcript-version-publication:'||length(m.id)||':'||m.id
              AND CASE WHEN json_valid(reg.value) THEN json_extract(reg.value,'$.job_id') END=p.job_id
              AND CASE WHEN json_valid(reg.value) THEN json_extract(reg.value,'$.source_id') END=p.source_id
              AND CASE WHEN json_valid(reg.value) THEN json_extract(reg.value,'$.effective_id') END=src.source_label
              AND src.meeting_id=m.id AND src.origin='combined')";
        let sql=format!("SELECT CASE WHEN length(CAST(j.id AS BLOB))<=128 THEN j.id END {CONTEXT_FROM}
          AND j.id>?1 AND j.cancel_requested=0
          AND EXISTS(SELECT 1 FROM service_post_process_checkpoints p JOIN service_post_process_anchors a ON a.job_id=p.job_id WHERE p.job_id=j.id AND a.meeting_id=m.id AND p.state='active' AND p.lifecycle_revision=s.lifecycle_revision AND p.transcript_revision=t.revision AND p.source_id IS t.current_source_id AND p.document_state=t.document_state
            AND ((m.status='processing' AND p.phase IN ('captured','local_asr','automatic_cleanup','speakers') AND p.lifecycle_revision=a.root_lifecycle_revision)
              OR (m.status='ready' AND p.phase IN ('ready','ready_cleanup') AND p.lifecycle_revision<>a.root_lifecycle_revision))
            AND ((p.phase IN ('captured','automatic_cleanup') AND p.source_id IS a.root_source_id AND p.document_state=a.root_document_state)
              OR (p.phase IN ('speakers','ready','ready_cleanup') AND p.source_id IS a.root_source_id
                AND p.document_state='present' AND EXISTS(SELECT 1 FROM service_transcript_sources src
                  WHERE src.id=p.source_id AND src.meeting_id=m.id AND src.origin='captured' AND src.source_label IN ('soniox','local_live')))
              OR (p.phase IN ('local_asr','automatic_cleanup','speakers','ready','ready_cleanup') AND p.source_id IS NOT a.root_source_id AND p.document_state='present'
                AND EXISTS(SELECT 1 FROM service_post_process_local_sources l WHERE l.job_id=p.job_id AND l.source_id=p.source_id))
              OR (p.phase IN ('ready','ready_cleanup') AND p.source_id IS NOT a.root_source_id AND p.document_state='present' AND {combined_registration}))
            AND (p.phase NOT IN ('speakers','ready','ready_cleanup')
              OR EXISTS(SELECT 1 FROM service_post_process_speaker_results s WHERE s.job_id=p.job_id AND s.source_id=p.source_id)
              OR (p.phase IN ('ready','ready_cleanup') AND {combined_registration}))
            AND CASE WHEN length(CAST(c.outcome_json AS BLOB))<=2048 THEN CASE WHEN json_valid(c.outcome_json) THEN json_extract(c.outcome_json,'$.revision') END END=a.root_lifecycle_revision
            AND CASE WHEN length(CAST(j.payload_json AS BLOB))<=4096 THEN CASE WHEN json_valid(j.payload_json) THEN json_extract(j.payload_json,'$.transcript_revision') END END=a.root_transcript_revision)
          AND (j.state IN ('waiting_for_worker','interrupted') OR (j.state='running' AND (j.lease_until<=?2 OR EXISTS(SELECT 1 FROM service_worker_job_claims b WHERE b.job_id=j.id AND b.state='active' AND b.library_epoch<>?3))))
          ORDER BY j.id LIMIT ?4");
        let ids = {
            let mut statement = tx.prepare(&sql)?;
            let rows = statement
                .query_map(
                    params![
                        after,
                        eligible,
                        epoch.to_string(),
                        i64::from(request.limit) + 1
                    ],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut page = PostProcessJobPage {
            library_scope: scope.clone(),
            snapshot_sequence: sequence,
            eligible_at: eligible,
            items: Vec::new(),
            next: None,
        };
        for (index, id) in ids.iter().enumerate() {
            if index == request.limit as usize {
                break;
            }
            let candidate = candidate_on(&tx, id)?;
            page.items.push(candidate);
            let more = index + 1 < ids.len();
            page.next = more.then(|| PostProcessJobCursor {
                library_scope: scope.clone(),
                library_epoch: epoch,
                worker_session_id: request.proof.session_id,
                snapshot_sequence: sequence,
                eligible_at: eligible,
                after_job_id: id.clone(),
                query_hash: query_hash.clone(),
            });
            if fingerprint(&page, request.max_bytes as usize).is_err() {
                page.items.pop();
                let Some(last) = page.items.last() else {
                    return Err(DataError::Invalid("post-process page budget"));
                };
                page.next = Some(PostProcessJobCursor {
                    library_scope: scope.clone(),
                    library_epoch: epoch,
                    worker_session_id: request.proof.session_id,
                    snapshot_sequence: sequence,
                    eligible_at: eligible,
                    after_job_id: last.job_id.clone(),
                    query_hash: query_hash.clone(),
                });
                fingerprint(&page, request.max_bytes as usize)?;
                break;
            }
        }
        tx.commit()?;
        Ok(page)
    }

    pub fn service_claim_post_process_job(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &ClaimPostProcessJobRequest,
    ) -> DataResult<PostProcessJobLease> {
        uid(request.claim_id)?;
        uid(request.operation_id)?;
        job_valid(&request.job)?;
        request.fence.validate()?;
        let hash = fingerprint(request, MAX_OPERATION)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let worker = worker_on(&tx, peer, epoch, &request.proof)?;
        if let Some(value) = replay(&tx, &request.proof, request.operation_id, "claim", &hash)? {
            return Ok(value);
        }
        let until = expiry(&worker, request.ttl_seconds)?;
        let current = candidate_on(&tx, &request.job.job_id)?;
        check_revision(&current.revision, &request.job.revision)?;
        if current != request.job {
            return Err(DataError::Forbidden);
        }
        let collision: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM service_worker_job_claims WHERE claim_id=?1)",
            [request.claim_id.to_string()],
            |r| r.get(0),
        )?;
        if collision {
            return Err(DataError::IdempotencyConflict);
        }
        let stale:Option<(String,String,Option<String>)>=tx.query_row("SELECT b.library_epoch,b.token_hash,j.claim_token FROM service_worker_job_claims b JOIN service_jobs j ON j.id=b.job_id WHERE b.job_id=?1 AND b.state='active'",[&current.job_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let old_epoch = stale.is_some_and(|(old, hash, token)| {
            old != epoch.to_string() && token.is_some_and(|token| token_hash(&token) == hash)
        });
        let actor = format!("worker:{}", worker.session_id);
        let claim = execution::claim_job_on(
            &tx,
            &current.job_id,
            JobExecutor::GuiWorker,
            &current.revision,
            until,
            old_epoch,
            Some(&actor),
        )?;
        tx.execute("INSERT INTO service_worker_job_claims(claim_id,job_id,worker_session_id,library_epoch,fence_hash,token_hash,input_hash,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8)",params![request.claim_id.to_string(),current.job_id,worker.session_id.to_string(),epoch.to_string(),request.fence.hash()?,token_hash(&claim.token),input_hash(&current)?,Utc::now()])?;
        let value = PostProcessJobLease {
            operation_id: request.operation_id,
            claim_id: request.claim_id,
            worker_session_id: worker.session_id,
            library_epoch: epoch,
            job_id: current.job_id,
            meeting_id: current.meeting_id,
            job_revision: claim.job.revision,
            input: current.input,
            expires_at: until,
            change_sequence: current_sequence(&tx)?,
            dedup_until: Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS),
        };
        receipt(
            &tx,
            &request.proof,
            request.operation_id,
            "claim",
            &hash,
            &value,
            value.dedup_until,
        )?;
        tx.commit()?;
        Ok(value)
    }

    pub fn service_renew_post_process_job(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &RenewPostProcessJobRequest,
    ) -> DataResult<PostProcessJobLease> {
        uid(request.operation_id)?;
        lease_valid(&request.lease, &request.proof)?;
        request.fence.validate()?;
        let hash = fingerprint(request, MAX_OPERATION)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let worker = worker_on(&tx, peer, epoch, &request.proof)?;
        if let Some(value) = replay(&tx, &request.proof, request.operation_id, "renew", &hash)? {
            return Ok(value);
        }
        let until = expiry(&worker, request.ttl_seconds)?;
        let token = check_binding(&tx, &request.proof, &request.lease, &request.fence)?;
        let actor = format!("worker:{}", worker.session_id);
        let claim = execution::renew_job_on(
            &tx,
            &request.lease.job_id,
            &token,
            &request.lease.job_revision,
            until,
            Some(&actor),
        )?;
        let value = PostProcessJobLease {
            operation_id: request.operation_id,
            job_revision: claim.job.revision,
            expires_at: until,
            change_sequence: current_sequence(&tx)?,
            dedup_until: Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS),
            ..request.lease.clone()
        };
        receipt(
            &tx,
            &request.proof,
            request.operation_id,
            "renew",
            &hash,
            &value,
            value.dedup_until,
        )?;
        tx.commit()?;
        Ok(value)
    }

    pub fn service_fail_post_process_job(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &FailPostProcessJobRequest,
    ) -> DataResult<PostProcessFailureReceipt> {
        uid(request.operation_id)?;
        lease_valid(&request.lease, &request.proof)?;
        request.fence.validate()?;
        let hash = fingerprint(request, MAX_OPERATION)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let worker = worker_on(&tx, peer, epoch, &request.proof)?;
        if let Some(value) = replay(&tx, &request.proof, request.operation_id, "fail", &hash)? {
            return Ok(value);
        }
        let token = check_binding(&tx, &request.proof, &request.lease, &request.fence)?;
        let actor = format!("worker:{}", worker.session_id);
        let code = match request.error_code {
            PostProcessFailureCode::PostProcessFailed => "POST_PROCESS_FAILED",
            PostProcessFailureCode::PostProcessInputUnavailable => "POST_PROCESS_INPUT_UNAVAILABLE",
        };
        let finished = execution::finish_job_on(
            &tx,
            &request.lease.job_id,
            &token,
            &request.lease.job_revision,
            Some(code),
            Some(&actor),
        )?;
        let lifecycle = revision();
        let id = request.lease.meeting_id.to_string();
        if tx.execute(
            "UPDATE meetings SET status='failed',error=?2 WHERE id=?1 AND status='processing'",
            params![id, code],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        if tx.execute("UPDATE service_meeting_state SET lifecycle_revision=?2 WHERE meeting_id=?1 AND lifecycle_revision=?3 AND tombstoned=0",params![id,lifecycle,request.lease.input.lifecycle_revision])?!=1 {return Err(DataError::Forbidden);}
        let sequence = consistency::change(&tx, "meeting_lifecycle", &id, &lifecycle, &actor)?;
        tx.execute("UPDATE service_worker_job_claims SET state='failed' WHERE claim_id=?1 AND state='active'",[request.lease.claim_id.to_string()])?;
        let value = PostProcessFailureReceipt {
            operation_id: request.operation_id,
            claim_id: request.lease.claim_id,
            worker_session_id: worker.session_id,
            library_epoch: epoch,
            job_id: finished.id,
            meeting_id: request.lease.meeting_id,
            job_revision: finished.revision,
            lifecycle_revision: lifecycle,
            error_code: request.error_code,
            change_sequence: sequence,
            dedup_until: Utc::now() + Duration::seconds(DEDUP_RETENTION_SECONDS),
        };
        receipt(
            &tx,
            &request.proof,
            request.operation_id,
            "fail",
            &hash,
            &value,
            value.dedup_until,
        )?;
        tx.commit()?;
        Ok(value)
    }
}

#[cfg(test)]
#[path = "postprocess_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "postprocess_input_tests.rs"]
mod input_tests;

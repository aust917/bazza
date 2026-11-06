//! Durable worker/capture state. Core authenticates worker requests and holds the
//! released artifact lock through finalization or recovery. Heartbeat expiry
//! alone never authorizes recovery.
#[cfg(test)]
#[path = "capture_start_tests.rs"]
mod start_tests;
use super::*;
use crate::Store;
use bazza_domain::{capture_screen::RawScreenEvidence, capture_stt::SonioxCaptureOutcome};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const CAPTION_LIMIT: usize = 10_000;

/// Client-generated random 256-bit secret. Only its digest is persisted.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct WorkerSecret(String);
impl std::fmt::Debug for WorkerSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerSecret([redacted])")
    }
}
impl WorkerSecret {
    pub fn random() -> Self {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).expect("OS randomness unavailable for worker secret");
        Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
    pub fn from_string(value: String) -> DataResult<Self> {
        let secret = Self(value);
        secret.validate()?;
        Ok(secret)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub(super) fn validate(&self) -> DataResult<()> {
        if self.0.len() != 64
            || !self
                .0
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DataError::Invalid("worker secret"));
        }
        Ok(())
    }
    pub(super) fn hash(&self) -> DataResult<String> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(self.0.as_bytes())))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCapability {
    Capture,
    PostProcess,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRegistration {
    pub session_id: Uuid,
    pub secret: WorkerSecret,
    pub capabilities: Vec<WorkerCapability>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerProof {
    pub session_id: Uuid,
    pub secret: WorkerSecret,
    pub library_epoch: Uuid,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredWorker {
    pub session_id: Uuid,
    pub library_epoch: Uuid,
    pub peer_pid: u32,
    pub capabilities: Vec<WorkerCapability>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProvider {
    Local,
    Soniox,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureSelection {
    pub provider: CaptureProvider,
    pub model: Option<String>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureStart {
    pub capture_id: Uuid,
    pub meeting_id: Uuid,
    pub fence: WorkerSecret,
    pub title: String,
    pub app: Option<String>,
    pub started_at: DateTime<Utc>,
    pub participants: Vec<String>,
    pub provider: CaptureSelection,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureLease {
    pub capture_id: Uuid,
    pub meeting_id: Uuid,
    pub worker_session_id: Uuid,
    pub generation: Uuid,
    pub library_epoch: Uuid,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmedCaption {
    pub sequence: u64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTerminalStatus {
    Completed,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureTerminal {
    pub status: CaptureTerminalStatus,
    pub provider: CaptureSelection,
    pub duration_ms: i64,
    pub ended_at: DateTime<Utc>,
    pub has_video: bool,
    pub last_confirmed_sequence: u64,
    pub captions: Vec<ConfirmedCaption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_screen: Option<RawScreenEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soniox_outcome: Option<SonioxCaptureOutcome>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retain_local_live: bool,
}

fn valid_uuid(value: Uuid) -> DataResult<()> {
    if value.is_nil() {
        Err(DataError::Invalid("nil identifier"))
    } else {
        Ok(())
    }
}
fn ttl(seconds: i64) -> DataResult<DateTime<Utc>> {
    if !(5..=300).contains(&seconds) {
        return Err(DataError::Invalid("worker TTL"));
    }
    Ok(Utc::now() + Duration::seconds(seconds))
}
pub(super) fn selection(value: &CaptureSelection) -> DataResult<()> {
    if let Some(model) = &value.model {
        if model.is_empty()
            || model.len() > 128
            || !model.as_bytes()[0].is_ascii_alphanumeric()
            || !model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
        {
            return Err(DataError::Invalid("capture model identifier"));
        }
    }
    if let Some(language) = &value.language {
        bounded_text(language, 2, 64)?;
        if !language.split('-').all(|part| {
            !part.is_empty() && part.len() <= 8 && part.bytes().all(|c| c.is_ascii_alphanumeric())
        }) || !language
            .split('-')
            .next()
            .is_some_and(|part| part.len() >= 2 && part.bytes().all(|c| c.is_ascii_alphabetic()))
        {
            return Err(DataError::Invalid("capture language"));
        }
    }
    Ok(())
}
fn fingerprint<T: Serialize>(value: &T, limit: usize) -> DataResult<String> {
    struct HashBudget {
        hash: Sha256,
        used: usize,
        limit: usize,
    }
    impl std::io::Write for HashBudget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| std::io::Error::other("capture budget"))?;
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashBudget {
        hash: Sha256::new(),
        used: 0,
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| DataError::Invalid("capture budget"))?;
    Ok(format!("{:x}", writer.hash.finalize()))
}
fn worker_on(conn: &Connection, id: Uuid) -> DataResult<(RegisteredWorker, String, bool)> {
    type WorkerRecord = (u32, String, String, String, DateTime<Utc>, bool);
    let raw:Option<WorkerRecord>=conn.query_row("SELECT peer_pid,secret_hash,library_epoch,capabilities_json,expires_at,revoked FROM service_worker_sessions WHERE id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let Some((peer_pid, hash, epoch, caps, expires_at, revoked)) = raw else {
        return Err(DataError::NotFound);
    };
    Ok((
        RegisteredWorker {
            session_id: id,
            peer_pid,
            library_epoch: Uuid::parse_str(&epoch)
                .map_err(|_| DataError::Invalid("stored epoch"))?,
            capabilities: serde_json::from_str(&caps)?,
            expires_at,
        },
        hash,
        revoked,
    ))
}
pub(super) fn authenticate(
    conn: &Connection,
    peer_pid: u32,
    current_epoch: Uuid,
    proof: &WorkerProof,
    allow_expired: bool,
) -> DataResult<RegisteredWorker> {
    valid_uuid(current_epoch)?;
    valid_uuid(proof.session_id)?;
    let (worker, hash, revoked) =
        worker_on(conn, proof.session_id).map_err(|error| match error {
            DataError::NotFound => DataError::Forbidden,
            other => other,
        })?;
    if peer_pid == 0
        || worker.peer_pid != peer_pid
        || revoked
        || worker.library_epoch != current_epoch
        || proof.library_epoch != current_epoch
        || hash != proof.secret.hash()?
        || (!allow_expired && worker.expires_at <= Utc::now())
    {
        return Err(DataError::Forbidden);
    }
    Ok(worker)
}
fn lease_on(conn: &Connection, id: Uuid) -> DataResult<CaptureLease> {
    let values:Option<(String,String,String,String)>=conn.query_row("SELECT meeting_id,worker_session_id,generation,library_epoch FROM service_capture_leases WHERE id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((meeting, session, generation, epoch)) = values else {
        return Err(DataError::NotFound);
    };
    let parse =
        |s: &str| Uuid::parse_str(s).map_err(|_| DataError::Invalid("stored capture identity"));
    Ok(CaptureLease {
        capture_id: id,
        meeting_id: parse(&meeting)?,
        worker_session_id: parse(&session)?,
        generation: parse(&generation)?,
        library_epoch: parse(&epoch)?,
    })
}
pub(super) fn capture_identity_authority(
    conn: &Connection,
    worker: &RegisteredWorker,
    lease: &CaptureLease,
    fence: &WorkerSecret,
) -> DataResult<()> {
    let stored = lease_on(conn, lease.capture_id)?;
    if stored != *lease
        || lease.worker_session_id != worker.session_id
        || lease.library_epoch != worker.library_epoch
    {
        return Err(DataError::Forbidden);
    }
    let hash: String = conn.query_row(
        "SELECT fence_hash FROM service_capture_leases WHERE id=?1",
        [lease.capture_id.to_string()],
        |r| r.get(0),
    )?;
    if hash != fence.hash()? {
        return Err(DataError::Forbidden);
    }
    Ok(())
}
pub(super) fn capture_authority(
    conn: &Connection,
    worker: &RegisteredWorker,
    lease: &CaptureLease,
    fence: &WorkerSecret,
) -> DataResult<()> {
    capture_identity_authority(conn, worker, lease, fence)?;
    let (current_generation, tombstoned): (String, bool) = conn.query_row(
        "SELECT lifecycle_revision,tombstoned FROM service_meeting_state WHERE meeting_id=?1",
        [lease.meeting_id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if tombstoned {
        return Err(DataError::Forbidden);
    }
    check_revision(&current_generation, &lease.generation.to_string())
}

/// Narrow authentication for the first reattach or its lost-ACK replay. It
/// grants no normal mutation permission under an old service epoch.
fn reattach_context_on(
    conn: &Connection,
    peer_pid: u32,
    current_epoch: Uuid,
    proof: &WorkerProof,
    capture_id: Uuid,
    fence: &WorkerSecret,
) -> DataResult<(CaptureLease, CaptureSelection)> {
    valid_uuid(current_epoch)?;
    valid_uuid(proof.session_id)?;
    valid_uuid(proof.library_epoch)?;
    let (worker, hash, revoked) =
        worker_on(conn, proof.session_id).map_err(|error| match error {
            DataError::NotFound => DataError::Forbidden,
            other => other,
        })?;
    if peer_pid == 0
        || worker.peer_pid != peer_pid
        || revoked
        || hash != proof.secret.hash()?
        || !worker.capabilities.contains(&WorkerCapability::Capture)
        || (worker.library_epoch != proof.library_epoch && worker.library_epoch != current_epoch)
    {
        return Err(DataError::Forbidden);
    }
    let lease = lease_on(conn, capture_id)?;
    let (state, provider): (String, String) = conn.query_row(
        "SELECT state,provider_json FROM service_capture_leases WHERE id=?1",
        [capture_id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    capture_identity_authority(conn, &worker, &lease, fence)?;
    if !matches!(state.as_str(), "reserved" | "capturing") {
        return Err(DataError::Busy);
    }
    capture_authority(conn, &worker, &lease, fence)?;
    Ok((lease, serde_json::from_str(&provider)?))
}

impl Store {
    /// Internal orchestration metadata; no worker credentials or raw payloads.
    pub fn service_capture_selection(&self, capture_id: Uuid) -> DataResult<CaptureSelection> {
        let conn = self.conn.lock();
        let json: String = conn
            .query_row(
                "SELECT provider_json FROM service_capture_leases WHERE id=?1",
                [capture_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(DataError::NotFound)?;
        Ok(serde_json::from_str(&json)?)
    }
    /// `peer_pid` and `current_epoch` are trusted host context, never wire claims.
    pub fn service_register_worker(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        input: &WorkerRegistration,
        ttl_seconds: i64,
    ) -> DataResult<RegisteredWorker> {
        valid_uuid(current_epoch)?;
        valid_uuid(input.session_id)?;
        let expiry = ttl(ttl_seconds)?;
        if peer_pid == 0 || input.capabilities.is_empty() || input.capabilities.len() > 2 {
            return Err(DataError::Invalid("worker registration"));
        }
        let mut caps = input.capabilities.clone();
        caps.sort();
        caps.dedup();
        if caps.len() != input.capabilities.len() {
            return Err(DataError::Invalid("worker capabilities"));
        }
        let hash = input.secret.hash()?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match worker_on(&tx, input.session_id) {
            Ok((existing, saved, revoked)) => {
                if revoked
                    || existing.peer_pid != peer_pid
                    || existing.library_epoch != current_epoch
                    || existing.capabilities != caps
                    || saved != hash
                {
                    return Err(DataError::Forbidden);
                }
                return Ok(existing);
            }
            Err(DataError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM service_worker_sessions WHERE revoked=0 AND expires_at>?1",
            [Utc::now()],
            |r| r.get(0),
        )?;
        if active >= 32 {
            return Err(DataError::Busy);
        }
        tx.execute("INSERT INTO service_worker_sessions(id,peer_pid,secret_hash,library_epoch,capabilities_json,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![input.session_id.to_string(),peer_pid,hash,current_epoch.to_string(),serde_json::to_string(&caps)?,expiry,Utc::now()])?;
        let worker = worker_on(&tx, input.session_id)?.0;
        tx.commit()?;
        Ok(worker)
    }
    pub fn service_renew_worker(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        proof: &WorkerProof,
        ttl_seconds: i64,
    ) -> DataResult<RegisteredWorker> {
        let expiry = ttl(ttl_seconds)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        authenticate(&tx, peer_pid, current_epoch, proof, false)?;
        tx.execute(
            "UPDATE service_worker_sessions SET expires_at=?2 WHERE id=?1",
            params![proof.session_id.to_string(), expiry],
        )?;
        let worker = worker_on(&tx, proof.session_id)?.0;
        tx.commit()?;
        Ok(worker)
    }
    pub fn service_close_worker(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        proof: &WorkerProof,
    ) -> DataResult<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        authenticate(&tx, peer_pid, current_epoch, proof, true)?;
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM service_capture_leases WHERE worker_session_id=?1 AND state IN ('reserved','capturing'))",[proof.session_id.to_string()],|r|r.get(0))?;
        if active {
            return Err(DataError::Busy);
        }
        tx.execute(
            "UPDATE service_worker_sessions SET revoked=1 WHERE id=?1",
            [proof.session_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Authenticate before core touches any capture artifacts. Finalized leases
    /// keep their immutable capture identity for exact receipt replay, even when
    /// the current meeting lifecycle revision has advanced.
    pub fn service_capture_context(
        &self,
        peer_pid: u32,
        expected_epoch: Uuid,
        proof: &WorkerProof,
        capture_id: Uuid,
        fence: &WorkerSecret,
    ) -> DataResult<(CaptureLease, CaptureSelection)> {
        let conn = self.conn.lock();
        let worker = authenticate(&conn, peer_pid, expected_epoch, proof, true)?;
        if !worker.capabilities.contains(&WorkerCapability::Capture) {
            return Err(DataError::Forbidden);
        }
        let lease = lease_on(&conn, capture_id)?;
        capture_identity_authority(&conn, &worker, &lease, fence)?;
        let selection: String = conn.query_row(
            "SELECT provider_json FROM service_capture_leases WHERE id=?1",
            [capture_id.to_string()],
            |r| r.get(0),
        )?;
        Ok((lease, serde_json::from_str(&selection)?))
    }
    /// Read only a committed minimal outcome using the original worker epoch.
    /// This permits a lost finalize ACK to be replayed after service restart,
    /// without accepting any old-epoch mutation or reading capture artifacts.
    pub fn service_capture_receipt(
        &self,
        peer_pid: u32,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
    ) -> DataResult<Option<MutationOutcome>> {
        let conn = self.conn.lock();
        let worker = authenticate(&conn, peer_pid, proof.library_epoch, proof, true)?;
        capture_identity_authority(&conn, &worker, lease, fence)?;
        let (state, outcome): (String, Option<String>) = conn.query_row(
            "SELECT state,outcome_json FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if state != "finalized" {
            return Ok(None);
        }
        let body = outcome.ok_or(DataError::Invalid("missing capture receipt"))?;
        Ok(Some(serde_json::from_str(&body)?))
    }
    /// Authenticate a reattach before core inspects the live artifact owner.
    /// A valid old proof can retry the same transition after its ACK was lost.
    pub fn service_reattach_context(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        proof: &WorkerProof,
        capture_id: Uuid,
        fence: &WorkerSecret,
    ) -> DataResult<(CaptureLease, CaptureSelection)> {
        reattach_context_on(
            &self.conn.lock(),
            peer_pid,
            current_epoch,
            proof,
            capture_id,
            fence,
        )
    }
    pub fn service_prepare_capture(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        proof: &WorkerProof,
        input: &CaptureStart,
    ) -> DataResult<CaptureLease> {
        valid_uuid(input.capture_id)?;
        valid_uuid(input.meeting_id)?;
        bounded_text(&input.title, 1, 512)?;
        selection(&input.provider)?;
        if let Some(app) = &input.app {
            bounded_text(app, 1, 512)?;
        }
        if input.participants.len() > 64 {
            return Err(DataError::Invalid("participants"));
        }
        for p in &input.participants {
            bounded_text(p, 1, 256)?;
        }
        let start_hash = fingerprint(input, 65_536)?;
        let fence_hash = input.fence.hash()?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // An exact lost-ACK prepare retry is allowed after heartbeat expiry.
        // Expiry still forbids creating a different capture below.
        let worker = authenticate(&tx, peer_pid, current_epoch, proof, true)?;
        if !worker.capabilities.contains(&WorkerCapability::Capture) {
            return Err(DataError::Forbidden);
        }
        match lease_on(&tx, input.capture_id) {
            Ok(lease) => {
                capture_identity_authority(&tx, &worker, &lease, &input.fence)?;
                let (saved, state): (String, String) = tx.query_row(
                    "SELECT start_hash,state FROM service_capture_leases WHERE id=?1",
                    [input.capture_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if !matches!(state.as_str(), "reserved" | "capturing") {
                    return Err(DataError::Busy);
                }
                if saved != start_hash {
                    return Err(DataError::IdempotencyConflict);
                }
                capture_authority(&tx, &worker, &lease, &input.fence)?;
                return Ok(lease);
            }
            Err(DataError::NotFound) => {}
            Err(error) => return Err(error),
        }
        if worker.expires_at <= Utc::now() {
            return Err(DataError::Forbidden);
        }
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM service_capture_leases WHERE state IN ('reserved','capturing'))",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(DataError::Busy);
        }
        let id = input.meeting_id.to_string();
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
            [&id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(DataError::Busy);
        }
        tx.execute("INSERT INTO meetings(id,title,app,started_at,status,created_at,participants_json) VALUES(?1,?2,?3,?4,'recording',?5,?6)",params![id,input.title,input.app,input.started_at,Utc::now(),serde_json::to_string(&input.participants)?])?;
        let (_metadata, generation) = meetings::initialize_meeting_on(&tx, &id, "recorded")?;
        tx.execute("INSERT INTO service_capture_leases(id,meeting_id,worker_session_id,library_epoch,generation,fence_hash,start_hash,provider_json,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'reserved',?9)",params![input.capture_id.to_string(),id,proof.session_id.to_string(),current_epoch.to_string(),generation,fence_hash,start_hash,serde_json::to_string(&input.provider)?,Utc::now()])?;
        let mut projection = serde_json::json!({
            "provider": input.provider.provider,
            "model": input.provider.model,
            "language": input.provider.language,
            "captions": [],
            "capture_pending": true,
        });
        if input.provider.provider == CaptureProvider::Soniox {
            projection["status"] = "streaming".into();
        }
        tx.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)",
            params![format!("stt:{id}"), serde_json::to_string(&projection)?],
        )?;
        search::rebuild_meeting_on(&tx, &id)?;
        consistency::change(
            &tx,
            "meeting_lifecycle",
            &id,
            &generation,
            &format!("worker:{}", proof.session_id),
        )?;
        let lease = lease_on(&tx, input.capture_id)?;
        tx.commit()?;
        Ok(lease)
    }
    /// The core has verified that this peer currently holds the exact artifact
    /// guard. Writers may start only after this durable transition is ACKed.
    pub fn service_begin_capture(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
    ) -> DataResult<CaptureLease> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let worker = authenticate(&tx, peer_pid, current_epoch, proof, true)?;
        if !worker.capabilities.contains(&WorkerCapability::Capture) {
            return Err(DataError::Forbidden);
        }
        capture_identity_authority(&tx, &worker, lease, fence)?;
        let state: String = tx.query_row(
            "SELECT state FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |r| r.get(0),
        )?;
        if !matches!(state.as_str(), "reserved" | "capturing") {
            return Err(DataError::Busy);
        }
        capture_authority(&tx, &worker, lease, fence)?;
        if state == "reserved" {
            if worker.expires_at <= Utc::now() {
                return Err(DataError::Forbidden);
            }
            if tx.execute("UPDATE service_capture_leases SET state='capturing' WHERE id=?1 AND state='reserved'",[lease.capture_id.to_string()])? != 1 {
                return Err(DataError::Busy);
            }
        }
        tx.commit()?;
        Ok(lease.clone())
    }
    /// Narrow original-proof authority for abort/replay only. This does not
    /// authorize a new capture or any ordinary old-epoch worker mutation.
    pub fn service_abort_capture_context(
        &self,
        peer_pid: u32,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
    ) -> DataResult<()> {
        let conn = self.conn.lock();
        abort_identity_on(&conn, peer_pid, proof, lease, fence)?;
        let state: String = conn.query_row(
            "SELECT state FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |r| r.get(0),
        )?;
        if state != "reserved" {
            return Err(DataError::Busy);
        }
        Ok(())
    }
    pub fn service_abort_capture_receipt(
        &self,
        peer_pid: u32,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
    ) -> DataResult<Option<MutationOutcome>> {
        let conn = self.conn.lock();
        abort_identity_on(&conn, peer_pid, proof, lease, fence)?;
        let (state, outcome): (String, Option<String>) = conn.query_row(
            "SELECT state,outcome_json FROM service_capture_leases WHERE id=?1",
            [lease.capture_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if state != "aborted" {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(
            outcome
                .as_deref()
                .ok_or(DataError::Invalid("missing abort receipt"))?,
        )?))
    }
    /// Core holds an exclusive, pristine artifact guard through this commit.
    /// No capture file is deleted and no inference job is created.
    pub fn service_abort_capture(
        &self,
        peer_pid: u32,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
    ) -> DataResult<MutationOutcome> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        abort_identity_on(&tx, peer_pid, proof, lease, fence)?;
        let result = abort_on(&tx, lease, &format!("worker:{}", proof.session_id))?;
        tx.commit()?;
        Ok(result)
    }
    /// Startup reconciliation considers old-epoch reservations only. Current
    /// reservations are never cancelled by the monitor or heartbeat expiry.
    pub fn service_pending_reservations(
        &self,
        current_epoch: Uuid,
        limit: usize,
    ) -> DataResult<Vec<CaptureLease>> {
        valid_uuid(current_epoch)?;
        if !(1..=32).contains(&limit) {
            return Err(DataError::Invalid("pending reservation limit"));
        }
        let conn = self.conn.lock();
        let ids = {
            let mut st=conn.prepare("SELECT id FROM service_capture_leases WHERE state='reserved' AND library_epoch!=?1 ORDER BY created_at,id LIMIT ?2")?;
            let rows = st
                .query_map(params![current_epoch.to_string(), limit as i64], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        ids.into_iter()
            .map(|id| {
                lease_on(
                    &conn,
                    Uuid::parse_str(&id)
                        .map_err(|_| DataError::Invalid("stored capture identity"))?,
                )
            })
            .collect()
    }
    /// Internal only: caller holds the pristine OS artifact guard. Old epoch
    /// is a scheduling boundary; the monotone reserved state proves no begin.
    pub fn service_recover_reserved_capture(
        &self,
        current_epoch: Uuid,
        expected: &CaptureLease,
    ) -> DataResult<MutationOutcome> {
        valid_uuid(current_epoch)?;
        if expected.library_epoch == current_epoch {
            return Err(DataError::Forbidden);
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = abort_on(&tx, expected, &format!("service:startup:{current_epoch}"))?;
        tx.commit()?;
        Ok(result)
    }
    /// Core must first verify this same authenticated live worker owns the
    /// artifact identity. An expired heartbeat is not proof of an orphan.
    pub fn service_reattach_capture(
        &self,
        peer_pid: u32,
        new_epoch: Uuid,
        proof: &WorkerProof,
        capture_id: Uuid,
        fence: &WorkerSecret,
        ttl_seconds: i64,
    ) -> DataResult<CaptureLease> {
        let expiry = ttl(ttl_seconds)?;
        valid_uuid(new_epoch)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        reattach_context_on(&tx, peer_pid, new_epoch, proof, capture_id, fence)?;
        tx.execute(
            "UPDATE service_worker_sessions SET library_epoch=?2,expires_at=?3 WHERE id=?1",
            params![proof.session_id.to_string(), new_epoch.to_string(), expiry],
        )?;
        tx.execute(
            "UPDATE service_capture_leases SET library_epoch=?2 WHERE id=?1",
            params![capture_id.to_string(), new_epoch.to_string()],
        )?;
        let lease = lease_on(&tx, capture_id)?;
        tx.commit()?;
        Ok(lease)
    }
    /// Called after core verifies immutable terminal/journal evidence while
    /// holding the now-released raw artifact lock. No inference or I/O here.
    pub fn service_finalize_capture(
        &self,
        peer_pid: u32,
        current_epoch: Uuid,
        proof: &WorkerProof,
        lease: &CaptureLease,
        fence: &WorkerSecret,
        terminal: &CaptureTerminal,
    ) -> DataResult<MutationOutcome> {
        validate_terminal(terminal)?;
        let hash = fingerprint(terminal, CAPTURE_BYTES)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let worker = authenticate(&tx, peer_pid, current_epoch, proof, true)?;
        capture_identity_authority(&tx, &worker, lease, fence)?;
        let actor = format!("worker:{}", worker.session_id);
        let result = finalize_on(&tx, lease, terminal, &hash, &actor, false)?;
        tx.commit()?;
        Ok(result)
    }
    /// Service-owned startup scan. A returned row is only pending state; it does
    /// not prove its writer is dead and does not authorize recovery.
    pub fn service_pending_captures(&self, limit: usize) -> DataResult<Vec<CaptureLease>> {
        if !(1..=32).contains(&limit) {
            return Err(DataError::Invalid("pending capture limit"));
        }
        let conn = self.conn.lock();
        let ids = {
            let mut st=conn.prepare("SELECT id FROM service_capture_leases WHERE state='capturing' ORDER BY created_at,id LIMIT ?1")?;
            let ids = st
                .query_map([limit as i64], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        ids.into_iter()
            .map(|id| {
                lease_on(
                    &conn,
                    Uuid::parse_str(&id)
                        .map_err(|_| DataError::Invalid("stored capture identity"))?,
                )
            })
            .collect()
    }
    /// Internal service recovery, with no public RPC. Core must hold the
    /// acquired artifact guard and verify immutable identity/terminal/journal.
    /// No PID timeout or heartbeat is a substitute for that filesystem evidence.
    pub fn service_recover_capture(
        &self,
        current_epoch: Uuid,
        expected: &CaptureLease,
        terminal: &CaptureTerminal,
    ) -> DataResult<MutationOutcome> {
        self.recover_capture(current_epoch, expected, terminal, false)
    }

    /// Close a begun capture whose exact artifact owner has released its lock
    /// without publishing a terminal. Core retains that lock until commit.
    pub fn service_recover_interrupted_capture(
        &self,
        current_epoch: Uuid,
        expected: &CaptureLease,
        terminal: &CaptureTerminal,
    ) -> DataResult<MutationOutcome> {
        if terminal.status != CaptureTerminalStatus::Failed
            || terminal.has_video
            || terminal.raw_screen.is_some()
        {
            return Err(DataError::Invalid("interrupted capture terminal"));
        }
        self.recover_capture(current_epoch, expected, terminal, true)
    }

    fn recover_capture(
        &self,
        current_epoch: Uuid,
        expected: &CaptureLease,
        terminal: &CaptureTerminal,
        interrupted: bool,
    ) -> DataResult<MutationOutcome> {
        valid_uuid(current_epoch)?;
        validate_terminal(terminal)?;
        let hash = fingerprint(terminal, CAPTURE_BYTES)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = finalize_on(
            &tx,
            expected,
            terminal,
            &hash,
            &format!("service:recovery:{current_epoch}"),
            interrupted,
        )?;
        tx.commit()?;
        Ok(result)
    }
}

fn abort_identity_on(
    conn: &Connection,
    peer_pid: u32,
    proof: &WorkerProof,
    lease: &CaptureLease,
    fence: &WorkerSecret,
) -> DataResult<()> {
    let worker = authenticate(conn, peer_pid, proof.library_epoch, proof, true)?;
    if !worker.capabilities.contains(&WorkerCapability::Capture) {
        return Err(DataError::Forbidden);
    }
    capture_identity_authority(conn, &worker, lease, fence)
}

fn abort_on(
    tx: &Transaction<'_>,
    lease: &CaptureLease,
    actor: &str,
) -> DataResult<MutationOutcome> {
    if lease_on(tx, lease.capture_id)? != *lease {
        return Err(DataError::Forbidden);
    }
    let (state, saved): (String, Option<String>) = tx.query_row(
        "SELECT state,outcome_json FROM service_capture_leases WHERE id=?1",
        [lease.capture_id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if state == "aborted" {
        return Ok(serde_json::from_str(
            saved
                .as_deref()
                .ok_or(DataError::Invalid("missing abort receipt"))?,
        )?);
    }
    if state != "reserved" {
        return Err(DataError::Busy);
    }
    let id = lease.meeting_id.to_string();
    let (generation, tombstoned): (String, bool) = tx.query_row(
        "SELECT lifecycle_revision,tombstoned FROM service_meeting_state WHERE meeting_id=?1",
        [&id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if tombstoned {
        return Err(DataError::Forbidden);
    }
    check_revision(&generation, &lease.generation.to_string())?;
    let status: String = tx.query_row("SELECT status FROM meetings WHERE id=?1", [&id], |r| {
        r.get(0)
    })?;
    if status != "recording" {
        return Err(DataError::Forbidden);
    }
    tx.execute("UPDATE meetings SET status='failed',ended_at=started_at,error='CAPTURE_NOT_STARTED' WHERE id=?1",[&id])?;
    // Retain provider provenance and any other projection keys verbatim.
    tx.execute(
        "UPDATE settings SET value=CASE WHEN json_extract(value,'$.provider')='soniox'
         THEN json_set(value,'$.capture_pending',json('false'),'$.capture_aborted',json('true'),
                       '$.status','incomplete','$.error','Soniox transcription did not start.')
         ELSE json_set(value,'$.capture_pending',json('false'),'$.capture_aborted',json('true'))
         END WHERE key=?1",
        [format!("stt:{id}")],
    )?;
    let revision = revision();
    tx.execute(
        "UPDATE service_meeting_state SET lifecycle_revision=?2 WHERE meeting_id=?1",
        params![id, revision],
    )?;
    let ctx = MutationContext {
        principal: actor.into(),
        idempotency_key: format!("abort:{}", lease.capture_id),
    };
    let outcome = consistency::outcome(tx, "meeting_lifecycle", &id, &revision, &ctx)?;
    tx.execute("UPDATE service_capture_leases SET state='aborted',outcome_json=?2 WHERE id=?1 AND state='reserved'",params![lease.capture_id.to_string(),serde_json::to_string(&outcome)?])?;
    Ok(outcome)
}

fn finalize_on(
    tx: &Transaction<'_>,
    lease: &CaptureLease,
    terminal: &CaptureTerminal,
    hash: &str,
    actor: &str,
    interrupted: bool,
) -> DataResult<MutationOutcome> {
    if lease_on(tx, lease.capture_id)? != *lease {
        return Err(DataError::Forbidden);
    }
    let (state,provider,saved_hash,saved_outcome):(String,String,Option<String>,Option<String>)=tx.query_row("SELECT state,provider_json,finalize_hash,outcome_json FROM service_capture_leases WHERE id=?1",[lease.capture_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    if state == "finalized" {
        if saved_hash.as_deref() != Some(hash) {
            return Err(DataError::IdempotencyConflict);
        }
        return Ok(serde_json::from_str(
            saved_outcome
                .as_deref()
                .ok_or(DataError::Invalid("missing capture receipt"))?,
        )?);
    }
    if state != "capturing" {
        return Err(DataError::Busy);
    }
    let (current_generation, tombstoned): (String, bool) = tx.query_row(
        "SELECT lifecycle_revision,tombstoned FROM service_meeting_state WHERE meeting_id=?1",
        [lease.meeting_id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if tombstoned {
        return Err(DataError::Forbidden);
    }
    check_revision(&current_generation, &lease.generation.to_string())?;
    if serde_json::from_str::<CaptureSelection>(&provider)? != terminal.provider {
        return Err(DataError::Forbidden);
    }
    let id = lease.meeting_id.to_string();
    let (status, started_at): (String, DateTime<Utc>) = tx.query_row(
        "SELECT status,started_at FROM meetings WHERE id=?1",
        [&id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if status != "recording" || (!interrupted && terminal.ended_at < started_at) {
        return Err(DataError::Forbidden);
    }
    if let Some(screen) = &terminal.raw_screen {
        tx.execute(
            "INSERT INTO service_capture_terminal_screen(capture_id,evidence_json) VALUES(?1,?2)",
            params![lease.capture_id.to_string(), serde_json::to_string(screen)?],
        )?;
    }
    if let Some(outcome) = &terminal.soniox_outcome {
        tx.execute(
            "INSERT INTO service_capture_terminal_stt(capture_id,outcome_json) VALUES(?1,?2)",
            params![
                lease.capture_id.to_string(),
                serde_json::to_string(outcome)?
            ],
        )?;
    }
    publish_captions(tx, lease, terminal, actor, interrupted)?;
    let completed = terminal.status == CaptureTerminalStatus::Completed;
    let final_status = if completed { "processing" } else { "failed" };
    let failure = if interrupted {
        Some("Capture was interrupted. Available recording files were kept.")
    } else if !completed {
        Some("Capture did not finish successfully")
    } else {
        None
    };
    tx.execute("UPDATE meetings SET ended_at=?2,duration_ms=?3,has_video=?4,status=?5,error=?6 WHERE id=?1",params![id,terminal.ended_at.max(started_at),terminal.duration_ms,terminal.has_video,final_status,failure])?;
    // A failed recorder stop keeps partial evidence but does not queue the
    // automatic pipeline. The caller still receives a durable finalize ACK.
    let job = if completed {
        let transcript_revision: String = tx.query_row(
            "SELECT revision FROM service_transcript_state WHERE meeting_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let payload = serde_json::to_string(
            &serde_json::json!({"capture_id":lease.capture_id,"capture_generation":lease.generation,
            "transcript_revision":transcript_revision,"provider":terminal.provider,"required_capabilities":["post_process"]}),
        )?;
        let job = execution::insert_job(
            tx,
            "processing",
            Some(&id),
            "gui_worker",
            false,
            actor,
            &payload,
        )?;
        let job_rev: String = tx.query_row(
            "SELECT revision FROM service_jobs WHERE id=?1",
            [&job],
            |r| r.get(0),
        )?;
        consistency::change(tx, "job", &job, &job_rev, actor)?;
        Some(job)
    } else {
        None
    };
    let finalized_revision = revision();
    tx.execute(
        "UPDATE service_meeting_state SET lifecycle_revision=?2 WHERE meeting_id=?1",
        params![id, finalized_revision],
    )?;
    // Receipt replay occurs before generation checks: a lost ACK remains exact
    // after this first committed lifecycle transition or service-owned recovery.
    let ctx = MutationContext {
        principal: actor.into(),
        idempotency_key: format!("finalize:{}", lease.capture_id),
    };
    let mut result = consistency::outcome(tx, "meeting_lifecycle", &id, &finalized_revision, &ctx)?;
    result.job_id = job;
    tx.execute("UPDATE service_capture_leases SET state='finalized',last_confirmed_sequence=?2,finalize_hash=?3,outcome_json=?4,job_id=?5 WHERE id=?1",params![lease.capture_id.to_string(),terminal.last_confirmed_sequence as i64,hash,serde_json::to_string(&result)?,result.job_id])?;
    if let Some(job) = &result.job_id {
        super::postprocess_checkpoint::initialize_on(tx, job)?;
    }
    Ok(result)
}

fn validate_terminal(terminal: &CaptureTerminal) -> DataResult<()> {
    selection(&terminal.provider)?;
    if terminal.duration_ms < 0
        || terminal.duration_ms > 9_007_199_254_740_991
        || terminal.captions.len() > CAPTION_LIMIT
        || terminal.last_confirmed_sequence != terminal.captions.len() as u64
    {
        return Err(DataError::Invalid("capture terminal"));
    }
    if terminal
        .raw_screen
        .as_ref()
        .is_some_and(|screen| terminal.has_video || !screen.valid())
    {
        return Err(DataError::Invalid("raw screen evidence"));
    }
    let soniox = terminal.provider.provider == CaptureProvider::Soniox;
    if (terminal.soniox_outcome.is_some() && (!soniox || terminal.provider.model.is_none()))
        || (soniox && terminal.raw_screen.is_some() && terminal.soniox_outcome.is_none())
        || (terminal.retain_local_live && (soniox || terminal.captions.is_empty()))
    {
        return Err(DataError::Invalid("Soniox capture evidence"));
    }
    for (i, c) in terminal.captions.iter().enumerate() {
        if c.sequence != i as u64 + 1
            || c.start_ms < 0
            || c.end_ms < c.start_ms
            || (c.end_ms == c.start_ms && !terminal.retain_local_live)
            || c.end_ms > terminal.duration_ms
        {
            return Err(DataError::Invalid("confirmed caption sequence or timing"));
        }
        bounded_text(&c.text, 0, 65_536)?;
        if let Some(s) = &c.speaker {
            bounded_text(s, 1, 256)?;
        }
    }
    Ok(())
}
fn publish_captions(
    tx: &Transaction<'_>,
    lease: &CaptureLease,
    terminal: &CaptureTerminal,
    actor: &str,
    interrupted: bool,
) -> DataResult<()> {
    let id = lease.meeting_id.to_string();
    if !terminal.captions.is_empty()
        || (terminal.provider.provider == CaptureProvider::Soniox
            && terminal.soniox_outcome.is_some())
    {
        let mut raw = Vec::with_capacity(terminal.captions.len());
        for caption in &terminal.captions {
            tx.execute("INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text) VALUES(?1,?2,?3,?4,?5)",params![id,caption.start_ms,caption.end_ms,caption.speaker,caption.text])?;
            let segment_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO service_capture_captions VALUES(?1,?2,?3,?4)",
                params![
                    lease.capture_id.to_string(),
                    caption.sequence as i64,
                    segment_id,
                    fingerprint(caption, 131_072)?
                ],
            )?;
            raw.push(serde_json::json!({"id":segment_id,"start_ms":caption.start_ms,"end_ms":caption.end_ms,"speaker":caption.speaker,"text":caption.text}));
        }
        let source = revision();
        let rev = revision();
        let label = match terminal.provider.provider {
            CaptureProvider::Local if terminal.retain_local_live => "local_live",
            CaptureProvider::Local => "local",
            CaptureProvider::Soniox => "soniox",
        };
        tx.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,language,raw_json,created_at) VALUES(?1,?2,'captured',?3,?4,?5,?6)",params![source,id,label,terminal.provider.language,serde_json::to_string(&raw)?,Utc::now()])?;
        tx.execute("UPDATE service_transcript_state SET revision=?2,current_source_id=?3,document_state='present' WHERE meeting_id=?1",params![id,rev,source])?;
        crate::ai::rebuild_chunks_on(tx, &id)?;
        consistency::change(tx, "transcript", &id, &rev, actor)?;
    }
    tx.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![format!("stt:{id}"), stt_projection(terminal, interrupted)?],
    )?;
    search::rebuild_meeting_on(tx, &id)?;
    Ok(())
}

fn stt_projection(terminal: &CaptureTerminal, interrupted: bool) -> DataResult<String> {
    #[derive(Serialize)]
    struct Caption<'a> {
        #[serde(flatten)]
        evidence: &'a ConfirmedCaption,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_final: Option<bool>,
    }
    #[derive(Serialize)]
    struct Projection<'a> {
        provider: CaptureProvider,
        model: Option<&'a str>,
        language: Option<&'a str>,
        captions: Vec<Caption<'a>>,
        capture_pending: bool,
        last_confirmed_sequence: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    }
    let soniox = terminal.provider.provider == CaptureProvider::Soniox;
    let (status, error) = if soniox {
        match terminal.soniox_outcome {
            Some(SonioxCaptureOutcome::Complete {}) => (Some("complete"), None),
            Some(outcome) => (Some("incomplete"), outcome.diagnostic()),
            None => (
                Some("incomplete"),
                Some("Soniox completion was not recorded by this capture format."),
            ),
        }
    } else if interrupted {
        (Some("incomplete"), Some("Capture was interrupted."))
    } else if terminal.retain_local_live {
        (Some("complete"), None)
    } else {
        (None, None)
    };
    Ok(serde_json::to_string(&Projection {
        provider: terminal.provider.provider,
        model: terminal.provider.model.as_deref(),
        language: terminal.provider.language.as_deref(),
        captions: terminal
            .captions
            .iter()
            .map(|evidence| Caption {
                evidence,
                is_final: (soniox || terminal.retain_local_live).then_some(true),
            })
            .collect(),
        capture_pending: false,
        last_confirmed_sequence: terminal.last_confirmed_sequence,
        status,
        error,
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_domain::{
        capture_screen::RawScreenWriterOutcome, capture_stt::SonioxIncompleteReason,
    };
    fn fixture() -> Store {
        let store = Store::open_in_memory().unwrap();
        if !store
            .conn
            .lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='service_worker_sessions')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        {
            store
                .conn
                .lock()
                .execute_batch(include_str!("workers.sql"))
                .unwrap();
        }
        store
    }
    fn new_registration() -> WorkerRegistration {
        WorkerRegistration {
            session_id: Uuid::new_v4(),
            secret: WorkerSecret::random(),
            capabilities: vec![WorkerCapability::Capture],
        }
    }
    fn new_proof(r: &WorkerRegistration, epoch: Uuid) -> WorkerProof {
        WorkerProof {
            session_id: r.session_id,
            secret: r.secret.clone(),
            library_epoch: epoch,
        }
    }
    fn capture_start() -> CaptureStart {
        CaptureStart {
            capture_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            fence: WorkerSecret::random(),
            title: "Synthetic capture".into(),
            app: Some("fixture".into()),
            started_at: Utc::now(),
            participants: vec!["Sam".into()],
            provider: CaptureSelection {
                provider: CaptureProvider::Soniox,
                model: Some("fixture-soniox".into()),
                language: Some("ru".into()),
            },
        }
    }
    fn terminal(start: &CaptureStart) -> CaptureTerminal {
        CaptureTerminal {
            status: CaptureTerminalStatus::Completed,
            provider: start.provider.clone(),
            duration_ms: 2000,
            ended_at: Utc::now(),
            has_video: false,
            last_confirmed_sequence: 2,
            captions: vec![
                ConfirmedCaption {
                    sequence: 1,
                    start_ms: 0,
                    end_ms: 1000,
                    speaker: Some("Sam".into()),
                    text: "оригинал".into(),
                },
                ConfirmedCaption {
                    sequence: 2,
                    start_ms: 1000,
                    end_ms: 2000,
                    speaker: None,
                    text: "second exact raw".into(),
                },
            ],
            raw_screen: None,
            soniox_outcome: None,
            retain_local_live: false,
        }
    }
    fn prepared(
        store: &Store,
    ) -> (
        Uuid,
        WorkerRegistration,
        WorkerProof,
        CaptureStart,
        CaptureLease,
    ) {
        let epoch = Uuid::new_v4();
        let registration = new_registration();
        store
            .service_register_worker(42, epoch, &registration, 30)
            .unwrap();
        let proof = new_proof(&registration, epoch);
        let start = capture_start();
        let lease = store
            .service_prepare_capture(42, epoch, &proof, &start)
            .unwrap();
        // Store-only fixture: core/real-process tests separately require the
        // actual worker-owned OS guard before reaching this transition.
        store
            .service_begin_capture(42, epoch, &proof, &lease, &start.fence)
            .unwrap();
        (epoch, registration, proof, start, lease)
    }
    #[test]
    fn credentials_are_redacted_hashed_bound_to_pid_epoch_and_capabilities() {
        let store = fixture();
        let epoch = Uuid::new_v4();
        let registration = new_registration();
        let worker = store
            .service_register_worker(42, epoch, &registration, 30)
            .unwrap();
        assert_eq!(
            worker,
            store
                .service_register_worker(42, epoch, &registration, 30)
                .unwrap()
        );
        assert!(!format!("{registration:?}").contains(registration.secret.expose()));
        let hash: String = store
            .conn
            .lock()
            .query_row("SELECT secret_hash FROM service_worker_sessions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_ne!(hash, registration.secret.expose());
        let proof = new_proof(&registration, epoch);
        assert!(matches!(
            store.service_renew_worker(43, epoch, &proof, 30),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_renew_worker(42, Uuid::new_v4(), &proof, 30),
            Err(DataError::Forbidden)
        ));
        let wrong = WorkerProof {
            secret: WorkerSecret::random(),
            ..proof.clone()
        };
        assert!(matches!(
            store.service_renew_worker(42, epoch, &wrong, 30),
            Err(DataError::Forbidden)
        ));
        assert!(store.service_renew_worker(42, epoch, &proof, 30).is_ok());
        let duplicate = WorkerRegistration {
            capabilities: vec![WorkerCapability::Capture, WorkerCapability::Capture],
            ..registration.clone()
        };
        assert!(matches!(
            store.service_register_worker(42, epoch, &duplicate, 30),
            Err(DataError::Invalid(_))
        ));
        let no_capture = WorkerRegistration {
            capabilities: vec![WorkerCapability::PostProcess],
            ..new_registration()
        };
        store
            .service_register_worker(43, epoch, &no_capture, 30)
            .unwrap();
        assert!(matches!(
            store.service_prepare_capture(
                43,
                epoch,
                &new_proof(&no_capture, epoch),
                &capture_start()
            ),
            Err(DataError::Forbidden)
        ));
    }
    #[test]
    fn prepare_is_idempotent_and_no_heartbeat_expiry_claims_an_orphan() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        assert_eq!(
            lease,
            store
                .service_prepare_capture(42, epoch, &proof, &start)
                .unwrap()
        );
        let changed = CaptureStart {
            title: "changed".into(),
            ..start.clone()
        };
        assert!(matches!(
            store.service_prepare_capture(42, epoch, &proof, &changed),
            Err(DataError::IdempotencyConflict)
        ));
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        assert_eq!(
            lease,
            store
                .service_prepare_capture(42, epoch, &proof, &start)
                .unwrap()
        );
        assert!(matches!(
            store.service_prepare_capture(42, epoch, &proof, &capture_start()),
            Err(DataError::Forbidden)
        ));
        let another = new_registration();
        store
            .service_register_worker(43, epoch, &another, 30)
            .unwrap();
        assert!(matches!(
            store.service_prepare_capture(
                43,
                epoch,
                &new_proof(&another, epoch),
                &super::tests::capture_start()
            ),
            Err(DataError::Busy)
        ));
        let m = store
            .service_meeting(&lease.meeting_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(m.meeting.status, crate::MeetingStatus::Recording);
        assert!(matches!(
            store.service_delete_meeting(
                &MutationContext {
                    principal: "fixture".into(),
                    idempotency_key: "delete".into()
                },
                &lease.meeting_id.to_string(),
                &lease.generation.to_string()
            ),
            Err(DataError::Busy)
        ));
    }
    #[test]
    fn reattach_rotates_service_epoch_without_changing_raw_generation() {
        let store = fixture();
        let (old, _registration, proof, start, lease) = prepared(&store);
        let new = Uuid::new_v4();
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.service_reattach_capture(43, new, &proof, lease.capture_id, &start.fence, 30),
            Err(DataError::Forbidden)
        ));
        let attached = store
            .service_reattach_capture(42, new, &proof, lease.capture_id, &start.fence, 30)
            .unwrap();
        assert_eq!(attached.generation, lease.generation);
        assert_eq!(attached.library_epoch, new);
        assert!(matches!(
            store.service_finalize_capture(
                42,
                new,
                &proof,
                &lease,
                &start.fence,
                &terminal(&start)
            ),
            Err(DataError::Forbidden)
        ));
        let current = WorkerProof {
            library_epoch: new,
            ..proof
        };
        assert!(store
            .service_finalize_capture(
                42,
                new,
                &current,
                &attached,
                &start.fence,
                &terminal(&start)
            )
            .is_ok());
        assert_ne!(old, new);
    }
    #[test]
    fn finalize_lost_ack_replays_after_reopen_with_one_job_and_no_body_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bazza.db");
        let store = Store::open(&path).unwrap();
        if !store
            .conn
            .lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='service_worker_sessions')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        {
            store
                .conn
                .lock()
                .execute_batch(include_str!("workers.sql"))
                .unwrap();
        }
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        let terminal = terminal(&start);
        let first = store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
            .unwrap();
        assert_ne!(first.revision, lease.generation.to_string());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            first,
            store
                .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
                .unwrap()
        );
        assert_eq!(
            store
                .segments(&lease.meeting_id.to_string())
                .unwrap()
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            vec!["оригинал", "second exact raw"]
        );
        let conn = store.conn.lock();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM service_jobs WHERE kind='processing'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT state FROM service_jobs WHERE kind='processing'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "waiting_for_worker"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM service_capture_captions", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            2
        );
        let receipt: String = conn
            .query_row("SELECT outcome_json FROM service_capture_leases", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!receipt.contains("second exact raw"));
        assert!(!receipt.contains(start.fence.expose()));
        let origin: String = conn
            .query_row("SELECT origin FROM service_transcript_sources", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(origin, "captured");
    }
    #[test]
    fn terminal_conflicts_stale_fences_bad_sequences_and_provider_never_rewrite_raw() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        let good = terminal(&start);
        assert!(matches!(
            store.service_finalize_capture(
                42,
                epoch,
                &proof,
                &lease,
                &WorkerSecret::random(),
                &good
            ),
            Err(DataError::Forbidden)
        ));
        let mut wrong = good.clone();
        wrong.captions[1].sequence = 1;
        assert!(matches!(
            store.service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &wrong),
            Err(DataError::Invalid(_))
        ));
        let mut wrong = good.clone();
        wrong.provider.provider = CaptureProvider::Local;
        assert!(matches!(
            store.service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &wrong),
            Err(DataError::Forbidden)
        ));
        assert!(store
            .segments(&lease.meeting_id.to_string())
            .unwrap()
            .is_empty());
        store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &good)
            .unwrap();
        let mut conflict = good.clone();
        conflict.captions[0].text = "different".into();
        assert!(matches!(
            store.service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &conflict),
            Err(DataError::IdempotencyConflict)
        ));
        assert_eq!(
            store.segments(&lease.meeting_id.to_string()).unwrap()[0].text,
            "оригинал"
        );
    }
    #[test]
    fn finalize_failure_rolls_back_all_captions_source_jobs_projection_and_receipt() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        let mut good = terminal(&start);
        good.raw_screen = Some(RawScreenEvidence {
            writer: RawScreenWriterOutcome::Failed,
            raw_mp4_bytes: Some(0),
            timeline_bytes: None,
            video_clock_bytes: None,
            audio_clock_bytes: Some(128),
        });
        assert!(matches!(
            store.service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &good),
            Err(DataError::Invalid("Soniox capture evidence"))
        ));
        good.soniox_outcome = Some(SonioxCaptureOutcome::Complete {});
        store.conn.lock().execute_batch("CREATE TRIGGER inject_finalize BEFORE INSERT ON service_jobs BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
        assert!(store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &good)
            .is_err());
        assert!(store
            .segments(&lease.meeting_id.to_string())
            .unwrap()
            .is_empty());
        let conn = store.conn.lock();
        for table in [
            "service_capture_captions",
            "service_capture_terminal_stt",
            "service_capture_terminal_screen",
            "service_transcript_sources",
            "service_jobs",
        ] {
            assert_eq!(
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            conn.query_row("SELECT state FROM service_capture_leases", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "capturing"
        );
        drop(conn);
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER inject_finalize;")
            .unwrap();
        assert!(store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &good)
            .is_ok());
    }
    #[test]
    fn successful_media_retains_explicit_incomplete_soniox_and_confirmed_text() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        let id = lease.meeting_id.to_string();
        let key = format!("stt:{id}");
        let initial: serde_json::Value = store.get_setting(&key).unwrap().unwrap();
        assert_eq!(initial["status"], "streaming");

        let mut terminal = terminal(&start);
        let stopped = SonioxCaptureOutcome::Incomplete {
            reason: SonioxIncompleteReason::PermissionRevoked,
        };
        terminal.soniox_outcome = Some(stopped);
        let receipt = store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
            .unwrap();
        assert!(receipt.job_id.is_some());
        let projection: serde_json::Value = store.get_setting(&key).unwrap().unwrap();
        assert_eq!(projection["provider"], "soniox");
        assert_eq!(projection["model"], "fixture-soniox");
        assert_eq!(projection["language"], "ru");
        assert_eq!(projection["status"], "incomplete");
        assert_eq!(projection["error"], stopped.diagnostic().unwrap());
        assert_eq!(projection["captions"][0]["is_final"], true);
        assert_eq!(projection["captions"][0]["text"], "оригинал");
        assert_eq!(store.segments(&id).unwrap()[1].text, "second exact raw");

        assert_eq!(
            store
                .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
                .unwrap(),
            receipt
        );
        terminal.soniox_outcome = Some(SonioxCaptureOutcome::Complete {});
        assert!(matches!(
            store.service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal),
            Err(DataError::IdempotencyConflict)
        ));
        let evidence: String = store
            .conn
            .lock()
            .query_row(
                "SELECT outcome_json FROM service_capture_terminal_stt WHERE capture_id=?1",
                [lease.capture_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<SonioxCaptureOutcome>(&evidence).unwrap(),
            stopped
        );
    }

    #[test]
    fn successful_empty_soniox_is_distinct_from_missing_legacy_evidence() {
        for outcome in [None, Some(SonioxCaptureOutcome::Complete {})] {
            let store = fixture();
            let (epoch, _registration, proof, start, lease) = prepared(&store);
            let mut terminal = terminal(&start);
            terminal.captions.clear();
            terminal.last_confirmed_sequence = 0;
            terminal.soniox_outcome = outcome;
            let encoded = serde_json::to_value(&terminal).unwrap();
            assert!(encoded.get("raw_screen").is_none());
            assert_eq!(encoded.get("soniox_outcome").is_some(), outcome.is_some());
            store
                .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
                .unwrap();
            let projection: serde_json::Value = store
                .get_setting(&format!("stt:{}", lease.meeting_id))
                .unwrap()
                .unwrap();
            assert_eq!(projection["captions"], serde_json::json!([]));
            if outcome.is_some() {
                assert_eq!(projection["status"], "complete");
                assert!(projection["error"].is_null());
            } else {
                assert_eq!(projection["status"], "incomplete");
                assert!(projection["error"].is_string());
            }
        }
    }
    #[test]
    fn changed_lifecycle_generation_blocks_uncommitted_finalize() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_meeting_state SET lifecycle_revision=?2 WHERE meeting_id=?1",
                params![lease.meeting_id.to_string(), revision()],
            )
            .unwrap();
        assert!(matches!(
            store.service_finalize_capture(
                42,
                epoch,
                &proof,
                &lease,
                &start.fence,
                &terminal(&start)
            ),
            Err(DataError::Conflict { .. })
        ));
        assert!(store
            .segments(&lease.meeting_id.to_string())
            .unwrap()
            .is_empty());
    }
    #[test]
    fn empty_soniox_failed_handoff_keeps_provider_without_a_job() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        let terminal = CaptureTerminal {
            status: CaptureTerminalStatus::Failed,
            provider: start.provider.clone(),
            duration_ms: 0,
            ended_at: Utc::now(),
            has_video: false,
            last_confirmed_sequence: 0,
            captions: vec![],
            raw_screen: None,
            soniox_outcome: None,
            retain_local_live: false,
        };
        let out = store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
            .unwrap();
        assert!(out.job_id.is_none());
        assert_eq!(
            store
                .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
                .unwrap(),
            out
        );
        let job_count: i64 = store
            .conn
            .lock()
            .query_row(
                "SELECT count(*) FROM service_jobs WHERE entity_id=?1",
                [lease.meeting_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(job_count, 0);
        assert!(store
            .service_changes(0, 100)
            .unwrap()
            .iter()
            .all(|change| change.entity_kind != "job"));
        assert_eq!(
            store
                .service_meeting(&lease.meeting_id.to_string())
                .unwrap()
                .unwrap()
                .meeting
                .status,
            crate::MeetingStatus::Failed
        );
        let provenance: serde_json::Value = store
            .get_setting(&format!("stt:{}", lease.meeting_id))
            .unwrap()
            .unwrap();
        assert_eq!(provenance["provider"], "soniox");
        assert_eq!(provenance["model"], "fixture-soniox");
        assert!(store
            .segments(&lease.meeting_id.to_string())
            .unwrap()
            .is_empty());
    }
    #[test]
    fn context_authenticates_before_artifacts_and_remains_readable_after_finalize() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        assert!(matches!(
            store.service_capture_context(43, epoch, &proof, lease.capture_id, &start.fence),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_capture_context(
                42,
                epoch,
                &proof,
                lease.capture_id,
                &WorkerSecret::random()
            ),
            Err(DataError::Forbidden)
        ));
        let context = store
            .service_capture_context(42, epoch, &proof, lease.capture_id, &start.fence)
            .unwrap();
        assert_eq!(context.0, lease);
        assert_eq!(context.1, start.provider);
        store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal(&start))
            .unwrap();
        assert_eq!(
            store
                .service_capture_context(42, epoch, &proof, lease.capture_id, &start.fence)
                .unwrap(),
            context
        );
    }
    #[test]
    fn pending_rows_do_not_recover_until_trusted_service_supplies_verified_terminal() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        store
            .conn
            .lock()
            .execute(
                "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        assert_eq!(
            store.service_pending_captures(32).unwrap(),
            vec![lease.clone()]
        );
        assert_eq!(
            store
                .service_meeting(&lease.meeting_id.to_string())
                .unwrap()
                .unwrap()
                .meeting
                .status,
            crate::MeetingStatus::Recording
        );
        assert!(matches!(
            store.service_pending_captures(0),
            Err(DataError::Invalid(_))
        ));
        // The Store API is internal: parent core separately acquires the raw
        // artifact guard and verifies identity/terminal/journal before this call.
        let terminal = terminal(&start);
        let recovered = store
            .service_recover_capture(Uuid::new_v4(), &lease, &terminal)
            .unwrap();
        assert!(store.service_pending_captures(32).unwrap().is_empty());
        assert_eq!(
            recovered,
            store
                .service_recover_capture(Uuid::new_v4(), &lease, &terminal)
                .unwrap()
        );
        assert_eq!(
            recovered,
            store
                .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
                .unwrap()
        );
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_jobs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let payload: String = store
            .conn
            .lock()
            .query_row("SELECT payload_json FROM service_jobs", [], |r| r.get(0))
            .unwrap();
        assert!(payload.contains("post_process"));
        assert!(!payload.contains("second exact raw"));
    }
    #[test]
    fn purged_capture_keeps_identity_and_late_prepare_cannot_resurrect_meeting() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("bazza.db")).unwrap();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        let terminal = terminal(&start);
        let finalized = store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal)
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET status='ready' WHERE id=?1",
                [lease.meeting_id.to_string()],
            )
            .unwrap();
        let deleted = store
            .service_delete_meeting(
                &MutationContext {
                    principal: "fixture-admin".into(),
                    idempotency_key: "delete-capture".into(),
                },
                &lease.meeting_id.to_string(),
                &finalized.revision,
            )
            .unwrap();
        assert!(store
            .service_run_purge(deleted.job_id.as_deref().unwrap())
            .unwrap());
        assert!(store
            .get_meeting(&lease.meeting_id.to_string())
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_capture_leases", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(matches!(
            store.service_prepare_capture(42, epoch, &proof, &start),
            Err(DataError::Busy)
        ));
        assert!(store
            .get_meeting(&lease.meeting_id.to_string())
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .service_capture_receipt(42, &proof, &lease, &start.fence)
                .unwrap(),
            Some(finalized)
        );
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_transcript_sources", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn readonly_receipt_replays_old_epoch_only_for_authenticated_finalized_identity() {
        let store = fixture();
        let (epoch, _registration, proof, start, lease) = prepared(&store);
        assert!(store
            .service_capture_receipt(42, &proof, &lease, &start.fence)
            .unwrap()
            .is_none());
        let finalized = store
            .service_finalize_capture(42, epoch, &proof, &lease, &start.fence, &terminal(&start))
            .unwrap();
        let new_service_epoch = Uuid::new_v4();
        assert!(matches!(
            store.service_finalize_capture(
                42,
                new_service_epoch,
                &proof,
                &lease,
                &start.fence,
                &terminal(&start)
            ),
            Err(DataError::Forbidden)
        ));
        assert_eq!(
            store
                .service_capture_receipt(42, &proof, &lease, &start.fence)
                .unwrap(),
            Some(finalized)
        );
        assert!(matches!(
            store.service_capture_receipt(43, &proof, &lease, &start.fence),
            Err(DataError::Forbidden)
        ));
        let wrong_proof = WorkerProof {
            secret: WorkerSecret::random(),
            ..proof.clone()
        };
        assert!(matches!(
            store.service_capture_receipt(42, &wrong_proof, &lease, &start.fence),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_capture_receipt(42, &proof, &lease, &WorkerSecret::random()),
            Err(DataError::Forbidden)
        ));
        let wrong_lease = CaptureLease {
            generation: Uuid::new_v4(),
            ..lease.clone()
        };
        assert!(matches!(
            store.service_capture_receipt(42, &proof, &wrong_lease, &start.fence),
            Err(DataError::Forbidden)
        ));
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_jobs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn invalid_model_or_language_cannot_reserve_the_only_capture_slot() {
        let store = fixture();
        let epoch = Uuid::new_v4();
        let registration = new_registration();
        store
            .service_register_worker(42, epoch, &registration, 30)
            .unwrap();
        let proof = new_proof(&registration, epoch);
        for model in [
            String::new(),
            "_leading".into(),
            "model/path".into(),
            "machine:C".into(),
            "кириллица".into(),
            "x".repeat(129),
        ] {
            let mut start = capture_start();
            start.provider.model = Some(model);
            assert!(matches!(
                store.service_prepare_capture(42, epoch, &proof, &start),
                Err(DataError::Invalid("capture model identifier"))
            ));
        }
        for language in ["e", "1en", "en_US", "en-", "en-123456789"] {
            let mut start = capture_start();
            start.provider.language = Some(language.into());
            assert!(matches!(
                store.service_prepare_capture(42, epoch, &proof, &start),
                Err(DataError::Invalid(_))
            ));
        }
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_capture_leases", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM meetings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let mut valid = capture_start();
        valid.provider.model = Some("Model.v1_q8+cpu-stable".into());
        valid.provider.language = Some("zh-Hant-TW".into());
        assert!(store
            .service_prepare_capture(42, epoch, &proof, &valid)
            .is_ok());
    }
    #[test]
    fn reattach_lost_ack_retries_old_proof_without_granting_old_epoch_mutations() {
        let store = fixture();
        let (_old_epoch, _registration, proof, start, lease) = prepared(&store);
        let current_epoch = Uuid::new_v4();
        assert_eq!(
            store
                .service_reattach_context(42, current_epoch, &proof, lease.capture_id, &start.fence)
                .unwrap()
                .0,
            lease
        );
        let attached = store
            .service_reattach_capture(
                42,
                current_epoch,
                &proof,
                lease.capture_id,
                &start.fence,
                30,
            )
            .unwrap();
        assert_eq!(attached.generation, lease.generation);
        assert_eq!(
            store
                .service_reattach_context(42, current_epoch, &proof, lease.capture_id, &start.fence)
                .unwrap()
                .0,
            attached
        );
        assert_eq!(
            store
                .service_reattach_capture(
                    42,
                    current_epoch,
                    &proof,
                    lease.capture_id,
                    &start.fence,
                    30
                )
                .unwrap(),
            attached
        );
        assert!(matches!(
            store.service_renew_worker(42, current_epoch, &proof, 30),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_prepare_capture(42, current_epoch, &proof, &capture_start()),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_reattach_context(
                43,
                current_epoch,
                &proof,
                lease.capture_id,
                &start.fence
            ),
            Err(DataError::Forbidden)
        ));
        let wrong = WorkerProof {
            secret: WorkerSecret::random(),
            ..proof.clone()
        };
        assert!(matches!(
            store.service_reattach_capture(
                42,
                current_epoch,
                &wrong,
                lease.capture_id,
                &start.fence,
                30
            ),
            Err(DataError::Forbidden)
        ));
        assert!(matches!(
            store.service_reattach_context(
                42,
                current_epoch,
                &proof,
                lease.capture_id,
                &WorkerSecret::random()
            ),
            Err(DataError::Forbidden)
        ));
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM service_capture_leases", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

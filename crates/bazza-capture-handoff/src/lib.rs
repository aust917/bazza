//! Capture-worker artifact ownership and crash-safe terminal handoff.
//!
//! These primitives do not start capture or recovery. The caller supplies an
//! explicit existing recording directory and retains the guard until raw writers
//! have closed/joined. Recovery must acquire this same OS lock, never infer its
//! absence from a heartbeat. All I/O belongs on a worker, not a capture callback.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub mod media_read;
mod owner_proof;

pub use bazza_domain::{
    capture_screen::{RawScreenEvidence, RawScreenWriterOutcome},
    capture_stt::{SonioxCaptureOutcome, SonioxIncompleteReason},
};

pub const MANIFEST_VERSION: u16 = 1;
pub const EVIDENCE_MANIFEST_VERSION: u16 = 2;
const LIVE_EVIDENCE_MANIFEST_VERSION: u16 = 3;
pub const MAX_MANIFEST_BYTES: usize = 16_384;
pub const MAX_FRAME_BYTES: usize = 65_536;
pub const MAX_TEXT_BYTES: usize = 32_768;
pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_RECORDS: usize = 200_000;
pub const LOCK_FILE: &str = ".bazza-capture-artifact.lock";
pub const JOURNAL_FILE: &str = ".bazza-confirmed-stt-v1.journal";
pub const TERMINAL_FILE: &str = ".bazza-capture-terminal-v1.json";
pub const OWNER_FILE: &str = ".bazza-capture-owner-v1.json";
const MAGIC: &[u8; 8] = b"BZCAPJ1\n";
const HEADER_BYTES: usize = 8 + 4 * 16;
const MAX_TIME_MS: u64 = 9_007_199_254_740_991;
const RAW_SCREEN_MP4: &str = "screen.raw.mp4";
const RAW_SCREEN_TIMELINE: &str = "screen.raw.timeline.json";
const RAW_SCREEN_CLOCK: &str = "screen.raw.clock.txt";
const MIX_CLOCK: &str = "mix.clock.json";

#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("capture artifacts have a live OS owner")]
    Busy,
    #[error("invalid capture handoff")]
    Invalid,
    #[error("capture handoff belongs to a different identity")]
    IdentityMismatch,
    #[error("confirmed STT journal is corrupt")]
    CorruptJournal,
    #[error("confirmed sequence conflicts with a previous record")]
    SequenceConflict,
    #[error("capture is already sealed")]
    Sealed,
    #[error("capture terminal is missing")]
    MissingTerminal,
    #[error("capture handoff exceeds its byte or record budget")]
    TooLarge,
    #[error("capture artifact is a link or an unsupported file")]
    UnsafePath,
    #[error("capture artifact I/O: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, HandoffError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureIdentity {
    pub library_id: Uuid,
    pub meeting_id: Uuid,
    pub worker_session_id: Uuid,
    pub generation: Uuid,
}
impl CaptureIdentity {
    fn validate(&self) -> Result<()> {
        if [
            self.library_id,
            self.meeting_id,
            self.worker_session_id,
            self.generation,
        ]
        .iter()
        .any(Uuid::is_nil)
        {
            return Err(HandoffError::Invalid);
        }
        Ok(())
    }
    fn header(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_BYTES);
        bytes.extend_from_slice(MAGIC);
        for id in [
            self.library_id,
            self.meeting_id,
            self.worker_session_id,
            self.generation,
        ] {
            bytes.extend_from_slice(id.as_bytes());
        }
        bytes
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    Local,
    Soniox,
}

/// Safe model identity only: never a credential, URL or local model path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderProvenance {
    pub provider: SttProvider,
    pub model_id: Option<String>,
}
impl ProviderProvenance {
    fn validate(&self) -> Result<()> {
        if let Some(id) = &self.model_id {
            if id.is_empty()
                || id.len() > 128
                || !id.as_bytes()[0].is_ascii_alphanumeric()
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
            {
                return Err(HandoffError::Invalid);
            }
        }
        Ok(())
    }
}

/// One confirmed provider segment. Sequence IDs are scoped to CaptureIdentity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedCaption {
    pub sequence: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
    pub provenance: ProviderProvenance,
}
impl ConfirmedCaption {
    fn validate(&self) -> Result<()> {
        if self.sequence == 0
            || self.end_ms < self.start_ms
            || (self.end_ms == self.start_ms && self.provenance.provider != SttProvider::Local)
            || self.end_ms > MAX_TIME_MS
        {
            return Err(HandoffError::Invalid);
        }
        bounded_text(&self.text, MAX_TEXT_BYTES)?;
        if let Some(speaker) = &self.speaker {
            bounded_text(speaker, 256)?;
        }
        self.provenance.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JournalAck {
    pub sequence: u64,
    pub digest: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTerminalStatus {
    Completed,
    Failed,
}

fn collect_raw_screen(
    directory: &Path,
    writer: RawScreenWriterOutcome,
) -> Result<RawScreenEvidence> {
    Ok(RawScreenEvidence {
        writer,
        raw_mp4_bytes: raw_screen_file(directory, RAW_SCREEN_MP4, true)?,
        timeline_bytes: raw_screen_file(directory, RAW_SCREEN_TIMELINE, true)?,
        video_clock_bytes: raw_screen_file(directory, RAW_SCREEN_CLOCK, true)?,
        audio_clock_bytes: raw_screen_file(directory, MIX_CLOCK, true)?,
    })
}

fn verify_raw_screen(directory: &Path, evidence: &RawScreenEvidence) -> Result<()> {
    for (name, size) in [
        (RAW_SCREEN_MP4, evidence.raw_mp4_bytes),
        (RAW_SCREEN_TIMELINE, evidence.timeline_bytes),
        (RAW_SCREEN_CLOCK, evidence.video_clock_bytes),
        (MIX_CLOCK, evidence.audio_clock_bytes),
    ] {
        if raw_screen_file(directory, name, false)? != size {
            return Err(HandoffError::Invalid);
        }
    }
    Ok(())
}

fn raw_screen_file(directory: &Path, name: &str, sync: bool) -> Result<Option<u64>> {
    let path = directory.join(name);
    match regular_file(&path) {
        Ok(()) => {}
        Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    let file = OpenOptions::new().read(true).write(sync).open(path)?;
    let bytes = file.metadata()?.len();
    if sync {
        file.sync_all()?;
    }
    Ok(Some(bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TerminalManifest {
    pub version: u16,
    pub identity: CaptureIdentity,
    pub provenance: ProviderProvenance,
    pub duration_ms: u64,
    pub ended_at: chrono::DateTime<chrono::Utc>,
    pub status: CaptureTerminalStatus,
    pub error_code: Option<String>,
    pub has_audio: bool,
    pub has_video: bool,
    pub last_confirmed_sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EvidenceTerminal {
    version: u16,
    capture: TerminalManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_screen: Option<RawScreenEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    soniox_outcome: Option<SonioxCaptureOutcome>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    retain_local_live: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum TerminalDocument {
    Capture(TerminalManifest),
    Evidence(EvidenceTerminal),
}

impl TerminalDocument {
    fn manifest(&self) -> &TerminalManifest {
        match self {
            Self::Capture(manifest) => manifest,
            Self::Evidence(terminal) => &terminal.capture,
        }
    }

    fn raw_screen(&self) -> Option<&RawScreenEvidence> {
        match self {
            Self::Capture(_) => None,
            Self::Evidence(terminal) => terminal.raw_screen.as_ref(),
        }
    }

    fn soniox_outcome(&self) -> Option<&SonioxCaptureOutcome> {
        match self {
            Self::Capture(_) => None,
            Self::Evidence(terminal) => terminal.soniox_outcome.as_ref(),
        }
    }

    fn retain_local_live(&self) -> bool {
        matches!(self, Self::Evidence(terminal) if terminal.retain_local_live)
    }

    fn validate(&self, identity: &CaptureIdentity) -> Result<()> {
        validate_terminal_shape(self.manifest(), identity)?;
        if let Self::Evidence(terminal) = self {
            let version = if terminal.retain_local_live {
                LIVE_EVIDENCE_MANIFEST_VERSION
            } else {
                EVIDENCE_MANIFEST_VERSION
            };
            if terminal.version != version {
                return Err(HandoffError::Invalid);
            }
            validate_evidence_admission(
                &terminal.capture,
                terminal.raw_screen.is_some(),
                terminal.soniox_outcome.is_some(),
                terminal.retain_local_live,
            )?;
            if terminal
                .raw_screen
                .as_ref()
                .is_some_and(|screen| !screen.valid())
            {
                return Err(HandoffError::TooLarge);
            }
        }
        Ok(())
    }
}

fn validate_evidence_admission(
    capture: &TerminalManifest,
    raw_screen: bool,
    soniox: bool,
    retain_local_live: bool,
) -> Result<()> {
    if !(raw_screen || soniox || retain_local_live)
        || (capture.provenance.provider == SttProvider::Soniox) != soniox
        || (soniox && capture.provenance.model_id.is_none())
        || (capture.has_video && raw_screen)
        || (retain_local_live && (soniox || capture.last_confirmed_sequence == 0))
    {
        return Err(HandoffError::Invalid);
    }
    Ok(())
}

/// Read a sealed capture's bounded terminal shape without touching its journal,
/// owner metadata or locks. Full journal verification already happened during
/// finalization; the caller must compare stored last_confirmed_sequence and
/// provider, retain its media gate, and recheck current repository authority.
pub fn read_terminal_snapshot(
    directory: &Path,
    identity: &CaptureIdentity,
) -> Result<TerminalManifest> {
    Ok(read_terminal_snapshot_with_evidence(directory, identity)?.manifest)
}

/// Bounded terminal evidence. Raw files described here may have been consumed
/// by a later mux. This snapshot does not recheck files or prove playback.
#[derive(Debug, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub manifest: TerminalManifest,
    pub raw_screen: Option<RawScreenEvidence>,
    pub soniox_outcome: Option<SonioxCaptureOutcome>,
}

pub fn read_terminal_snapshot_with_evidence(
    directory: &Path,
    identity: &CaptureIdentity,
) -> Result<TerminalSnapshot> {
    identity.validate()?;
    let directory = checked_directory(directory, identity)?;
    let manifest = read_terminal(&directory.join(TERMINAL_FILE))?.ok_or_else(|| {
        HandoffError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "capture terminal is missing",
        ))
    })?;
    manifest.validate(identity)?;
    if !(0..=MAX_TIME_MS as i64).contains(&manifest.manifest().ended_at.timestamp_millis()) {
        return Err(HandoffError::Invalid);
    }
    Ok(match manifest {
        TerminalDocument::Capture(manifest) => TerminalSnapshot {
            manifest,
            raw_screen: None,
            soniox_outcome: None,
        },
        TerminalDocument::Evidence(terminal) => TerminalSnapshot {
            manifest: terminal.capture,
            raw_screen: terminal.raw_screen,
            soniox_outcome: terminal.soniox_outcome,
        },
    })
}

fn validate_terminal_shape(manifest: &TerminalManifest, identity: &CaptureIdentity) -> Result<()> {
    if manifest.identity != *identity {
        return Err(HandoffError::IdentityMismatch);
    }
    manifest.provenance.validate()?;
    if manifest.version != MANIFEST_VERSION
        || manifest.duration_ms > MAX_TIME_MS
        || manifest.last_confirmed_sequence > MAX_RECORDS as u64
        || (manifest.status == CaptureTerminalStatus::Completed
            && (!manifest.has_audio || manifest.error_code.is_some()))
        || manifest.error_code.as_ref().is_some_and(|code| {
            code.is_empty()
                || code.len() > 64
                || !code
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        })
    {
        return Err(HandoffError::Invalid);
    }
    Ok(())
}

struct Journal {
    records: Vec<ConfirmedCaption>,
    digests: Vec<[u8; 32]>,
    valid_bytes: u64,
    incomplete_tail: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OwnerMetadata {
    version: u16,
    identity: CaptureIdentity,
    owner_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proof_nonce: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proof_endpoint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerState {
    Free,
    Busy { owner_pid: u32 },
}

/// Service-side reader of a sealed handoff under the writer's existing lock.
/// Holding this guard through publication excludes every cooperating writer.
/// Acquisition creates no files and does not advertise a capture worker.
pub struct SealedCaptureGuard {
    directory: PathBuf,
    terminal: TerminalDocument,
    _owner: File,
}

/// Recovery must keep the same lock from the first inspection through the
/// database commit, including when the old writer left no terminal seal.
pub enum CaptureRecoveryGuard {
    Sealed(SealedCaptureGuard),
    Interrupted(InterruptedCaptureGuard),
}

pub struct InterruptedCaptureGuard {
    directory: PathBuf,
    identity: CaptureIdentity,
    _owner: File,
}

impl CaptureRecoveryGuard {
    pub fn acquire(directory: &Path, identity: &CaptureIdentity) -> Result<Self> {
        identity.validate()?;
        let directory = checked_directory(directory, identity)?;
        let owner = lock_artifacts(&directory, false)?;
        if let Some(terminal) = read_terminal(&directory.join(TERMINAL_FILE))? {
            terminal.validate(identity)?;
            return Ok(Self::Sealed(SealedCaptureGuard {
                directory,
                terminal,
                _owner: owner,
            }));
        }
        let last_owner = read_owner_metadata(&directory)?;
        if last_owner.identity != *identity {
            return Err(HandoffError::IdentityMismatch);
        }
        Ok(Self::Interrupted(InterruptedCaptureGuard {
            directory,
            identity: identity.clone(),
            _owner: owner,
        }))
    }
}

impl InterruptedCaptureGuard {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Visit complete, identity-bound journal frames. An interrupted write can
    /// leave a torn tail. Leave it intact and publish only the valid prefix.
    pub fn scan_confirmed<E: From<HandoffError>>(
        &self,
        mut visit: impl FnMut(ConfirmedCaption) -> std::result::Result<bool, E>,
    ) -> std::result::Result<(), E> {
        if let Some(mut frames) =
            JournalFrames::open(&self.directory.join(JOURNAL_FILE), &self.identity)?
        {
            while let Some((caption, _)) = frames.next_record()? {
                if !visit(caption)? {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl SealedCaptureGuard {
    pub fn acquire(directory: &Path, identity: &CaptureIdentity) -> Result<Self> {
        identity.validate()?;
        let directory = checked_directory(directory, identity)?;
        let owner = lock_artifacts(&directory, false)?;
        let terminal =
            read_terminal(&directory.join(TERMINAL_FILE))?.ok_or(HandoffError::MissingTerminal)?;
        terminal.validate(identity)?;
        Ok(Self {
            directory,
            terminal,
            _owner: owner,
        })
    }

    pub fn terminal(&self) -> &TerminalManifest {
        self.terminal.manifest()
    }

    pub fn raw_screen_evidence(&self) -> Option<&RawScreenEvidence> {
        self.terminal.raw_screen()
    }

    pub fn soniox_outcome(&self) -> Option<&SonioxCaptureOutcome> {
        self.terminal.soniox_outcome()
    }

    pub fn retain_local_live(&self) -> bool {
        self.terminal.retain_local_live()
    }

    /// Compare a version-2 seal to the fixed files while retaining ownership.
    /// An older seal has no raw screen evidence to verify.
    pub fn verify_raw_screen_evidence(&self) -> Result<()> {
        if let Some(evidence) = self.raw_screen_evidence() {
            verify_raw_screen(&self.directory, evidence)?;
        }
        Ok(())
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Visit confirmed captions with one frame buffered at a time. The caller
    /// must discard tentative results on error. A successful scan has checked
    /// the entire journal against the terminal seal, including EOF.
    pub fn scan_confirmed<E: From<HandoffError>>(
        &self,
        mut visit: impl FnMut(ConfirmedCaption) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let terminal = self.terminal();
        let Some(mut frames) =
            JournalFrames::open(&self.directory.join(JOURNAL_FILE), &terminal.identity)?
        else {
            if terminal.last_confirmed_sequence == 0 {
                return Ok(());
            }
            return Err(HandoffError::Invalid.into());
        };
        while let Some((caption, _)) = frames.next_record()? {
            if caption.sequence > terminal.last_confirmed_sequence
                || caption.end_ms > terminal.duration_ms
                || caption.provenance != terminal.provenance
            {
                return Err(HandoffError::Invalid.into());
            }
            visit(caption)?;
        }
        frames.check_eof()?;
        if frames.records != terminal.last_confirmed_sequence {
            return Err(HandoffError::Invalid.into());
        }
        Ok(())
    }
}

/// Non-cloneable lifetime artifact owner. Drop it only after all raw writers
/// have closed/joined and terminal publication has completed (or failed safely).
/// The lock file is persistent and must never be unlinked/replaced by callers.
pub struct ArtifactGuard {
    directory: PathBuf,
    identity: CaptureIdentity,
    journal: Journal,
    rollback_live_tail: bool,
    terminal: Option<TerminalDocument>,
    // Its Drop stops/joins every proof connection before _owner unlocks.
    owner_proof: Option<owner_proof::ProofServer>,
    // Retained through journal/manifest I/O and dropped last.
    _owner: File,
}
impl ArtifactGuard {
    /// Creates no recording directory. The explicit existing leaf must equal
    /// the canonical meeting UUID. This primitive has no default library lookup.
    pub fn acquire(directory: &Path, identity: &CaptureIdentity) -> Result<Self> {
        identity.validate()?;
        let directory = checked_directory(directory, identity)?;
        let owner = lock_artifacts(&directory, true)?;
        let journal = read_journal(&directory.join(JOURNAL_FILE), identity)?;
        let terminal = read_terminal(&directory.join(TERMINAL_FILE))?;
        let mut guard = Self {
            directory,
            identity: identity.clone(),
            journal,
            rollback_live_tail: false,
            terminal,
            owner_proof: None,
            _owner: owner,
        };
        if let Some(terminal) = &guard.terminal {
            terminal.validate(&guard.identity)?;
            guard.validate_terminal(terminal.manifest())?;
        }
        let nonce = Uuid::new_v4();
        #[cfg(any(windows, target_os = "macos"))]
        {
            guard.owner_proof = Some(owner_proof::ProofServer::start(
                &guard.directory,
                identity,
                nonce,
            )?);
        }
        let metadata = OwnerMetadata {
            version: 1,
            identity: identity.clone(),
            owner_pid: std::process::id(),
            proof_nonce: guard.owner_proof.as_ref().map(|_| nonce),
            proof_endpoint: guard
                .owner_proof
                .as_ref()
                .map(|proof| proof.endpoint.clone()),
        };
        publish_file(
            &guard.directory,
            &guard.directory.join(OWNER_FILE),
            &bounded_json(&metadata, 2048)?,
            true,
        )?;
        Ok(guard)
    }

    /// Read-only discovery probe. The returned PID is an untrusted metadata
    /// hint, not proof of lock ownership. Use verify_owner to authorize begin
    /// or reattach; recovery instead acquires the exclusive artifact lock.
    pub fn probe(directory: &Path, identity: &CaptureIdentity) -> Result<OwnerState> {
        identity.validate()?;
        let directory = checked_directory(directory, identity)?;
        let path = directory.join(LOCK_FILE);
        match regular_file(&path) {
            Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(OwnerState::Free)
            }
            value => value?,
        }
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(OwnerState::Free),
            Err(TryLockError::WouldBlock) => {
                let owner = read_owner_metadata(&directory)?;
                if owner.identity != *identity {
                    return Err(HandoffError::IdentityMismatch);
                }
                Ok(OwnerState::Busy {
                    owner_pid: owner.owner_pid,
                })
            }
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// Verify a live guard in the exact OS-authenticated worker process. Owner
    /// metadata is only a bounded rendezvous: the endpoint is independently
    /// derived from this canonical root and nonce, then the transport verifies
    /// the kernel server PID/current user and a fresh typed challenge is echoed.
    /// The caller must retain its guard through the begin/reattach ACK. This
    /// blocking control-path operation must never run on a capture callback.
    pub fn verify_owner(
        directory: &Path,
        identity: &CaptureIdentity,
        expected_pid: u32,
    ) -> Result<()> {
        Self::verify_owner_state(directory, identity, expected_pid, false)
    }

    /// Beginning raw writers requires both the live OS-authenticated owner and
    /// its server-observed absence of a terminal seal. This is distinct from
    /// control reattach, which remains possible after a capture has sealed.
    pub fn verify_begin_owner(
        directory: &Path,
        identity: &CaptureIdentity,
        expected_pid: u32,
    ) -> Result<()> {
        Self::verify_owner_state(directory, identity, expected_pid, true)
    }

    fn verify_owner_state(
        directory: &Path,
        identity: &CaptureIdentity,
        expected_pid: u32,
        require_unsealed: bool,
    ) -> Result<()> {
        identity.validate()?;
        if expected_pid == 0 {
            return Err(HandoffError::Invalid);
        }
        let directory = checked_directory(directory, identity)?;
        if !artifact_lock_busy(&directory)? {
            return Err(HandoffError::Busy);
        }
        let metadata = read_owner_metadata(&directory)?;
        if metadata.identity != *identity {
            return Err(HandoffError::IdentityMismatch);
        }
        let nonce = metadata.proof_nonce.ok_or(HandoffError::Invalid)?;
        let endpoint = metadata.proof_endpoint.ok_or(HandoffError::Invalid)?;
        if endpoint != owner_proof::endpoint(&directory, nonce)? {
            return Err(HandoffError::Invalid);
        }
        owner_proof::verify(
            endpoint,
            identity.clone(),
            nonce,
            expected_pid,
            require_unsealed,
        )?;
        if !artifact_lock_busy(&directory)? {
            return Err(HandoffError::Busy);
        }
        Ok(())
    }

    pub fn identity(&self) -> &CaptureIdentity {
        &self.identity
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn confirmed(&self) -> &[ConfirmedCaption] {
        &self.journal.records
    }
    pub fn last_sequence(&self) -> u64 {
        self.journal.records.len() as u64
    }
    /// Only an incomplete final frame is recoverable. Complete corrupt records
    /// fail acquisition; accepted records are never silently skipped.
    pub fn has_incomplete_tail(&self) -> bool {
        self.journal.incomplete_tail
    }
    pub fn terminal(&self) -> Option<&TerminalManifest> {
        self.terminal.as_ref().map(TerminalDocument::manifest)
    }

    /// ACK is returned only after the complete frame has been sync_all'd. Retry
    /// of the same sequence/payload returns its original ACK; gaps or changes fail.
    pub fn append_confirmed(&mut self, caption: &ConfirmedCaption) -> Result<JournalAck> {
        self.append_caption(caption, false)
    }

    /// A local live row is displayed only after its durable ACK. Do not recover
    /// a failed append as a displayed row on the next append or terminal seal.
    pub fn append_local_live_confirmed(
        &mut self,
        caption: &ConfirmedCaption,
    ) -> Result<JournalAck> {
        if caption.provenance.provider != SttProvider::Local {
            return Err(HandoffError::Invalid);
        }
        self.append_caption(caption, true)
    }

    fn append_caption(
        &mut self,
        caption: &ConfirmedCaption,
        local_live: bool,
    ) -> Result<JournalAck> {
        caption.validate()?;
        if self.terminal.is_some() {
            return Err(HandoffError::Sealed);
        }
        self.discard_failed_live_tail()?;
        self.reconcile_unacknowledged_tail()?;
        if caption.sequence <= self.last_sequence() {
            let index = (caption.sequence - 1) as usize;
            return if self.journal.records[index] == *caption {
                // A complete frame recovered after a write/sync failure is
                // accepted only after this retry has made it durable.
                OpenOptions::new()
                    .write(true)
                    .open(self.directory.join(JOURNAL_FILE))?
                    .sync_all()?;
                Ok(JournalAck {
                    sequence: caption.sequence,
                    digest: hex(&self.journal.digests[index]),
                })
            } else {
                Err(HandoffError::SequenceConflict)
            };
        }
        if caption.sequence != self.last_sequence() + 1 {
            return Err(HandoffError::SequenceConflict);
        }
        if self.journal.records.len() == MAX_RECORDS {
            return Err(HandoffError::TooLarge);
        }
        if self
            .journal
            .records
            .first()
            .is_some_and(|first| first.provenance != caption.provenance)
        {
            return Err(HandoffError::Invalid);
        }
        let body = bounded_json(caption, MAX_FRAME_BYTES)?;
        let previous = self
            .journal
            .digests
            .last()
            .copied()
            .unwrap_or_else(|| Sha256::digest(self.identity.header()).into());
        let digest = frame_digest(&previous, &body);
        let length = body.len() as u32;
        let mut frame = Vec::with_capacity(body.len() + 36);
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&body);
        frame.extend_from_slice(&digest);
        let next_len = self.journal.valid_bytes + frame.len() as u64;
        if next_len > MAX_JOURNAL_BYTES {
            return Err(HandoffError::TooLarge);
        }
        let path = self.directory.join(JOURNAL_FILE);
        if !path.try_exists()? {
            publish_new(&self.directory, &path, &self.identity.header())?;
        }
        regular_file(&path)?;
        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        // Repair only the unacknowledged trailing frame, under the same OS owner.
        if file.metadata()?.len() != self.journal.valid_bytes {
            file.set_len(self.journal.valid_bytes)?;
        }
        file.seek(SeekFrom::Start(self.journal.valid_bytes))?;
        self.rollback_live_tail = local_live;
        file.write_all(&frame)?;
        file.sync_all()?;
        self.rollback_live_tail = false;
        self.journal.records.push(caption.clone());
        self.journal.digests.push(digest);
        self.journal.valid_bytes = next_len;
        self.journal.incomplete_tail = false;
        Ok(JournalAck {
            sequence: caption.sequence,
            digest: hex(&digest),
        })
    }

    /// Caller must first stop/join writers. Fixed declared artifacts are synced
    /// before the versioned terminal manifest is atomically published. A retry
    /// must have exactly the same manifest; no terminal file is overwritten.
    pub fn publish_terminal(&mut self, manifest: &TerminalManifest) -> Result<()> {
        self.publish_terminal_document(manifest, None, None, false)
    }

    /// Seal capture with raw screen evidence, a Soniox outcome, or both.
    /// Stop and join all writers first. Raw screen evidence is independent of
    /// the common manifest's published video flag and audio outcome.
    pub fn publish_terminal_with_evidence(
        &mut self,
        manifest: &TerminalManifest,
        screen_writer: Option<RawScreenWriterOutcome>,
        soniox_outcome: Option<SonioxCaptureOutcome>,
    ) -> Result<()> {
        validate_evidence_admission(
            manifest,
            screen_writer.is_some(),
            soniox_outcome.is_some(),
            false,
        )?;
        self.publish_terminal_document(manifest, screen_writer, soniox_outcome, false)
    }

    /// Seal the nonempty, displayed local transcript as the capture's initial source.
    pub fn publish_terminal_with_local_live(
        &mut self,
        manifest: &TerminalManifest,
        screen_writer: Option<RawScreenWriterOutcome>,
    ) -> Result<()> {
        validate_evidence_admission(manifest, screen_writer.is_some(), false, true)?;
        self.publish_terminal_document(manifest, screen_writer, None, true)
    }

    fn publish_terminal_document(
        &mut self,
        manifest: &TerminalManifest,
        screen_writer: Option<RawScreenWriterOutcome>,
        soniox_outcome: Option<SonioxCaptureOutcome>,
        retain_local_live: bool,
    ) -> Result<()> {
        self.discard_failed_live_tail()?;
        self.reconcile_unacknowledged_tail()?;
        self.validate_terminal(manifest)?;
        if let Some(existing) = &self.terminal {
            return if existing.manifest() == manifest
                && existing.raw_screen().map(|screen| screen.writer) == screen_writer
                && existing.soniox_outcome() == soniox_outcome.as_ref()
                && existing.retain_local_live() == retain_local_live
            {
                Ok(())
            } else {
                Err(HandoffError::SequenceConflict)
            };
        }
        if self.directory.join(JOURNAL_FILE).try_exists()? {
            // The terminal seal describes only confirmed complete frames.
            let path = self.directory.join(JOURNAL_FILE);
            regular_file(&path)?;
            let file = OpenOptions::new().write(true).open(path)?;
            if self.journal.incomplete_tail {
                file.set_len(self.journal.valid_bytes)?;
            }
            file.sync_all()?;
            self.journal.incomplete_tail = false;
        }
        for (present, name) in [
            (manifest.has_audio, "mix.wav"),
            (manifest.has_video, "screen.mp4"),
        ] {
            if present {
                let path = self.directory.join(name);
                regular_file(&path)?;
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)?
                    .sync_all()?;
            }
        }
        let terminal = if screen_writer.is_some() || soniox_outcome.is_some() || retain_local_live {
            TerminalDocument::Evidence(EvidenceTerminal {
                version: if retain_local_live {
                    LIVE_EVIDENCE_MANIFEST_VERSION
                } else {
                    EVIDENCE_MANIFEST_VERSION
                },
                capture: manifest.clone(),
                raw_screen: screen_writer
                    .map(|writer| collect_raw_screen(&self.directory, writer))
                    .transpose()?,
                soniox_outcome,
                retain_local_live,
            })
        } else {
            TerminalDocument::Capture(manifest.clone())
        };
        terminal.validate(&self.identity)?;
        let path = self.directory.join(TERMINAL_FILE);
        let bytes = bounded_json(&terminal, MAX_MANIFEST_BYTES)?;
        match publish_new(&self.directory, &path, &bytes) {
            Ok(()) => {}
            Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_terminal(&path)?.as_ref() != Some(&terminal) {
                    return Err(HandoffError::SequenceConflict);
                }
            }
            Err(error) => return Err(error),
        }
        self.terminal = Some(terminal);
        Ok(())
    }

    fn discard_failed_live_tail(&mut self) -> Result<()> {
        if !self.rollback_live_tail {
            return Ok(());
        }
        let path = self.directory.join(JOURNAL_FILE);
        let mut frames =
            JournalFrames::open(&path, &self.identity)?.ok_or(HandoffError::CorruptJournal)?;
        // Validate exactly the acknowledged prefix without interpreting any
        // bytes from the failed write as a confirmed frame.
        for (caption, digest) in self.journal.records.iter().zip(&self.journal.digests) {
            let (saved, saved_digest) =
                frames.next_record()?.ok_or(HandoffError::CorruptJournal)?;
            if &saved != caption || &saved_digest != digest {
                return Err(HandoffError::CorruptJournal);
            }
        }
        if frames.valid_bytes != self.journal.valid_bytes {
            return Err(HandoffError::CorruptJournal);
        }
        drop(frames);
        let file = OpenOptions::new().write(true).open(&path)?;
        file.set_len(self.journal.valid_bytes)?;
        file.sync_all()?;
        self.journal.incomplete_tail = false;
        self.rollback_live_tail = false;
        Ok(())
    }

    fn reconcile_unacknowledged_tail(&mut self) -> Result<()> {
        let path = self.directory.join(JOURNAL_FILE);
        match regular_file(&path) {
            Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if !self.journal.records.is_empty() {
                    return Err(HandoffError::CorruptJournal);
                }
                return Ok(());
            }
            value => value?,
        }
        if fs::metadata(&path)?.len() != self.journal.valid_bytes {
            let recovered = read_journal(&path, &self.identity)?;
            let saved = self.journal.records.len();
            if recovered.records.len() < saved
                || recovered.records[..saved] != self.journal.records
                || recovered.digests[..saved] != self.journal.digests
            {
                return Err(HandoffError::CorruptJournal);
            }
            self.journal = recovered;
        }
        Ok(())
    }

    fn validate_terminal(&self, manifest: &TerminalManifest) -> Result<()> {
        validate_terminal_shape(manifest, &self.identity)?;
        if manifest.last_confirmed_sequence != self.last_sequence()
            || self.journal.records.iter().any(|record| {
                record.provenance != manifest.provenance || record.end_ms > manifest.duration_ms
            })
        {
            return Err(HandoffError::Invalid);
        }
        Ok(())
    }
}

impl Drop for ArtifactGuard {
    fn drop(&mut self) {
        // Explicit order, including partial acquire/publication failures: a
        // proof can never remain live after another process acquires this lock.
        drop(self.owner_proof.take());
    }
}

fn artifact_lock_busy(directory: &Path) -> Result<bool> {
    let path = directory.join(LOCK_FILE);
    regular_file(&path)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn read_owner_metadata(directory: &Path) -> Result<OwnerMetadata> {
    let path = directory.join(OWNER_FILE);
    regular_file(&path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(2049).read_to_end(&mut bytes)?;
    if bytes.len() > 2048 {
        return Err(HandoffError::TooLarge);
    }
    let owner: OwnerMetadata = serde_json::from_slice(&bytes).map_err(|_| HandoffError::Invalid)?;
    if owner.version != 1 || owner.owner_pid == 0 {
        return Err(HandoffError::Invalid);
    }
    Ok(owner)
}

fn lock_artifacts(directory: &Path, create: bool) -> Result<File> {
    let path = directory.join(LOCK_FILE);
    if create {
        regular_if_present(&path)?;
    } else {
        regular_file(&path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(HandoffError::Busy),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn bounded_text(text: &str, max: usize) -> Result<()> {
    if text.len() > max {
        return Err(HandoffError::TooLarge);
    }
    if text.contains('\0') {
        return Err(HandoffError::Invalid);
    }
    Ok(())
}
fn checked_directory(directory: &Path, identity: &CaptureIdentity) -> Result<PathBuf> {
    let metadata = regular_metadata(directory)?;
    if !metadata.is_dir()
        || directory.file_name().and_then(|s| s.to_str())
            != Some(identity.meeting_id.to_string().as_str())
    {
        return Err(HandoffError::UnsafePath);
    }
    Ok(directory.canonicalize()?)
}
fn bounded_json(value: &impl Serialize, max: usize) -> Result<Vec<u8>> {
    struct Writer {
        bytes: Vec<u8>,
        max: usize,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.max {
                return Err(std::io::Error::other("capture byte budget"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        bytes: Vec::new(),
        max,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| HandoffError::TooLarge)?;
    Ok(writer.bytes)
}
fn regular_metadata(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(HandoffError::UnsafePath);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(HandoffError::UnsafePath);
        }
    }
    Ok(metadata)
}
fn regular_file(path: &Path) -> Result<()> {
    if !regular_metadata(path)?.is_file() {
        return Err(HandoffError::UnsafePath);
    }
    Ok(())
}
fn regular_if_present(path: &Path) -> Result<()> {
    match regular_file(path) {
        Ok(()) => Ok(()),
        Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
fn read_terminal(path: &Path) -> Result<Option<TerminalDocument>> {
    match regular_file(path) {
        Ok(()) => {}
        Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        }
        Err(error) => return Err(error),
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(HandoffError::TooLarge);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| HandoffError::Invalid)
}
fn read_journal(path: &Path, identity: &CaptureIdentity) -> Result<Journal> {
    let mut journal = Journal {
        records: Vec::new(),
        digests: Vec::new(),
        valid_bytes: HEADER_BYTES as u64,
        incomplete_tail: false,
    };
    if let Some(mut frames) = JournalFrames::open(path, identity)? {
        while let Some((caption, digest)) = frames.next_record()? {
            journal.records.push(caption);
            journal.digests.push(digest);
        }
        journal.valid_bytes = frames.valid_bytes;
        journal.incomplete_tail = frames.incomplete_tail;
    }
    Ok(journal)
}

struct JournalFrames {
    file: BufReader<File>,
    length: u64,
    valid_bytes: u64,
    records: u64,
    previous: [u8; 32],
    provenance: Option<ProviderProvenance>,
    body: Vec<u8>,
    incomplete_tail: bool,
}

impl JournalFrames {
    fn open(path: &Path, identity: &CaptureIdentity) -> Result<Option<Self>> {
        match regular_file(path) {
            Ok(()) => {}
            Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        }
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        if length > MAX_JOURNAL_BYTES {
            return Err(HandoffError::TooLarge);
        }
        let mut file = BufReader::new(file);
        let mut header = [0u8; HEADER_BYTES];
        file.read_exact(&mut header).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                HandoffError::CorruptJournal
            } else {
                error.into()
            }
        })?;
        if header[..8] != MAGIC[..] {
            return Err(HandoffError::CorruptJournal);
        }
        if header != identity.header()[..] {
            return Err(HandoffError::IdentityMismatch);
        }
        Ok(Some(Self {
            file,
            length,
            valid_bytes: HEADER_BYTES as u64,
            records: 0,
            previous: Sha256::digest(header).into(),
            provenance: None,
            body: Vec::with_capacity(MAX_FRAME_BYTES),
            incomplete_tail: false,
        }))
    }

    fn next_record(&mut self) -> Result<Option<(ConfirmedCaption, [u8; 32])>> {
        let remaining = self.length - self.valid_bytes;
        if remaining == 0 || self.incomplete_tail {
            return Ok(None);
        }
        if remaining < 4 {
            self.incomplete_tail = true;
            return Ok(None);
        }
        let mut prefix = [0u8; 4];
        self.file.read_exact(&mut prefix)?;
        let size = u32::from_be_bytes(prefix) as usize;
        if size == 0 || size > MAX_FRAME_BYTES {
            return Err(HandoffError::CorruptJournal);
        }
        if remaining - 4 < (size + 32) as u64 {
            self.incomplete_tail = true;
            return Ok(None);
        }
        self.body.resize(size, 0);
        self.file.read_exact(&mut self.body)?;
        let mut digest = [0u8; 32];
        self.file.read_exact(&mut digest)?;
        if digest != frame_digest(&self.previous, &self.body) {
            return Err(HandoffError::CorruptJournal);
        }
        let caption: ConfirmedCaption =
            serde_json::from_slice(&self.body).map_err(|_| HandoffError::CorruptJournal)?;
        caption
            .validate()
            .map_err(|_| HandoffError::CorruptJournal)?;
        if caption.sequence != self.records + 1
            || self
                .provenance
                .as_ref()
                .is_some_and(|first| *first != caption.provenance)
        {
            return Err(HandoffError::CorruptJournal);
        }
        if self.records == MAX_RECORDS as u64 {
            return Err(HandoffError::TooLarge);
        }
        self.provenance
            .get_or_insert_with(|| caption.provenance.clone());
        self.valid_bytes += (size + 36) as u64;
        self.records += 1;
        self.previous = digest;
        Ok(Some((caption, digest)))
    }

    fn check_eof(&mut self) -> Result<()> {
        let mut extra = [0u8; 1];
        if self.incomplete_tail
            || self.valid_bytes != self.length
            || self.file.get_ref().metadata()?.len() != self.length
            || self.file.read(&mut extra)? != 0
        {
            return Err(HandoffError::CorruptJournal);
        }
        Ok(())
    }
}
fn frame_digest(previous: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(previous);
    hash.update((body.len() as u32).to_be_bytes());
    hash.update(body);
    hash.finalize().into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn publish_new(directory: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    publish_file(directory, path, bytes, false)
}

/// Install a synced file at a new name in the same directory. The caller owns
/// path admission, staging contents and any needed replay receipt.
pub fn install_synced_media(source: &Path, target: &Path) -> std::io::Result<()> {
    let directory = source.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    if target.parent() != Some(directory) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    #[cfg(windows)]
    move_file(source, target, false)?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = CString::new(source.as_os_str().as_bytes())?;
        let target = CString::new(target.as_os_str().as_bytes())?;
        // Both strings are NUL-terminated, and the native exclusive rename
        // never overwrites an existing leaf or creates a second hard link.
        #[cfg(target_os = "macos")]
        let result =
            unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        File::open(directory)?.sync_all()?;
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    return Err(std::io::ErrorKind::Unsupported.into());
    Ok(())
}

/// Open an ordinary, single-link stage or installed video without following a
/// reparse point/symlink. The caller checks the containing directory first.
pub fn open_video_output(path: &Path, writable: bool) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(writable);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() || !single_video_link(&file)? {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(file)
}

#[cfg(windows)]
fn single_video_link(file: &File) -> std::io::Result<bool> {
    use std::{
        mem::MaybeUninit,
        os::windows::{fs::MetadataExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    if file.metadata()?.file_attributes() & 0x400 != 0 {
        return Ok(false);
    }
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.assume_init() }.nNumberOfLinks == 1)
}

#[cfg(unix)]
fn single_video_link(file: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink() == 1)
}

#[cfg(not(any(windows, unix)))]
fn single_video_link(_: &File) -> std::io::Result<bool> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(windows)]
fn move_file(source: &Path, target: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), flags) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn publish_file(directory: &Path, path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    regular_if_present(path)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    #[cfg(windows)]
    move_file(temporary.path(), path, replace)?;
    #[cfg(not(windows))]
    {
        if replace {
            temporary.persist(path).map_err(|error| error.error)?;
        } else {
            temporary
                .persist_noclobber(path)
                .map_err(|error| error.error)?;
        }
        #[cfg(unix)]
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod local_live_tests {
    use super::*;

    #[test]
    fn failed_local_live_tail_never_becomes_part_of_the_frozen_seal() {
        let temp = tempfile::tempdir().unwrap();
        let mirror = tempfile::tempdir().unwrap();
        let identity = CaptureIdentity {
            library_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            worker_session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        };
        let provenance = ProviderProvenance {
            provider: SttProvider::Local,
            model_id: None,
        };
        let row = |sequence| ConfirmedCaption {
            sequence,
            start_ms: 0,
            end_ms: sequence,
            speaker: None,
            text: format!("Local live row {sequence}"),
            provenance: provenance.clone(),
        };
        let open = |root: &Path| {
            let directory = root.join(identity.meeting_id.to_string());
            fs::create_dir(&directory).unwrap();
            ArtifactGuard::acquire(&directory, &identity).unwrap()
        };
        let mut guard = open(temp.path());
        guard.append_local_live_confirmed(&row(1)).unwrap();
        let journal = guard.directory().join(JOURNAL_FILE);
        let acknowledged = fs::read(&journal).unwrap();
        let mut sample = open(mirror.path());
        sample.append_local_live_confirmed(&row(1)).unwrap();
        sample.append_local_live_confirmed(&row(2)).unwrap();
        let full = fs::read(sample.directory().join(JOURNAL_FILE)).unwrap();
        assert!(full.starts_with(&acknowledged));

        // First the write returns with only part of the frame. The retry
        // discards it, then the second write leaves a complete frame but no ACK.
        fs::write(&journal, &full[..acknowledged.len() + 2]).unwrap();
        guard.rollback_live_tail = true;
        guard.discard_failed_live_tail().unwrap();
        assert_eq!(fs::read(&journal).unwrap(), acknowledged);
        fs::write(&journal, &full).unwrap();
        guard.rollback_live_tail = true;
        let manifest = TerminalManifest {
            version: MANIFEST_VERSION,
            identity: identity.clone(),
            provenance: provenance.clone(),
            duration_ms: 1,
            ended_at: chrono::Utc::now(),
            status: CaptureTerminalStatus::Failed,
            error_code: None,
            has_audio: false,
            has_video: false,
            last_confirmed_sequence: guard.last_sequence(),
        };
        guard
            .publish_terminal_with_local_live(&manifest, None)
            .unwrap();
        guard
            .publish_terminal_with_local_live(&manifest, None)
            .unwrap();
        let directory = guard.directory().to_path_buf();
        drop(guard);
        let sealed = SealedCaptureGuard::acquire(&directory, &identity).unwrap();
        let mut rows = Vec::new();
        sealed
            .scan_confirmed::<HandoffError>(|row| {
                rows.push(row);
                Ok(())
            })
            .unwrap();
        assert_eq!(rows, vec![row(1)]);
        assert_eq!(fs::read(journal).unwrap(), acknowledged);
    }
}

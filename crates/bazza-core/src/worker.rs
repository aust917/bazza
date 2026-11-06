//! Capture worker orchestration. OS peer identity comes from transport;
//! terminal data comes from locked durable artifacts, never a finalize DTO.
#[cfg(test)]
#[path = "worker_start_tests.rs"]
mod start_tests;
use crate::{CoreError, Repository};
use bazza_capture_handoff::{
    ArtifactGuard, CaptureIdentity, CaptureRecoveryGuard, HandoffError, InterruptedCaptureGuard,
    OwnerState, SealedCaptureGuard,
};
use bazza_service_api::{
    soniox_permission::{
        SonioxPermissionOperation, SonioxPermissionReply, SonioxPermissionRequest,
    },
    worker as wire,
};
use bazza_store::service_data as domain;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FINALIZE_RECEIPT_WAIT: Duration = Duration::from_millis(500);
const FINALIZE_RECEIPT_POLL: Duration = Duration::from_millis(25);
const MAX_FINALIZE_CAPTIONS: u64 = 10_000;
const MAX_FINALIZE_TEXT_BYTES: usize = 8 * 1024 * 1024;

fn convert<T: Serialize, D: DeserializeOwned>(value: &T) -> Result<D, CoreError> {
    // Both sides are closed, typed private contracts. No arbitrary invoke or
    // Store/SQLite dependency is exposed to the client by this adapter.
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}
fn handoff(error: HandoffError) -> CoreError {
    match error {
        HandoffError::Busy => domain::DataError::Busy.into(),
        HandoffError::MissingTerminal => {
            domain::DataError::Invalid("terminal manifest missing").into()
        }
        other => CoreError::Handoff(other),
    }
}
fn regular(path: &Path, dir: bool) -> Result<(), CoreError> {
    let meta = path.symlink_metadata()?;
    let mut unsafe_path =
        meta.file_type().is_symlink() || (dir && !meta.is_dir()) || (!dir && !meta.is_file());
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        unsafe_path |= meta.file_attributes() & 0x400 != 0;
    }
    if unsafe_path {
        return Err(domain::DataError::Forbidden.into());
    }
    Ok(())
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct HandoffRecovery {
    pub aborted: usize,
    pub recovered: usize,
    pub active: usize,
    pub pending: usize,
    pub invalid: usize,
}

impl Repository {
    pub fn soniox_permission(
        &self,
        peer_pid: u32,
        request: SonioxPermissionRequest,
    ) -> Result<SonioxPermissionReply, CoreError> {
        if !request.valid() {
            return Err(domain::DataError::Invalid("Soniox permission request").into());
        }
        let proof: domain::WorkerProof = convert(&request.proof)?;
        let lease: domain::CaptureLease = convert(&request.lease)?;
        let fence: domain::WorkerSecret = convert(&request.fence)?;
        let action = match request.operation {
            SonioxPermissionOperation::Issue {} => domain::SonioxPermissionAction::Issue,
            SonioxPermissionOperation::Renew { permission_epoch } => {
                domain::SonioxPermissionAction::Renew(permission_epoch)
            }
            SonioxPermissionOperation::AckDrained { permission_epoch } => {
                domain::SonioxPermissionAction::AckDrained(permission_epoch)
            }
        };
        if !matches!(action, domain::SonioxPermissionAction::AckDrained(_)) {
            self.store
                .service_check_soniox_capture(peer_pid, self.epoch, &proof, &lease, &fence)?;
            let path = self.existing_recording(lease.meeting_id)?;
            ArtifactGuard::verify_begin_owner(&path, &self.identity(&lease), peer_pid)
                .map_err(|_| domain::DataError::Forbidden)?;
        }
        let reply = self
            .store
            .service_soniox_permission(peer_pid, self.epoch, &proof, &lease, &fence, action)?;
        if !request.accepts(&reply) {
            return Err(domain::DataError::Invalid("Soniox permission reply").into());
        }
        Ok(reply)
    }

    fn identity(&self, lease: &domain::CaptureLease) -> CaptureIdentity {
        CaptureIdentity {
            library_id: self.library_id,
            meeting_id: lease.meeting_id,
            worker_session_id: lease.worker_session_id,
            generation: lease.generation,
        }
    }
    fn recording_path(&self, id: uuid::Uuid) -> PathBuf {
        self.store.root().join("recordings").join(id.to_string())
    }
    fn existing_recording(&self, id: uuid::Uuid) -> Result<PathBuf, CoreError> {
        let parent = self.store.root().join("recordings");
        regular(&parent, true)?;
        let path = self.recording_path(id);
        regular(&path, true)?;
        Ok(path)
    }
    fn ensure_recording(&self, id: uuid::Uuid) -> Result<PathBuf, CoreError> {
        let parent = self.store.root().join("recordings");
        match fs::create_dir(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        regular(&parent, true)?;
        let path = self.recording_path(id);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        regular(&path, true)?;
        Ok(path)
    }
    pub fn register_worker(
        &self,
        peer_pid: u32,
        request: wire::RegisterWorkerRequest,
    ) -> Result<wire::RegisteredWorker, CoreError> {
        convert(&self.store.service_register_worker(
            peer_pid,
            self.epoch,
            &convert(&request.registration)?,
            request.ttl_seconds,
        )?)
    }
    pub fn renew_worker(
        &self,
        peer_pid: u32,
        request: wire::RenewWorkerRequest,
    ) -> Result<wire::RegisteredWorker, CoreError> {
        convert(&self.store.service_renew_worker(
            peer_pid,
            self.epoch,
            &convert(&request.proof)?,
            request.ttl_seconds,
        )?)
    }
    pub fn prepare_capture(
        &self,
        peer_pid: u32,
        request: wire::PrepareCaptureRequest,
    ) -> Result<wire::CaptureLease, CoreError> {
        let proof: domain::WorkerProof = convert(&request.proof)?;
        let start: domain::CaptureStart = convert(&request.start)?;
        let existing = self.store.service_capture_context(
            peer_pid,
            self.epoch,
            &proof,
            start.capture_id,
            &start.fence,
        );
        let path = self.recording_path(start.meeting_id);
        match existing {
            Ok((lease, _)) if lease.meeting_id == start.meeting_id => {}
            Ok(_) => return Err(domain::DataError::Forbidden.into()),
            Err(domain::DataError::NotFound) => {
                if path.symlink_metadata().is_ok() {
                    return Err(domain::DataError::Busy.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
        let lease = self
            .store
            .service_prepare_capture(peer_pid, self.epoch, &proof, &start)?;
        let path = self.ensure_recording(lease.meeting_id)?;
        let identity = self.identity(&lease);
        match ArtifactGuard::probe(&path, &identity).map_err(handoff)? {
            OwnerState::Busy { .. } => {
                ArtifactGuard::verify_owner(&path, &identity, peer_pid)
                    .map_err(|_| domain::DataError::Forbidden)?;
            }
            OwnerState::Free => {
                drop(ArtifactGuard::acquire(&path, &identity).map_err(handoff)?);
            }
        }
        convert(&lease)
    }
    pub fn begin_capture(
        &self,
        peer_pid: u32,
        request: wire::BeginCaptureRequest,
    ) -> Result<wire::CaptureLease, CoreError> {
        let proof: domain::WorkerProof = convert(&request.proof)?;
        let lease: domain::CaptureLease = convert(&request.lease)?;
        let fence: domain::WorkerSecret = convert(&request.fence)?;
        let (stored, _) = self.store.service_capture_context(
            peer_pid,
            self.epoch,
            &proof,
            lease.capture_id,
            &fence,
        )?;
        if stored != lease {
            return Err(domain::DataError::Forbidden.into());
        }
        let path = self.existing_recording(lease.meeting_id)?;
        ArtifactGuard::verify_begin_owner(&path, &self.identity(&lease), peer_pid)
            .map_err(|_| domain::DataError::Busy)?;
        convert(
            &self
                .store
                .service_begin_capture(peer_pid, self.epoch, &proof, &lease, &fence)?,
        )
    }
    pub fn abort_capture(
        &self,
        peer_pid: u32,
        request: wire::AbortCaptureRequest,
    ) -> Result<wire::WorkerMutationOutcome, CoreError> {
        let proof: domain::WorkerProof = convert(&request.proof)?;
        let lease: domain::CaptureLease = convert(&request.lease)?;
        let fence: domain::WorkerSecret = convert(&request.fence)?;
        if let Some(receipt) = self
            .store
            .service_abort_capture_receipt(peer_pid, &proof, &lease, &fence)?
        {
            return convert(&receipt);
        }
        self.store
            .service_abort_capture_context(peer_pid, &proof, &lease, &fence)?;
        // Prepare may have committed its reservation before directory init
        // failed. Creating only this authenticated leaf makes retry safe.
        let path = self.ensure_recording(lease.meeting_id)?;
        let guard = ArtifactGuard::acquire(&path, &self.identity(&lease)).map_err(handoff)?;
        if !pristine_reservation(&guard)? {
            return Err(domain::DataError::Busy.into());
        }
        convert(
            &self
                .store
                .service_abort_capture(peer_pid, &proof, &lease, &fence)?,
        )
    }
    pub fn reattach_capture(
        &self,
        peer_pid: u32,
        request: wire::ReattachCaptureRequest,
    ) -> Result<wire::CaptureLease, CoreError> {
        let proof: domain::WorkerProof = convert(&request.proof)?;
        let fence: domain::WorkerSecret = convert(&request.fence)?;
        let (lease, _) = self.store.service_reattach_context(
            peer_pid,
            self.epoch,
            &proof,
            request.capture_id,
            &fence,
        )?;
        let path = self.existing_recording(lease.meeting_id)?;
        ArtifactGuard::verify_owner(&path, &self.identity(&lease), peer_pid)
            .map_err(|_| domain::DataError::Busy)?;
        convert(&self.store.service_reattach_capture(
            peer_pid,
            self.epoch,
            &proof,
            request.capture_id,
            &fence,
            request.ttl_seconds,
        )?)
    }
    pub fn finalize_capture(
        &self,
        peer_pid: u32,
        request: wire::FinalizeCaptureRequest,
    ) -> Result<wire::WorkerMutationOutcome, CoreError> {
        let proof: domain::WorkerProof = convert(&request.proof)?;
        let fence: domain::WorkerSecret = convert(&request.fence)?;
        let requested: domain::CaptureLease = convert(&request.lease)?;
        // A committed receipt is immutable and contains no source body. It can
        // be replayed with its original authenticated worker identity after a
        // service restart or content purge, without authorizing an old-epoch
        // mutation or trying to recreate erased artifact paths.
        let read_receipt = || {
            self.store
                .service_capture_receipt(peer_pid, &proof, &requested, &fence)
        };
        if let Some(receipt) = read_receipt()? {
            return convert(&receipt);
        }
        let (lease, provider) = match self.store.service_capture_context(
            peer_pid,
            self.epoch,
            &proof,
            requested.capture_id,
            &fence,
        ) {
            Ok(context) => context,
            Err(error) => {
                if let Some(receipt) = read_receipt()? {
                    return convert(&receipt);
                }
                return Err(error.into());
            }
        };
        if lease != requested {
            return Err(domain::DataError::Forbidden.into());
        }
        let path = self.existing_recording(lease.meeting_id)?;
        let guard = match SealedCaptureGuard::acquire(&path, &self.identity(&lease)) {
            Ok(guard) => guard,
            Err(HandoffError::Busy) => {
                // The capture worker released its seal. Recovery may already
                // hold the lock and be committing the same immutable receipt.
                let deadline = Instant::now() + FINALIZE_RECEIPT_WAIT;
                loop {
                    if let Some(receipt) = read_receipt()? {
                        return convert(&receipt);
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(handoff(HandoffError::Busy));
                    }
                    std::thread::sleep(remaining.min(FINALIZE_RECEIPT_POLL));
                }
            }
            Err(error) => return Err(handoff(error)),
        };
        let terminal = verified_terminal(&guard, &provider)?;
        // Hold the existing artifact lock until the durable DB ACK. A replay
        // above needs only its authenticated stored receipt.
        convert(
            &self.store.service_finalize_capture(
                peer_pid, self.epoch, &proof, &lease, &fence, &terminal,
            )?,
        )
    }
    /// Called once during service startup, before accepting requests. A busy
    /// old reservation is preserved; later monitor/idle passes never abort it.
    pub fn reconcile_capture_reservations(&self) -> Result<HandoffRecovery, CoreError> {
        let mut report = HandoffRecovery::default();
        for lease in self.store.service_pending_reservations(self.epoch, 32)? {
            let result = (|| -> Result<bool, CoreError> {
                let path = self.ensure_recording(lease.meeting_id)?;
                let guard =
                    ArtifactGuard::acquire(&path, &self.identity(&lease)).map_err(handoff)?;
                if !pristine_reservation(&guard)? {
                    return Ok(false);
                }
                self.store
                    .service_recover_reserved_capture(self.epoch, &lease)?;
                Ok(true)
            })();
            match result {
                Ok(true) => report.aborted += 1,
                Ok(false) => report.pending += 1,
                Err(CoreError::Data(domain::DataError::Busy)) => report.active += 1,
                Err(_) => report.invalid += 1,
            }
        }
        Ok(report)
    }
    /// Recover only after acquiring the writer's released artifact lock. Begin
    /// requires this same lock before starting writers, so a begun capture with
    /// no terminal and no owner can be closed as interrupted.
    /// Reserved captures are intentionally absent from periodic/idle recovery.
    pub fn recover_capture_handoffs(&self) -> Result<HandoffRecovery, CoreError> {
        let mut report = HandoffRecovery::default();
        let leases = self.store.service_pending_captures(32)?;
        for lease in leases {
            let path = match self.existing_recording(lease.meeting_id) {
                Ok(path) => path,
                Err(_) => {
                    report.pending += 1;
                    continue;
                }
            };
            let guard = match CaptureRecoveryGuard::acquire(&path, &self.identity(&lease)) {
                Ok(guard) => guard,
                Err(HandoffError::Busy) => {
                    report.active += 1;
                    continue;
                }
                Err(HandoffError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    report.pending += 1;
                    continue;
                }
                Err(_) => {
                    report.invalid += 1;
                    continue;
                }
            };
            let result = (|| -> Result<(), CoreError> {
                let provider = self.store.service_capture_selection(lease.capture_id)?;
                match &guard {
                    CaptureRecoveryGuard::Sealed(guard) => {
                        let terminal = verified_terminal(guard, &provider)?;
                        self.store
                            .service_recover_capture(self.epoch, &lease, &terminal)?;
                    }
                    CaptureRecoveryGuard::Interrupted(guard) => {
                        let terminal = interrupted_terminal(guard, provider)?;
                        self.store
                            .service_recover_interrupted_capture(self.epoch, &lease, &terminal)?;
                    }
                }
                Ok(())
            })();
            if result.is_ok() {
                report.recovered += 1;
            } else {
                report.invalid += 1;
            }
        }
        Ok(report)
    }
}

fn pristine_reservation(guard: &ArtifactGuard) -> Result<bool, CoreError> {
    if guard.terminal().is_some() || !guard.confirmed().is_empty() || guard.has_incomplete_tail() {
        return Ok(false);
    }
    // At most two expected entries; stop on the first unexpected name. Even a
    // zero-byte media/journal file is evidence that must be retained untouched.
    for entry in fs::read_dir(guard.directory())? {
        let entry = entry?;
        if entry.file_name() != bazza_capture_handoff::LOCK_FILE
            && entry.file_name() != bazza_capture_handoff::OWNER_FILE
        {
            return Ok(false);
        }
        regular(&entry.path(), false)?;
    }
    Ok(true)
}

fn verified_terminal(
    guard: &SealedCaptureGuard,
    provider: &domain::CaptureSelection,
) -> Result<domain::CaptureTerminal, CoreError> {
    use bazza_capture_handoff as artifact;
    let manifest = guard.terminal();
    let expected_provider = match provider.provider {
        domain::CaptureProvider::Local => artifact::SttProvider::Local,
        domain::CaptureProvider::Soniox => artifact::SttProvider::Soniox,
    };
    if manifest.provenance.provider != expected_provider
        || manifest.provenance.model_id != provider.model
    {
        return Err(domain::DataError::Invalid("capture provider mismatch").into());
    }
    if manifest.last_confirmed_sequence > MAX_FINALIZE_CAPTIONS {
        return Err(domain::DataError::Invalid("capture finalization budget").into());
    }
    let path = guard.directory();
    if manifest.has_audio {
        let audio = path.join("mix.wav");
        regular(&audio, false)?;
        let mut header = [0u8; 12];
        fs::File::open(audio)?.read_exact(&mut header)?;
        if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
            return Err(domain::DataError::Invalid("capture WAV header").into());
        }
    }
    if manifest.has_video {
        let video = path.join("screen.mp4");
        regular(&video, false)?;
        if video.metadata()?.len() == 0 {
            return Err(domain::DataError::Invalid("empty capture video").into());
        }
    }
    guard.verify_raw_screen_evidence()?;
    let mut text_bytes = 0;
    let mut captions = Vec::with_capacity(manifest.last_confirmed_sequence as usize);
    guard.scan_confirmed::<CoreError>(|caption| {
        text_bytes += caption.text.len() + caption.speaker.as_ref().map_or(0, String::len);
        if text_bytes > MAX_FINALIZE_TEXT_BYTES {
            return Err(domain::DataError::Invalid("capture finalization budget").into());
        }
        captions.push(domain::ConfirmedCaption {
            sequence: caption.sequence,
            start_ms: caption.start_ms as i64,
            end_ms: caption.end_ms as i64,
            speaker: caption.speaker,
            text: caption.text,
        });
        Ok(())
    })?;
    Ok(domain::CaptureTerminal {
        status: match manifest.status {
            artifact::CaptureTerminalStatus::Completed => domain::CaptureTerminalStatus::Completed,
            artifact::CaptureTerminalStatus::Failed => domain::CaptureTerminalStatus::Failed,
        },
        provider: provider.clone(),
        duration_ms: manifest.duration_ms as i64,
        ended_at: manifest.ended_at,
        has_video: manifest.has_video,
        last_confirmed_sequence: manifest.last_confirmed_sequence,
        captions,
        raw_screen: guard.raw_screen_evidence().cloned(),
        soniox_outcome: guard.soniox_outcome().copied(),
        retain_local_live: guard.retain_local_live(),
    })
}

fn interrupted_terminal(
    guard: &InterruptedCaptureGuard,
    provider: domain::CaptureSelection,
) -> Result<domain::CaptureTerminal, CoreError> {
    use bazza_capture_handoff::SttProvider;
    let soniox = provider.provider == domain::CaptureProvider::Soniox;
    let expected = if soniox {
        SttProvider::Soniox
    } else {
        SttProvider::Local
    };
    let mut captions = Vec::new();
    let mut bytes = 0;
    let mut duration_ms = interrupted_wav_duration(&guard.directory().join("mix.wav"))?;
    let scan = guard.scan_confirmed::<CoreError>(|caption| {
        if caption.provenance.provider != expected || caption.provenance.model_id != provider.model
        {
            return Ok(false);
        }
        let caption = domain::ConfirmedCaption {
            sequence: caption.sequence,
            start_ms: caption.start_ms as i64,
            end_ms: caption.end_ms as i64,
            speaker: caption.speaker.filter(|speaker| !speaker.is_empty()),
            text: caption.text,
        };
        // Reserve room for the terminal envelope in Store's serialized budget.
        bytes += serde_json::to_vec(&caption)?.len() + 1;
        if bytes > MAX_FINALIZE_TEXT_BYTES - bazza_capture_handoff::MAX_MANIFEST_BYTES {
            return Ok(false);
        }
        duration_ms = duration_ms.max(caption.end_ms);
        captions.push(caption);
        Ok(captions.len() < MAX_FINALIZE_CAPTIONS as usize)
    });
    match scan {
        Ok(())
        | Err(CoreError::Handoff(
            HandoffError::CorruptJournal
            | HandoffError::IdentityMismatch
            | HandoffError::TooLarge
            | HandoffError::UnsafePath,
        )) => {}
        Err(error) => return Err(error),
    }
    let soniox_outcome = (soniox && provider.model.is_some()).then_some(
        bazza_capture_handoff::SonioxCaptureOutcome::Incomplete {
            reason: bazza_capture_handoff::SonioxIncompleteReason::FinalizationInterrupted,
        },
    );
    Ok(domain::CaptureTerminal {
        status: domain::CaptureTerminalStatus::Failed,
        provider,
        duration_ms,
        ended_at: std::time::SystemTime::now().into(),
        has_video: false,
        last_confirmed_sequence: captions.len() as u64,
        retain_local_live: !soniox && !captions.is_empty(),
        captions,
        raw_screen: None,
        soniox_outcome,
    })
}

fn interrupted_wav_duration(path: &Path) -> Result<i64, CoreError> {
    match regular(path, false) {
        Ok(()) => {}
        Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(CoreError::Data(domain::DataError::Forbidden)) => return Ok(0),
        Err(error) => return Err(error),
    }
    let mut file = fs::File::open(path)?;
    // The recorder writes a fixed 44-byte mono 16-bit PCM header and refreshes
    // it periodically. Only count the prefix covered by the last header update.
    let mut header = [0u8; 44];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    if &header[..4] != b"RIFF"
        || &header[8..16] != b"WAVEfmt "
        || header[16..24] != [16, 0, 0, 0, 1, 0, 1, 0]
        || header[32..40] != [2, 0, 16, 0, b'd', b'a', b't', b'a']
    {
        return Ok(0);
    }
    let read_u32 = |start| {
        u32::from_le_bytes(
            header[start..start + 4]
                .try_into()
                .expect("fixed WAV header"),
        )
    };
    let sample_rate = u64::from(read_u32(24));
    if sample_rate == 0 || u64::from(read_u32(28)) != sample_rate * 2 {
        return Ok(0);
    }
    let pcm_bytes =
        u64::from(read_u32(40)).min(file.metadata()?.len().saturating_sub(header.len() as u64));
    Ok((pcm_bytes / 2 * 1000 / sample_rate) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_test_support::create_v8;

    fn start(meeting_id: uuid::Uuid) -> wire::CaptureStart {
        wire::CaptureStart {
            capture_id: uuid::Uuid::new_v4(),
            meeting_id,
            fence: wire::WorkerSecret::random(),
            title: "fixture".into(),
            app: None,
            started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
            participants: vec![],
            provider: wire::CaptureSelection {
                provider: wire::CaptureProvider::Local,
                model: Some("fixture-model".into()),
                language: None,
            },
        }
    }
    #[test]
    fn unknown_or_non_capture_worker_is_rejected_before_existing_artifact_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let meeting = uuid::Uuid::new_v4();
        let path = repository.recording_path(meeting);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), b"preserve").unwrap();
        let proof = wire::WorkerProof {
            session_id: uuid::Uuid::new_v4(),
            secret: wire::WorkerSecret::random(),
            library_epoch: repository.epoch,
        };
        let result = repository.prepare_capture(
            std::process::id(),
            wire::PrepareCaptureRequest {
                proof,
                start: start(meeting),
            },
        );
        assert!(matches!(
            result,
            Err(CoreError::Data(domain::DataError::Forbidden))
        ));
        let registration = wire::WorkerRegistration::new(vec![wire::WorkerCapability::PostProcess]);
        repository
            .register_worker(
                std::process::id(),
                wire::RegisterWorkerRequest {
                    registration: registration.clone(),
                    ttl_seconds: 60,
                },
            )
            .unwrap();
        let proof = wire::WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: repository.epoch,
        };
        let result = repository.prepare_capture(
            std::process::id(),
            wire::PrepareCaptureRequest {
                proof,
                start: start(meeting),
            },
        );
        assert!(matches!(
            result,
            Err(CoreError::Data(domain::DataError::Forbidden))
        ));
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"preserve");
        assert!(!path.join(bazza_capture_handoff::LOCK_FILE).exists());
    }
}

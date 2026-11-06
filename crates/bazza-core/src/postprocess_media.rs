//! Fixed, private worker media inputs. Database admission brackets filesystem
//! inspection; a cooperating reader and this operation retain the media gate.
use crate::{CoreError, Repository};
use bazza_capture_handoff::{
    media_read::{self, MediaReadIdentity},
    read_terminal_snapshot_with_evidence, CaptureIdentity, CaptureTerminalStatus, SttProvider,
};
use bazza_media_gate::{MediaGateError, MediaReadGuard};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_media::{
        PostProcessMediaArtifact, PostProcessMediaGrant, PostProcessMediaKind,
        PostProcessMediaRequest,
    },
    postprocess_video::video_staging_name,
};
use bazza_store::service_data as data;
use std::{
    fs::{self, Metadata},
    io::{self, Read},
    path::Path,
};

pub(super) fn present(path: &Path) -> io::Result<bool> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn ordinary(path: &Path, directory: bool) -> Result<Metadata, CoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }
    {
        return Err(data::DataError::Forbidden.into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(data::DataError::Forbidden.into());
        }
    }
    Ok(metadata)
}

pub(super) fn gate_error(error: MediaGateError) -> CoreError {
    match error {
        MediaGateError::Busy => data::DataError::Busy.into(),
        MediaGateError::Invalid(_) => data::DataError::Forbidden.into(),
        MediaGateError::Io(error) => error.into(),
    }
}

impl Repository {
    pub fn post_process_media(
        &self,
        peer_pid: u32,
        request: PostProcessMediaRequest,
    ) -> Result<DesktopEnvelope<PostProcessMediaGrant>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process media request").into());
        }
        let stored_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        // No filesystem access or guard creation precedes registered admission.
        let context =
            self.store
                .service_post_process_media_context(peer_pid, self.epoch, &stored_request)?;
        let root = self.store.root().canonicalize()?;
        let meeting = request.lease.meeting_id.to_string();
        let _gate = MediaReadGuard::acquire(&root, &meeting).map_err(gate_error)?;
        let input = &request.lease.input;
        let owner_identity = MediaReadIdentity {
            library_id: self.library_id,
            library_epoch: self.epoch,
            worker_session_id: request.proof.session_id,
            claim_id: request.lease.claim_id,
            job_id: request.lease.job_id.clone(),
            meeting_id: request.lease.meeting_id,
            capture_id: input.capture_id,
            capture_generation: input.capture_generation,
            checkpoint_revision: input.checkpoint_revision.clone(),
            lifecycle_revision: input.lifecycle_revision.clone(),
            transcript_revision: input.transcript_revision.clone(),
            source_id: input.source_id.clone(),
        };
        media_read::verify_owner(&root, &owner_identity, request.owner_nonce, peer_pid)
            .map_err(|_| data::DataError::Forbidden)?;

        let recordings = root.join("recordings");
        ordinary(&recordings, true)?;
        if recordings.canonicalize()? != recordings {
            return Err(data::DataError::Forbidden.into());
        }
        let directory = recordings.join(&meeting);
        ordinary(&directory, true)?;
        if directory.canonicalize()? != directory {
            return Err(data::DataError::Forbidden.into());
        }
        let capture_identity = CaptureIdentity {
            library_id: self.library_id,
            meeting_id: request.lease.meeting_id,
            worker_session_id: context.capture_worker_session_id,
            generation: input.capture_generation,
        };
        let snapshot = read_terminal_snapshot_with_evidence(&directory, &capture_identity)?;
        let terminal = snapshot.manifest;
        let provider = match context.provider.provider {
            data::CaptureProvider::Local => SttProvider::Local,
            data::CaptureProvider::Soniox => SttProvider::Soniox,
        };
        if terminal.status != CaptureTerminalStatus::Completed
            || !terminal.has_audio
            || terminal.last_confirmed_sequence != context.last_confirmed_sequence
            || terminal.provenance.provider != provider
            || terminal.provenance.model_id != context.provider.model
            || snapshot.raw_screen != context.raw_screen
            || snapshot.soniox_outcome != context.soniox_outcome
        {
            return Err(data::DataError::Invalid("post-process capture terminal").into());
        }

        let mut artifacts = Vec::new();
        for kind in PostProcessMediaKind::ALL {
            // V1 keeps optional discovery. V2 screen files require sealed
            // evidence. An audio clock is independently optional when the
            // only V2 evidence is the Soniox outcome.
            let expected_raw = match (context.raw_screen.as_ref(), kind) {
                (Some(raw), PostProcessMediaKind::RawVideo) => Some(raw.raw_mp4_bytes),
                (Some(raw), PostProcessMediaKind::VideoTimeline) => Some(raw.timeline_bytes),
                (Some(raw), PostProcessMediaKind::VideoClock) => Some(raw.video_clock_bytes),
                (Some(raw), PostProcessMediaKind::AudioClock) => Some(raw.audio_clock_bytes),
                (
                    None,
                    PostProcessMediaKind::RawVideo
                    | PostProcessMediaKind::VideoTimeline
                    | PostProcessMediaKind::VideoClock,
                ) if context.soniox_outcome.is_some() => Some(None),
                _ => None,
            };
            if expected_raw == Some(None) {
                continue;
            }
            let path = directory.join(kind.file_name());
            let metadata = match ordinary(&path, false) {
                Ok(metadata) => metadata,
                Err(CoreError::Io(error))
                    if error.kind() == io::ErrorKind::NotFound
                        && kind != PostProcessMediaKind::AudioMix =>
                {
                    continue
                }
                Err(error) => return Err(error),
            };
            if expected_raw
                .flatten()
                .is_some_and(|bytes| metadata.len() != bytes)
            {
                continue;
            }
            if kind == PostProcessMediaKind::AudioMix {
                // Match existing finalize screening. This is not decoding or a
                // claim about waveform quality, clocks or A/V synchronization.
                let mut header = [0; 12];
                fs::File::open(&path)?.read_exact(&mut header)?;
                if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
                    return Err(data::DataError::Invalid("post-process WAV header").into());
                }
            }
            let artifact = PostProcessMediaArtifact {
                kind,
                private_path: path
                    .to_str()
                    .ok_or(data::DataError::Invalid("post-process media encoding"))?
                    .into(),
                bytes: metadata.len(),
            };
            if !artifact.valid() {
                return Err(data::DataError::Invalid("post-process media projection").into());
            }
            artifacts.push(artifact);
        }
        let video_publication_candidate = match &context.video_record {
            Some(data::PostProcessVideoRecord::Prepared {
                claim_id,
                owner_nonce,
                ..
            }) => {
                present(&directory.join("screen.mp4"))?
                    || present(&directory.join(video_staging_name(*claim_id, *owner_nonce)))?
            }
            Some(data::PostProcessVideoRecord::Published(..)) => true,
            _ => false,
        };
        // Revoke/delete/expiry/reclaim during proof or metadata I/O cannot
        // publish paths under the earlier database snapshot.
        let current =
            self.store
                .service_post_process_media_context(peer_pid, self.epoch, &stored_request)?;
        if current != context {
            return Err(data::DataError::Forbidden.into());
        }
        let grant = PostProcessMediaGrant {
            library_scope: context.library_scope,
            library_epoch: self.epoch,
            worker_session_id: request.proof.session_id,
            claim_id: request.lease.claim_id,
            job_id: request.lease.job_id.clone(),
            job_revision: request.lease.job_revision.clone(),
            meeting_id: request.lease.meeting_id,
            input: request.lease.input.clone(),
            owner_nonce: request.owner_nonce,
            provider: request.expected_provider.clone(),
            expires_at: context.expires_at,
            screen_writer: context.raw_screen.as_ref().map(|raw| raw.writer),
            video_publication_candidate,
            video_failure: match context.video_record {
                Some(data::PostProcessVideoRecord::Unavailable(reason, _)) => Some(reason),
                _ => None,
            },
            artifacts,
        };
        if !grant.valid_for(&request) {
            return Err(data::DataError::Invalid("post-process media grant").into());
        }
        self.desktop_envelope(grant)
    }
}

#[cfg(test)]
#[path = "postprocess_media_tests.rs"]
mod tests;

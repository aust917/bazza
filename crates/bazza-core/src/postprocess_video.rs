//! Install only the fixed, checked FFmpeg output of a live worker claim.
use crate::{
    postprocess_media::{gate_error, present},
    CoreError, Repository,
};
use bazza_capture_handoff::{install_synced_media, open_video_output};
use bazza_media_gate::MediaReadGuard;
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_media::PostProcessMediaKind,
    postprocess_video::{
        video_staging_name, PostProcessVideoAction, PostProcessVideoFailure,
        PostProcessVideoReceipt, PostProcessVideoRequest,
    },
};
use bazza_store::service_data::{self as data, PostProcessVideoContent, PostProcessVideoRecord};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

fn require_absent(path: &Path) -> data::DataResult<()> {
    if present(path)? {
        return Err(data::DataError::Forbidden);
    }
    Ok(())
}

/// Check the top-level MP4 structure with bounded work. The trusted native
/// muxer performs actual stream verification before writing this staged file.
fn mp4_structure(file: &mut File, bytes: u64) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "post-process MP4 structure");
    let mut offset = 0;
    let mut media = false;
    let mut movie = false;
    for index in 0..1024 {
        if offset == bytes && media && movie {
            file.rewind()?;
            return Ok(());
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let tag: [u8; 4] = header[4..].try_into().unwrap();
        let short = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (size, header_bytes) = match short {
            0 => (bytes - offset, 8),
            1 => {
                file.read_exact(&mut header)?;
                (u64::from_be_bytes(header), 16)
            }
            value => (u64::from(value), 8),
        };
        if size < header_bytes || size > bytes - offset {
            return Err(invalid());
        }
        if index == 0 && (tag != *b"ftyp" || size - header_bytes < 8) {
            return Err(invalid());
        }
        media |= tag == *b"mdat" && size > header_bytes;
        movie |= tag == *b"moov" && size > header_bytes;
        offset += size;
    }
    Err(invalid())
}

fn inspect(
    path: &Path,
    request: &PostProcessVideoRequest,
    staging: bool,
) -> Result<PostProcessVideoContent, CoreError> {
    let mut file = open_video_output(path, staging)?;
    let metadata = file.metadata()?;
    if staging {
        file.sync_all()?;
    }
    mp4_structure(&mut file, metadata.len())
        .map_err(|_| data::DataError::Invalid("post-process MP4 structure"))?;
    Ok(PostProcessVideoContent::read(
        file,
        metadata.len(),
        request.media.lease.expires_at,
    )?)
}

impl Repository {
    pub fn post_process_video(
        &self,
        peer_pid: u32,
        request: PostProcessVideoRequest,
    ) -> Result<DesktopEnvelope<PostProcessVideoReceipt>, CoreError> {
        if !request.media.valid() {
            return Err(data::DataError::Invalid("post-process video request").into());
        }
        let stored = serde_json::from_value(serde_json::to_value(&request.media)?)?;
        let record = self
            .store
            .service_post_process_video_record(peer_pid, self.epoch, &stored)?;
        let root = self.store.root().canonicalize()?;
        let lease = &request.media.lease;
        let meeting = lease.meeting_id.to_string();
        let _gate = MediaReadGuard::acquire(&root, &meeting).map_err(gate_error)?;
        // This also authenticates the peer's live media owner, rechecks sealed
        // raw inputs, and verifies the fixed ordinary meeting directory.
        let media = self
            .post_process_media(peer_pid, request.media.clone())?
            .data;
        let raw = media
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == PostProcessMediaKind::RawVideo && artifact.bytes > 0);
        let directory = root.join("recordings").join(meeting);
        let final_path = directory.join("screen.mp4");
        let staging = directory.join(video_staging_name(
            lease.claim_id,
            request.media.owner_nonce,
        ));
        let (outcome, change_sequence) = match request.result {
            PostProcessVideoAction::Publish {} => {
                if present(&final_path)? {
                    let content = inspect(&final_path, &request, false)?;
                    self.store.service_commit_post_process_video(
                        peer_pid,
                        self.epoch,
                        &stored,
                        &content,
                        || {
                            #[cfg(unix)]
                            File::open(&directory)?.sync_all()?;
                            Ok(())
                        },
                    )?
                } else {
                    if matches!(record, Some(PostProcessVideoRecord::Published(..))) {
                        return Err(data::DataError::Forbidden.into());
                    }
                    let previous = match record {
                        Some(PostProcessVideoRecord::Prepared {
                            content,
                            claim_id,
                            owner_nonce,
                        }) => {
                            let path = directory.join(video_staging_name(claim_id, owner_nonce));
                            if present(&path)? {
                                Some((path, content))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    let (source, content) = if let Some((path, saved)) = previous {
                        let content = inspect(&path, &request, true)?;
                        if saved != content {
                            return Err(data::DataError::Forbidden.into());
                        }
                        (path, content)
                    } else {
                        if !raw {
                            return Err(data::DataError::Forbidden.into());
                        }
                        let content = inspect(&staging, &request, true)?;
                        #[cfg(unix)]
                        File::open(&directory)?.sync_all()?;
                        self.store.service_prepare_post_process_video(
                            peer_pid,
                            self.epoch,
                            &stored,
                            &content,
                            || require_absent(&final_path),
                        )?;
                        (staging, content)
                    };
                    self.store.service_commit_post_process_video(
                        peer_pid,
                        self.epoch,
                        &stored,
                        &content,
                        || Ok(install_synced_media(&source, &final_path)?),
                    )?
                }
            }
            PostProcessVideoAction::Unavailable { reason } => {
                if raw == (reason == PostProcessVideoFailure::MissingRaw) {
                    return Err(
                        data::DataError::Invalid("post-process video failure reason").into(),
                    );
                }
                self.store.service_unavailable_post_process_video(
                    peer_pid,
                    self.epoch,
                    &stored,
                    reason,
                    || require_absent(&final_path),
                )?
            }
        };
        self.desktop_envelope(PostProcessVideoReceipt {
            claim_id: lease.claim_id,
            job_id: lease.job_id.clone(),
            job_revision: lease.job_revision.clone(),
            meeting_id: lease.meeting_id,
            capture_id: lease.input.capture_id,
            change_sequence,
            outcome,
        })
    }
}

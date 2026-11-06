use super::{emit, media_path, Claim, WorkError, WorkResult, Worker};
use crate::state::RuntimeState;
use bazza_service_client::{
    api::{postprocess_media::PostProcessMediaKind, postprocess_video::PostProcessVideoFailure},
    postprocess_media::OwnedPostProcessMedia,
};
use std::{
    fs::{File, OpenOptions},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::{AppHandle, Manager};

impl Worker {
    pub(super) async fn settle_video(
        &mut self,
        app: &AppHandle,
        media: &Arc<OwnedPostProcessMedia>,
        claim: &mut Claim,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        if media.grant().screen_writer.is_none() {
            if media_path(media, PostProcessMediaKind::RawVideo).is_some() {
                return Err(WorkError::Deferred(
                    "raw screen media lacks a sealed capture outcome",
                ));
            }
            return Ok(());
        }
        if media.grant().video_failure.is_some() {
            return Ok(());
        }
        let result = if media.has_video_publication_candidate()? {
            // Service-side replay validates the fixed final artifact against
            // its durable intent and never overwrites an unrelated file.
            Ok(())
        } else {
            emit(app, claim.lease.meeting_id, "mux", 0.02, None);
            let media = media.clone();
            let ffmpeg = app.state::<RuntimeState>().ffmpeg.clone();
            let pin = media.try_clone_file()?;
            self.compute(claim, stopping, move || {
                Ok(mux(&media, ffmpeg.as_deref(), &pin))
            })
            .await?
        };
        if stopping.load(Ordering::Acquire) {
            return Err(WorkError::Stopping);
        }
        // The service streams a digest of a potentially large video before
        // rechecking the claim and making the recording visible.
        self.keep_claim_for(claim, 65).await?;
        match result {
            Ok(()) => {
                self.client
                    .publish_post_process_video(
                        &self.registered,
                        media,
                        &self.proof,
                        &claim.lease,
                        &claim.fence,
                    )
                    .await?;
            }
            Err(reason) => {
                self.client
                    .record_post_process_video_failure(
                        &self.registered,
                        media,
                        &self.proof,
                        &claim.lease,
                        &claim.fence,
                        reason,
                    )
                    .await?;
                emit(
                    app,
                    claim.lease.meeting_id,
                    "mux",
                    0.02,
                    Some("Video processing failed. The original capture media was kept."),
                );
            }
        }
        Ok(())
    }
}

fn mux(
    media: &OwnedPostProcessMedia,
    ffmpeg: Option<&std::path::Path>,
    pin: &File,
) -> Result<(), PostProcessVideoFailure> {
    let raw = media
        .grant()
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == PostProcessMediaKind::RawVideo && artifact.bytes != 0)
        .ok_or(PostProcessVideoFailure::MissingRaw)?;
    let raw = std::path::Path::new(&raw.private_path);
    let audio = media_path(media, PostProcessMediaKind::AudioMix)
        .ok_or(PostProcessVideoFailure::MuxFailed)?;
    let ffmpeg = ffmpeg.ok_or(PostProcessVideoFailure::FfmpegUnavailable)?;
    // Native screen capture uses both clocks and the pause timeline. An
    // artifact absent from the grant may be unsealed or have changed length.
    // Do not let legacy fallback silently interpret it as zero pause time.
    for kind in [
        PostProcessMediaKind::VideoTimeline,
        PostProcessMediaKind::VideoClock,
        PostProcessMediaKind::AudioClock,
    ] {
        if media_path(media, kind).is_none() {
            return Err(PostProcessVideoFailure::MuxFailed);
        }
    }
    let staging = media
        .video_staging_path()
        .map_err(|_| PostProcessVideoFailure::MuxFailed)?;
    bazza_capture::video::mux_recording_for_recovery_with_pin(ffmpeg, raw, audio, &staging, pin)
        .map_err(|_| PostProcessVideoFailure::MuxFailed)?;
    OpenOptions::new()
        .write(true)
        .open(staging)
        .and_then(|file| file.sync_all())
        .map_err(|_| PostProcessVideoFailure::MuxFailed)?;
    Ok(())
}

//! The existing worker owns admission and heartbeats through native decoding.
use super::{Claim, WorkError, WorkResult, Worker};
use crate::{
    bot_supplement,
    commands::CmdResult,
    state::RuntimeState,
    transcript_versions::{self, TranscriptAudioSource, TranscriptVersionOptions, VersionJobs},
    ui_message::UiMessage,
};
use bazza_media_gate::MediaReadGuard;
use bazza_service_client::RepositoryClient;
use std::{path::Path, sync::atomic::AtomicBool};
use tauri::{AppHandle, Manager};

pub(super) enum Outcome { Unchanged, Published, Incomplete }

impl Worker {
    pub(super) async fn supplement(
        &mut self, app: &AppHandle, root: &Path, claim: &mut Claim, stopping: &AtomicBool,
    ) -> WorkResult<Outcome> {
        let id = claim.lease.meeting_id.to_string();
        if !bot_supplement::marker_exists(&root.join("recordings").join(&id)) {
            return Ok(Outcome::Unchanged);
        }
        let repository = app.try_state::<RepositoryClient>()
            .ok_or(WorkError::Deferred("the transcript repository is unavailable"))?.inner().clone();
        let versions = repository.transcript_versions(&id).await?.data;
        if bot_supplement::has_result(&versions) {
            return Ok(if bot_supplement::published_result(&versions) { Outcome::Unchanged } else { Outcome::Incomplete });
        }
        let before = repository.meeting(id.clone()).await?;
        let meeting = before.data.ok_or(WorkError::Deferred("the meeting was removed"))?;
        if meeting.lifecycle_revision != claim.lease.input.lifecycle_revision
            || meeting.transcript_revision != claim.lease.input.transcript_revision {
            return Err(WorkError::Deferred("the supplement input changed"));
        }
        let lease = app.state::<VersionJobs>().start(&id)
            .map_err(|_| WorkError::Deferred("a transcript version is already running"))?;
        let job = lease.0.clone();
        let worker_job = job.clone();
        let desktop = app.clone();
        let library = root.to_path_buf();
        let meeting_id = id.clone();
        let computed = self.compute_with_cancel(claim, stopping, Some(job.cancel.clone()), move || {
            let result: CmdResult<_> = (|| {
                let runtime = desktop.state::<RuntimeState>();
                let _work = runtime.local_asr_work.lock();
                let _busy = runtime.busy_guard();
                let options = transcript_versions::resolve_options(&runtime, TranscriptVersionOptions {
                    source: TranscriptAudioSource::Combined, model: None, language: None,
                })?;
                let guard = MediaReadGuard::acquire(&library, &meeting_id).map_err(|_| unavailable())?;
                let version = bot_supplement::compute(&desktop, &runtime, &guard, &meeting_id,
                    &options.model, options.language.as_deref(), &worker_job)?;
                Ok((version, guard))
            })();
            Ok(result)
        }).await?;
        let (version, _media_pin) = match computed {
            Ok(result) => result,
            Err(error) => {
                lease.finish(&Err(error));
                incomplete(app, &id);
                return Ok(Outcome::Incomplete);
            }
        };
        if let Err(error) = job.begin_save(app, &id) {
            lease.finish(&Err(error));
            return Ok(Outcome::Incomplete);
        }
        self.keep_claim(claim).await?;
        let result = repository.save_transcript_version(&before.identity, &id,
            &meeting.lifecycle_revision, &meeting.transcript_revision, &version).await;
        let info = match result {
            Ok(result) => result.data,
            Err(error) => {
                lease.finish(&Err(unavailable()));
                return Err(error.into());
            }
        };
        lease.finish(&Ok(info.clone()));
        // A complete publication retires the old claim and rebinds the Ready
        // checkpoint. Return to polling: optional AI must use a fresh claim.
        let after = repository.transcript_versions(&id).await?.data;
        if after.effective_id.as_deref() == Some(info.id.as_str()) {
            Ok(Outcome::Published)
        } else {
            incomplete(app, &id);
            Ok(Outcome::Incomplete)
        }
    }
}

fn unavailable() -> UiMessage {
    UiMessage::app("native.botSupplementUnavailable", "Could not prepare the complete transcript. The original subtitles were kept.")
}

fn incomplete(app: &AppHandle, id: &str) {
    log::warn!("bot supplement incomplete: meeting={id}");
    crate::recorder::notice_message(app, "warning", UiMessage::app("native.botSupplementIncomplete",
        "The complete transcript needs review. Original subtitles and any partial version remain available."));
}

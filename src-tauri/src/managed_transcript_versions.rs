//! Native recognition with protected service media and immutable service writes.

use crate::{
    bot_transcript,
    commands::{blocking, CmdResult},
    recorder_service::ServiceCapture,
    state::RuntimeState,
    transcript_versions::{self, TranscriptAudioSource, TranscriptVersionOptions, VersionJob},
    ui_message::{app as ui, UiMessage},
};
use bazza_domain::transcript_versions::{
    NewTranscriptVersion, TranscriptVersion, TranscriptVersionSegment, TranscriptVersionSource,
    TranscriptVersions,
};
use bazza_media_gate::MediaReadGuard;
use bazza_service_client::{
    api::{desktop::DesktopMeeting, rpc::ErrorCode},
    repository::RepositoryIdentity,
    ClientError, RepositoryClient,
};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

fn unavailable() -> UiMessage {
    ui(
        "native.serviceMeetingUnavailable",
        "The meeting service is unavailable.",
    )
}

fn stale() -> UiMessage {
    ui(
        "native.serviceMeetingRefreshRequired",
        "Refresh the meeting before continuing.",
    )
}

fn busy() -> UiMessage {
    ui(
        "native.serviceMeetingBusy",
        "The meeting is still recording or processing.",
    )
}

fn media_unavailable() -> UiMessage {
    ui(
        "native.botTranscriptUnavailable",
        "The requested meeting audio is unavailable.",
    )
}

fn service_error(error: ClientError) -> UiMessage {
    if let ClientError::Remote(error) = error {
        return match error.code {
            ErrorCode::Conflict | ErrorCode::ResyncRequired => stale(),
            ErrorCode::ResourceBusy => busy(),
            ErrorCode::NotFound => ui("native.meetingNotFound", "Meeting not found."),
            ErrorCode::UpgradeRequired => ui(
                "native.serviceMeetingUnavailable",
                "Update the local Bazza service to use transcript versions.",
            ),
            _ => unavailable(),
        };
    }
    unavailable()
}

fn repository(app: &AppHandle) -> CmdResult<RepositoryClient> {
    app.try_state::<RepositoryClient>()
        .map(|state| state.inner().clone())
        .ok_or_else(unavailable)
}

struct MeetingInput {
    identity: RepositoryIdentity,
    row: DesktopMeeting,
}

impl MeetingInput {
    async fn read(repository: &RepositoryClient, meeting_id: &str) -> CmdResult<Self> {
        let snapshot = repository
            .meeting(meeting_id.to_owned())
            .await
            .map_err(service_error)?;
        let row = snapshot
            .data
            .ok_or_else(|| ui("native.meetingNotFound", "Meeting not found."))?;
        Ok(Self {
            identity: snapshot.identity,
            row,
        })
    }

    fn ensure_same(&self, other: &Self) -> CmdResult<()> {
        if self.identity != other.identity
            || self.row.id != other.row.id
            || self.row.lifecycle_revision != other.row.lifecycle_revision
            || self.row.transcript_revision != other.row.transcript_revision
        {
            return Err(stale());
        }
        Ok(())
    }
}

pub(crate) async fn list(app: &AppHandle, meeting_id: &str) -> CmdResult<TranscriptVersions> {
    let repository = repository(app)?;
    let input = MeetingInput::read(&repository, meeting_id).await?;
    let mut snapshot = repository
        .transcript_versions(meeting_id)
        .await
        .map_err(service_error)?;
    if snapshot.identity != input.identity {
        return Err(stale());
    }
    if matches!(input.row.status.as_str(), "processing" | "ready") {
        let root = repository.root().to_path_buf();
        let id = meeting_id.to_owned();
        let preferred = input.row.app.clone();
        snapshot.data.participant_source = blocking(move || {
            let source = MediaReadGuard::acquire(&root, &id).ok().and_then(|guard| {
                bot_transcript::participant_source(&guard, preferred.as_deref())
                    .ok()
                    .flatten()
            });
            Ok(source.map(transcript_versions::participant_label))
        })
        .await?;
    }
    input.ensure_same(&MeetingInput::read(&repository, meeting_id).await?)?;
    Ok(snapshot.data)
}

pub(crate) async fn get(
    app: &AppHandle,
    meeting_id: &str,
    version_id: &str,
) -> CmdResult<Vec<TranscriptVersionSegment>> {
    let repository = repository(app)?;
    repository
        .transcript_version(meeting_id, version_id)
        .await
        .map_err(service_error)?
        .data
        .ok_or_else(|| {
            ui(
                "native.transcriptVersionUnavailable",
                "The transcript version is unavailable.",
            )
        })
}

pub(crate) async fn regenerate(
    app: &AppHandle,
    meeting_id: &str,
    options: TranscriptVersionOptions,
    job: &Arc<VersionJob>,
) -> CmdResult<TranscriptVersion> {
    let repository = repository(app)?;
    let input = MeetingInput::read(&repository, meeting_id).await?;
    if input.row.status != "ready" {
        return Err(busy());
    }
    let (input, version, _media_pin) = {
        let repository = repository.clone();
        let app = app.clone();
        let meeting_id = meeting_id.to_owned();
        let job = Arc::clone(job);
        blocking(move || {
            job.checkpoint()?;
            let runtime = app.state::<RuntimeState>();
            let _asr = runtime.local_asr_work.try_lock().ok_or_else(busy)?;
            let _busy = runtime.busy_guard();
            if !app.state::<ServiceCapture>().try_admit_postprocess() {
                return Err(busy());
            }
            let options = transcript_versions::resolve_options(&runtime, options)?;
            let pin = MediaReadGuard::acquire(repository.root(), &meeting_id)
                .map_err(|_| media_unavailable())?;
            let latest =
                tauri::async_runtime::block_on(MeetingInput::read(&repository, &meeting_id))?;
            input.ensure_same(&latest)?;
            if options.source == TranscriptAudioSource::Combined {
                let version = crate::bot_supplement::compute(&app, &runtime, &pin, &meeting_id,
                    &options.model, options.language.as_deref(), &job)?;
                return Ok((input, version, pin));
            }
            let (source, segments) = match options.source {
                TranscriptAudioSource::Recording => {
                    if !input.row.media.audio {
                        return Err(media_unavailable());
                    }
                    (
                        TranscriptVersionSource::Recording,
                        transcript_versions::compute_mix(
                            &app,
                            &runtime,
                            &pin,
                            &meeting_id,
                            &options.model,
                            options.language.as_deref(),
                            &job,
                        )?
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    )
                }
                TranscriptAudioSource::Participants => {
                    let source =
                        bot_transcript::participant_source(&pin, input.row.app.as_deref())?
                            .ok_or_else(media_unavailable)?;
                    let segments = bot_transcript::compute_for_version(
                        &app,
                        &runtime,
                        &pin,
                        &meeting_id,
                        source,
                        &options.model,
                        options.language.as_deref(),
                        &job,
                    )?;
                    let label = transcript_versions::track_version_source(source);
                    (label, segments)
                }
                TranscriptAudioSource::Combined => unreachable!(),
            };
            if segments.is_empty() {
                return Err(transcript_versions::no_speech());
            }
            let version = NewTranscriptVersion {
                id: Uuid::new_v4().to_string(),
                source,
                model: options.model,
                language: options.language,
                segments,
                partial: false,
            };
            Ok((input, version, pin))
        })
        .await?
    };
    job.begin_save(app, meeting_id)?;
    let result = repository
        .save_transcript_version(
            &input.identity,
            meeting_id,
            &input.row.lifecycle_revision,
            &input.row.transcript_revision,
            &version,
        )
        .await
        .map_err(service_error)?;
    Ok(result.data)
}

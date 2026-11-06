use crate::{
    bot_transcript::{self, Source},
    commands::{blocking, err, CmdResult},
    state::{AppState, RuntimeState},
    ui_message::UiMessage,
};
use bazza_domain::transcript_versions::{
    NewTranscriptVersion, TranscriptParticipantSource, TranscriptVersion, TranscriptVersionSegment,
    TranscriptVersionSource, TranscriptVersions,
};
use bazza_media_gate::{MediaReadGuard, PlaybackMediaKind};
use bazza_service_client::RepositoryClient;
use bazza_store::{MeetingStatus, NewSegment};
use serde::Deserialize;
use std::{io::BufReader, sync::Arc};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

mod jobs;
pub(crate) use jobs::{Phase, VersionJob, VersionJobs};
use jobs::VersionStatus;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TranscriptAudioSource {
    Recording,
    Participants,
    Combined,
}

#[derive(Deserialize)]
pub(crate) struct TranscriptVersionOptions {
    pub(crate) source: TranscriptAudioSource,
    pub(crate) model: Option<String>,
    pub(crate) language: Option<String>,
}

pub(crate) struct ResolvedVersionOptions {
    pub(crate) source: TranscriptAudioSource,
    pub(crate) model: String,
    pub(crate) language: Option<String>,
}

fn missing() -> UiMessage {
    UiMessage::app(
        "native.transcriptVersionUnavailable",
        "This transcript version is unavailable.",
    )
}

pub(crate) fn no_speech() -> UiMessage {
    UiMessage::app(
        "native.retranscriptionEmpty",
        "Re-transcription found no speech. The saved transcript was kept.",
    )
}

fn model_unavailable() -> UiMessage {
    UiMessage::app(
        "native.transcriptVersionModelUnavailable",
        "Download the selected Whisper model in Settings and try again.",
    )
}

fn version_busy() -> UiMessage {
    UiMessage::app(
        "native.transcriptVersionBusy",
        "Wait for recording or processing to finish, then try again.",
    )
}

fn cancelled() -> UiMessage {
    UiMessage::app("native.transcriptVersionCancelled", "Transcript version generation cancelled.")
}

fn valid_meeting(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|value| !value.is_nil() && value.to_string() == id)
}

pub(crate) fn resolve_options(
    runtime: &RuntimeState,
    options: TranscriptVersionOptions,
) -> CmdResult<ResolvedVersionOptions> {
    let settings = runtime.settings.read();
    let tier = settings.tier(&runtime.hardware);
    let (live_id, quality_id, _) = settings.model_ids(&runtime.catalog, tier);
    let available = |id: &str| {
        runtime
            .catalog
            .get(id)
            .is_some_and(|model| model.kind == bazza_models::ModelKind::Asr)
            && runtime.model_file(id).is_some()
    };
    let model = match options.model {
        Some(model) => model,
        None if settings.quality_asr_model.is_some() || available(&quality_id) => quality_id,
        None if available(&live_id) => live_id,
        None => runtime
            .catalog
            .all()
            .iter()
            .find(|model| available(&model.id))
            .map(|model| model.id.clone())
            .ok_or_else(model_unavailable)?,
    };
    if !available(&model) {
        return Err(model_unavailable());
    }
    let language = match options.language {
        None => settings.language.clone(),
        Some(value) if value == "auto" => None,
        Some(value)
            if (2..=3).contains(&value.len())
                && value.bytes().all(|byte| byte.is_ascii_lowercase()) =>
        {
            Some(value)
        }
        Some(_) => {
            return Err(UiMessage::app(
                "native.transcriptVersionLanguage",
                "Choose a supported transcription language.",
            ))
        }
    };
    Ok(ResolvedVersionOptions {
        source: options.source,
        model,
        language,
    })
}

pub(crate) fn compute_mix(
    app: &AppHandle,
    runtime: &RuntimeState,
    guard: &MediaReadGuard,
    meeting_id: &str,
    model: &str,
    language: Option<&str>,
    job: &VersionJob,
) -> CmdResult<Vec<NewSegment>> {
    let _busy = runtime.busy_guard();
    job.checkpoint()?;
    job.update(app, meeting_id, Phase::Reading, None);
    let audio = guard
        .open_playback_file(PlaybackMediaKind::Audio)
        .map_err(|_| missing())?;
    let pcm =
        bazza_capture::wav::read_mono_resampled_from(BufReader::new(audio), bazza_capture::ASR_HZ)
            .map_err(err)?;
    job.checkpoint()?;
    job.update(app, meeting_id, Phase::Loading, None);
    let engine = runtime.whisper(model).ok_or_else(model_unavailable)?;
    job.update(app, meeting_id, Phase::Recognizing, None);
    let captions =
        bazza_asr::mix_prefix::transcribe_with_control(&engine, &pcm, language, Some(&job.cancel), |percent| {
            job.update(app, meeting_id, Phase::Recognizing, Some(percent));
        })
        .map_err(err)?;
    job.checkpoint()?;
    let mut rows: Vec<_> = captions
        .into_iter()
        .filter(|row| {
            row.is_final
                && row.start_ms >= 0
                && row.end_ms >= row.start_ms
                && !row.text.trim().is_empty()
        })
        .map(|row| NewSegment {
            start_ms: row.start_ms,
            end_ms: row.end_ms,
            speaker: None,
            text: row.text,
        })
        .collect();
    if !rows.is_empty() && runtime.settings.read().diarize_enabled {
        if let Some(state) = app.try_state::<AppState>() {
            job.update(app, meeting_id, Phase::Aligning, None);
            crate::pipeline::label_version_rows(
                app,
                meeting_id,
                &state.store.recordings_dir(meeting_id),
                &mut rows,
            );
        }
    }
    Ok(rows)
}

pub(crate) fn track_version_source(source: Source) -> TranscriptVersionSource {
    match source {
        Source::Meet => TranscriptVersionSource::Meet,
        Source::Discord => TranscriptVersionSource::Discord,
    }
}

pub(crate) fn participant_label(source: Source) -> TranscriptParticipantSource {
    match source {
        Source::Meet => TranscriptParticipantSource::Meet,
        Source::Discord => TranscriptParticipantSource::Discord,
    }
}

#[tauri::command]
pub(crate) async fn transcript_versions(
    app: AppHandle,
    id: String,
) -> CmdResult<TranscriptVersions> {
    if !valid_meeting(&id) {
        return Err(missing());
    }
    if app.try_state::<RepositoryClient>().is_some() {
        return crate::managed_transcript_versions::list(&app, &id).await;
    }
    blocking(move || {
        let state = app.state::<AppState>();
        let meeting = state
            .store
            .get_meeting(&id)
            .map_err(err)?
            .ok_or_else(missing)?;
        let mut list = state.store.transcript_versions(&id).map_err(err)?;
        if !matches!(meeting.status, MeetingStatus::Recording) {
            let root = crate::attendee_settings::library_root(&app)?;
            if let Ok(guard) = MediaReadGuard::acquire(&root, &id) {
                list.participant_source =
                    bot_transcript::participant_source(&guard, meeting.app.as_deref())
                        .ok()
                        .flatten()
                        .map(participant_label);
            }
        }
        Ok(list)
    })
    .await
}

#[tauri::command]
pub(crate) async fn transcript_version(
    app: AppHandle,
    id: String,
    version_id: String,
) -> CmdResult<Vec<TranscriptVersionSegment>> {
    if !valid_meeting(&id) {
        return Err(missing());
    }
    if app.try_state::<RepositoryClient>().is_some() {
        return crate::managed_transcript_versions::get(&app, &id, &version_id).await;
    }
    blocking(move || {
        app.state::<AppState>()
            .store
            .transcript_version(&id, &version_id)
            .map_err(err)?
            .ok_or_else(missing)
    })
    .await
}

#[tauri::command]
pub(crate) async fn regenerate_transcript(
    app: AppHandle,
    id: String,
    options: TranscriptVersionOptions,
) -> CmdResult<TranscriptVersion> {
    if !valid_meeting(&id) {
        return Err(missing());
    }
    let lease = app.state::<VersionJobs>().start(&id)?;
    tauri::async_runtime::spawn(async move {
        let job = &lease.0;
        let result = regenerate(&app, &id, options, job).await;
        let result = if result.is_err() { job.checkpoint().and(result) } else { result };
        lease.finish(&result);
        result
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub(crate) fn transcript_version_job(id: String, jobs: State<'_, VersionJobs>) -> Option<VersionStatus> {
    jobs.status(&id)
}

#[tauri::command]
pub(crate) fn cancel_transcript_version(app: AppHandle, id: String, job_id: Uuid) -> bool {
    let Some(queued) = app.state::<VersionJobs>().cancel(&id, job_id) else { return false; };
    if queued {
        if let Some(state) = app.try_state::<AppState>() {
            state.processing_jobs.cancel_pending(job_id);
        }
    }
    true
}

async fn regenerate(
    app: &AppHandle,
    id: &str,
    options: TranscriptVersionOptions,
    job: &Arc<VersionJob>,
) -> CmdResult<TranscriptVersion> {
    job.checkpoint()?;
    if app.try_state::<RepositoryClient>().is_some() {
        return crate::managed_transcript_versions::regenerate(app, id, options, job).await;
    }
    let (send, received) = tokio::sync::oneshot::channel();
    {
        let state = app.state::<AppState>();
        let meeting = state
            .store
            .get_meeting(id)
            .map_err(err)?
            .ok_or_else(missing)?;
        if !matches!(meeting.status, MeetingStatus::Ready | MeetingStatus::Failed) {
            return Err(version_busy());
        }
        let options = resolve_options(&state, options)?;
        let id = id.to_owned();
        let worker_job = Arc::clone(job);
        crate::processing_jobs::enqueue_tagged(app, id.clone(), job.id(), move |app| {
            let _ = send.send(regenerate_legacy(app, &id, options, &worker_job));
        })
        .map_err(err)?;
        if job.checkpoint().is_err() {
            state.processing_jobs.cancel_pending(job.id());
        }
    }
    received.await.map_err(err)?
}

fn regenerate_legacy(
    app: &AppHandle,
    id: &str,
    options: ResolvedVersionOptions,
    job: &VersionJob,
) -> CmdResult<TranscriptVersion> {
    job.checkpoint()?;
    let state = app.state::<AppState>();
    let meeting = state
        .store
        .get_meeting(id)
        .map_err(err)?
        .ok_or_else(missing)?;
    let root = crate::attendee_settings::library_root(app)?;
    let guard = MediaReadGuard::acquire(&root, id).map_err(|_| missing())?;
    if options.source == TranscriptAudioSource::Combined {
        let version = crate::bot_supplement::compute(app, &state, &guard, id,
            &options.model, options.language.as_deref(), job)?;
        job.begin_save(app, id)?;
        return state.store.save_transcript_version(id, &version).map_err(err);
    }
    let (source, segments) = match options.source {
        TranscriptAudioSource::Recording => (
            TranscriptVersionSource::Recording,
            compute_mix(
                app,
                &state,
                &guard,
                id,
                &options.model,
                options.language.as_deref(),
                job,
            )?
            .into_iter()
            .map(Into::into)
            .collect(),
        ),
        TranscriptAudioSource::Participants => {
            let source = bot_transcript::participant_source(&guard, meeting.app.as_deref())?
                .ok_or_else(missing)?;
            let rows = bot_transcript::compute_for_version(
                app,
                &state,
                &guard,
                id,
                source,
                &options.model,
                options.language.as_deref(),
                job,
            )?;
            (track_version_source(source), rows)
        }
        TranscriptAudioSource::Combined => unreachable!(),
    };
    if segments.is_empty() {
        return Err(no_speech());
    }
    job.begin_save(app, id)?;
    state
        .store
        .save_transcript_version(
            id,
            &NewTranscriptVersion {
                id: Uuid::new_v4().to_string(),
                source,
                model: options.model,
                language: options.language,
                segments,
                partial: false,
            },
        )
        .map_err(err)
}

/// Runs within the existing post-call queue, before Ready-only edits and AI.
pub(crate) fn supplement_legacy(app: &AppHandle, id: &str) -> CmdResult<bool> {
    let state = app.state::<AppState>();
    let versions = state.store.transcript_versions(id).map_err(err)?;
    if crate::bot_supplement::has_result(&versions) {
        return Ok(crate::bot_supplement::published_result(&versions));
    }
    let lease = app.state::<VersionJobs>().start(id)?;
    let result = (|| {
        let options = resolve_options(&state, TranscriptVersionOptions {
            source: TranscriptAudioSource::Combined, model: None, language: None,
        })?;
        regenerate_legacy(app, id, options, &lease.0)
    })();
    lease.finish(&result);
    let version = result?;
    let versions = state.store.transcript_versions(id).map_err(err)?;
    Ok(versions.effective_id.as_deref() == Some(version.id.as_str()))
}

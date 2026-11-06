use super::{err, CmdResult};
use crate::recorder;
use crate::state::AppState;
use bazza_service_client::RepositoryClient;
use bazza_store::{Meeting, MeetingSpeaker, SearchHit, Segment, SpeakerStat, TranscriptCorrectionError};
use serde::Serialize;
use std::path::Path;
use tauri::{AppHandle, Manager, State, WebviewWindow};

#[path = "meeting_repository.rs"]
mod meeting_repository;
pub use meeting_repository::MeetingCommandRow;
#[path = "meeting_playback.rs"]
mod meeting_playback;
pub(crate) use meeting_playback::{serve_service_playback, ServicePlayback};

#[derive(Default, Serialize)]
pub struct Media {
    pub audio: Option<String>,
    pub video: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease: Option<uuid::Uuid>,
}

fn ordinary_path(path: &Path, directory: bool) -> bool {
    path.symlink_metadata().is_ok_and(|metadata| {
        let linked = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            linked || metadata.file_attributes() & 0x400 != 0
        };
        !linked && if directory { metadata.is_dir() } else { metadata.is_file() }
    })
}

fn meeting_media(dir: &Path, has_video: bool, mut allow_file: impl FnMut(&Path) -> CmdResult<()>) -> CmdResult<Media> {
    // A read of a text-only or missing-media meeting must not create a recording.
    // Never publish a media leaf through a linked/reparse recording directory.
    let available = ordinary_path(dir, true)
        && dir.parent().is_some_and(|parent| ordinary_path(parent, true));
    let audio = dir.join("mix.wav");
    let audio = (available && ordinary_path(&audio, false)).then_some(audio);
    let video = dir.join("screen.mp4");
    // Recovery may retain a damaged MP4 for diagnosis while publishing audio.
    let video = (available && has_video && ordinary_path(&video, false)).then_some(video);
    // Windows package virtualization can resolve a file into a different physical
    // directory than its parent. Grant each existing leaf so Tauri also records
    // its canonical path, rather than relying only on the startup directory scope.
    for path in audio.iter().chain(video.as_ref()) {
        allow_file(path)?;
    }
    Ok(Media {
        audio: audio.map(|path| path.to_string_lossy().into_owned()),
        video: video.map(|path| path.to_string_lossy().into_owned()),
        lease: None,
    })
}

#[derive(Serialize)]
pub struct MeetingDetail {
    pub meeting: MeetingCommandRow,
    pub segments: Vec<Segment>,
    pub media: Media,
    pub highlights: Vec<u64>,
    pub speakers: Vec<MeetingSpeaker>,
    /// One row per speaker label actually used in the transcript, first-heard order.
    pub speaker_stats: Vec<SpeakerStat>,
}

#[tauri::command]
pub async fn list_meetings(app: AppHandle) -> CmdResult<Vec<MeetingCommandRow>> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        return meeting_repository::list(&repository).await;
    }
    super::blocking(move || {
        let state = app.state::<AppState>();
        let meetings = state.store.list_meetings().map_err(err)?;
        Ok(meetings.into_iter().map(MeetingCommandRow::legacy).collect())
    }).await
}

pub(super) async fn list_service_meetings_filtered(
    repository: &RepositoryClient,
    filter: bazza_store::MeetingFilter,
) -> CmdResult<Vec<MeetingCommandRow>> {
    use bazza_service_client::api::desktop::{DesktopFolderFilter, DesktopMeetingFilter};
    let folder = match filter.folder {
        None => DesktopFolderFilter::All,
        Some(None) => DesktopFolderFilter::Unfiled,
        Some(Some(folder_id)) => DesktopFolderFilter::Subtree { folder_id },
    };
    meeting_repository::list_filtered(repository, DesktopMeetingFilter {
        folder,
        tag_id: filter.tag_id,
        content_kind: None,
    }).await
}

#[tauri::command]
pub async fn get_meeting(app: AppHandle, window: WebviewWindow, id: String) -> CmdResult<MeetingDetail> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        let playback = app.try_state::<ServicePlayback>()
            .filter(|_| window.label() == "main")
            .ok_or_else(|| crate::ui_message::app(
                "native.serviceMeetingDetailUnavailable",
                "Meeting details are unavailable in this window.",
            ))?;
        let first = meeting_repository::detail(&repository, &playback, id.clone()).await;
        return match first {
            Err(error) if error.code == "native.serviceMeetingRefreshRequired" => {
                meeting_repository::detail(&repository, &playback, id).await
            }
            result => result,
        };
    }
    super::blocking(move || {
        let state = app.state::<AppState>();
        let meeting = state.store.get_meeting(&id).map_err(err)?.ok_or_else(|| crate::ui_message::app("native.meetingNotFound", "meeting not found"))?;
        let segments = state.store.segments(&id).map_err(err)?;
        // Meeting IDs from old/imported libraries are data, never path syntax.
        let safe_media_id = !id.is_empty() && !matches!(id.as_str(), "." | "..")
            && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c));
        let dir = state.store.root().join("recordings").join(&id);
        let scope = app.asset_protocol_scope();
        let media = if safe_media_id {
            meeting_media(&dir, meeting.has_video, |path| scope.allow_file(path).map_err(err))?
        } else {
            Media::default()
        };
        let highlights = state.store.get_setting::<Vec<u64>>(&format!("highlights:{id}")).map_err(err)?.unwrap_or_default();
        let speakers = state.store.meeting_speakers(&id).map_err(err)?;
        let speaker_stats = state.store.speaker_stats(&id).map_err(err)?;
        Ok(MeetingDetail { meeting: MeetingCommandRow::legacy(meeting), segments, media, highlights, speakers, speaker_stats })
    }).await
}

#[tauri::command]
pub(crate) fn release_meeting_media(window: WebviewWindow, playback: State<'_, ServicePlayback>, lease: uuid::Uuid) {
    if window.label() == "main" {
        playback.release(lease);
    }
}

fn correction_error(error: TranscriptCorrectionError) -> crate::ui_message::UiMessage {
    use crate::ui_message::app;
    match error {
        TranscriptCorrectionError::MeetingNotFound => app("native.meetingNotFound", "meeting not found"),
        TranscriptCorrectionError::NotReady => app("native.transcriptCorrectionNotReady", "Finish processing before editing the transcript."),
        TranscriptCorrectionError::SegmentNotFound => app("native.transcriptCorrectionSegment", "This transcript segment is unavailable or belongs to another meeting."),
        TranscriptCorrectionError::TooLarge => app("native.transcriptCorrectionTooLarge", "Transcript correction exceeds 65536 UTF-8 bytes.")
            .with_args(serde_json::json!({"maxBytes":bazza_store::MAX_TRANSCRIPT_CORRECTION_BYTES})),
        TranscriptCorrectionError::Invalid => app("native.transcriptCorrectionInvalid", "Transcript correction cannot contain a NUL character."),
        TranscriptCorrectionError::Sqlite(error) => app("native.transcriptCorrectionSaveFailed", "Could not save the transcript correction.").with_detail(error.to_string()),
    }
}

/// User corrections update only the cleaned representation. Local retrieval
/// refresh never calls an LLM or a subscription agent, including on missing models.
#[tauri::command]
pub async fn set_transcript_correction(app: AppHandle, id: String, segment_id: i64, text: Option<String>) -> CmdResult<Segment> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _busy = state.busy_guard();
        let segment = state.store.set_transcript_correction(&id, segment_id, text.as_deref()).map_err(correction_error)?;
        if let Err(error) = crate::embed_stage::embed_meeting(&app, &id) {
            log::warn!("embedding after transcript correction failed: {error}");
            crate::recorder::notice_message(&app, "warning", crate::ui_message::app(
                "native.transcriptCorrectionEmbeddingFailed", "Transcript correction saved, but semantic search could not be refreshed.",
            ).with_detail(error));
        }
        Ok(segment)
    }).await.map_err(|error| crate::ui_message::app("native.transcriptCorrectionSaveFailed", "Could not save the transcript correction.").with_detail(error.to_string()))?
}

#[tauri::command]
pub async fn rename_meeting(
    app: AppHandle,
    id: String,
    title: String,
    expected_metadata_revision: Option<String>,
    idempotency_key: Option<String>,
) -> CmdResult<MeetingCommandRow> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        return meeting_repository::rename(
            &repository,
            id,
            title,
            expected_metadata_revision,
            idempotency_key,
        )
        .await;
    }
    super::blocking(move || {
        let state = app.state::<AppState>();
        let mut m = state.store.get_meeting(&id).map_err(err)?.ok_or_else(|| crate::ui_message::app("native.meetingNotFound", "meeting not found"))?;
        m.title = title.trim().to_string();
        if m.title.is_empty() {
            return Err(crate::ui_message::app("native.titleEmpty", "title cannot be empty"));
        }
        m.title_auto = false;
        state.store.update_meeting(&m).map_err(err)?;
        Ok(MeetingCommandRow::legacy(m))
    }).await
}

fn require_recording_uuid(id: &str) -> CmdResult<()> {
    if !uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) {
        return Err(crate::ui_message::app("native.meetingIdInvalid", "invalid meeting ID"));
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_meeting(
    app: AppHandle,
    id: String,
    expected_lifecycle_revision: Option<String>,
    idempotency_key: Option<String>,
) -> CmdResult<()> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        require_recording_uuid(&id)?;
        return meeting_repository::delete(
            &repository,
            id,
            expected_lifecycle_revision,
            idempotency_key,
        )
        .await;
    }
    super::blocking(move || {
        let state = app.state::<AppState>();
        let _copy = state.data_copy.try_read().ok_or_else(|| err("A library copy is in progress."))?;
        require_recording_uuid(&id)?;
        let lifecycle = crate::recorder::try_recording_lifecycle().ok_or_else(|| {
            crate::ui_message::app("native.deleteRecording", "Cannot delete a meeting while recording is starting or stopping.")
        })?;
        let _reservation = state.processing_jobs.reserve(&id).map_err(err)?;
        if state.recording.lock().as_ref().map(|a| a.meeting.id == id).unwrap_or(false) {
            return Err(crate::ui_message::app("native.deleteRecording", "cannot delete a meeting that is being recorded"));
        }
        state.store.delete_meeting(&id).map_err(err)?;
        drop(lifecycle);
        if let Some(job) = state.summary_cancels.lock().get(&id) {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let dir = state.store.root().join("recordings").join(&id);
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }).await
}

#[tauri::command]
pub async fn search(app: AppHandle, query: String, limit: Option<usize>) -> CmdResult<Vec<SearchHit>> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        state.store.search(&query, limit.unwrap_or(50)).map_err(err)
    }).await
}

#[tauri::command]
pub async fn retranscribe(app: AppHandle, id: String, model: Option<String>) -> CmdResult<()> {
    use crate::transcript_versions::{regenerate_transcript, TranscriptAudioSource, TranscriptVersionOptions};
    regenerate_transcript(app, id, TranscriptVersionOptions {
        source: TranscriptAudioSource::Recording, model, language: None,
    }).await.map(|_| ())
}

/// Import an audio/video file as a new meeting and process it like a recording.
#[tauri::command]
pub async fn import_recording(app: AppHandle, path: String, title: Option<String>, transcript_path: Option<String>) -> CmdResult<Meeting> {
    tauri::async_runtime::spawn_blocking(move || crate::import::import(&app, std::path::Path::new(&path), title.as_deref(), transcript_path.as_deref().map(std::path::Path::new)))
        .await
        .map_err(super::err)?
}

/// Calendar events from 10 minutes ago to `hours` ahead (default 12).
#[tauri::command]
pub fn list_upcoming(state: State<AppState>, hours: Option<i64>) -> Vec<bazza_calendar::CalEvent> {
    crate::calendar::upcoming(&state, hours.unwrap_or(12))
}

/// Re-fetch every calendar source now; returns per-source errors.
#[tauri::command]
pub async fn refresh_calendar(app: AppHandle) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || crate::calendar::refresh(&app)).await.map_err(super::err)
}

#[tauri::command]
pub fn rediarize(app: AppHandle, id: String) -> CmdResult<()> {
    crate::processing_jobs::enqueue(&app, id.clone(), move |app| {
        let _ = crate::pipeline::rediarize(app, &id);
    }).map_err(err)
}

#[tauri::command]
pub fn reembed(app: AppHandle, id: String) -> CmdResult<()> {
    crate::processing_jobs::enqueue(&app, id.clone(), move |app| {
        let _ = crate::pipeline::reembed(app, &id);
    }).map_err(err)
}

#[cfg(test)]
mod media_tests {
    use super::*;

    #[test]
    fn grants_each_existing_published_media_file() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["mix.wav", "screen.mp4", "screen.raw.mp4"] {
            std::fs::write(dir.path().join(name), b"media").unwrap();
        }
        let mut granted = Vec::new();
        let media = meeting_media(dir.path(), true, |path| {
            granted.push(path.to_path_buf());
            Ok(())
        }).unwrap();
        assert_eq!(granted, vec![dir.path().join("mix.wav"), dir.path().join("screen.mp4")]);
        assert_eq!(media.audio.as_deref(), granted[0].to_str());
        assert_eq!(media.video.as_deref(), granted[1].to_str());
    }

    #[test]
    fn recovered_or_missing_video_is_not_published_or_granted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mix.wav"), b"audio").unwrap();
        std::fs::write(dir.path().join("screen.mp4"), b"damaged video").unwrap();
        let mut granted = Vec::new();
        let media = meeting_media(dir.path(), false, |path| {
            granted.push(path.to_path_buf());
            Ok(())
        }).unwrap();
        assert!(media.video.is_none());
        assert_eq!(granted, vec![dir.path().join("mix.wav")]);

        let missing = tempfile::tempdir().unwrap();
        let media = meeting_media(missing.path(), true, |_| panic!("missing files must not be granted")).unwrap();
        assert!(media.audio.is_none());
        assert!(media.video.is_none());
    }

    #[test]
    fn absent_directory_stays_absent_and_video_only_is_readable() {
        let root = tempfile::tempdir().unwrap();
        let absent = root.path().join("recordings").join("text-only");
        let media = meeting_media(&absent, true, |_| panic!("absent media must not be granted")).unwrap();
        assert!(media.audio.is_none() && media.video.is_none());
        assert!(!root.path().join("recordings").exists());
        let video_dir = root.path().join("video-only");
        std::fs::create_dir(&video_dir).unwrap();
        std::fs::write(video_dir.join("screen.mp4"), b"published video").unwrap();
        let mut granted = Vec::new();
        let media = meeting_media(&video_dir, true, |path| { granted.push(path.to_path_buf()); Ok(()) }).unwrap();
        assert!(media.audio.is_none());
        assert_eq!(media.video.as_deref(), video_dir.join("screen.mp4").to_str());
        assert_eq!(granted, [video_dir.join("screen.mp4")]);
    }

    #[test]
    fn scope_failure_is_returned_to_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mix.wav"), b"audio").unwrap();
        assert!(meeting_media(dir.path(), false, |_| Err(err("media scope failed"))).is_err());
    }
}

#[cfg(test)]
mod correction_tests {
    use super::*;

    #[test]
    fn correction_errors_have_stable_codes_and_utf8_byte_limit() {
        for (error, code) in [
            (TranscriptCorrectionError::NotReady,"native.transcriptCorrectionNotReady"),
            (TranscriptCorrectionError::SegmentNotFound,"native.transcriptCorrectionSegment"),
            (TranscriptCorrectionError::Invalid,"native.transcriptCorrectionInvalid"),
            (TranscriptCorrectionError::MeetingNotFound,"native.meetingNotFound"),
        ] { assert_eq!(correction_error(error).code, code); }
        let limit = correction_error(TranscriptCorrectionError::TooLarge);
        assert_eq!(limit.code, "native.transcriptCorrectionTooLarge");
        assert_eq!(limit.args["maxBytes"], 65_536);
    }
}

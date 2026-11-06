use super::{err, CmdResult};
use crate::state::AppState;
use bazza_capture::video::{ffmpeg_clip_args, run_ffmpeg};
use bazza_share::{export_bundle, import_bundle, ImportReport, ShareServer};
use bazza_service_client::RepositoryClient;
use bazza_store::{Folder, MeetingFilter, Share, Tag};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

// ---- folders & tags ----

#[tauri::command]
pub async fn list_folders(app: AppHandle) -> CmdResult<Vec<Folder>> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        state.store.list_folders().map_err(err)
    }).await
}

#[tauri::command]
pub fn create_folder(state: State<AppState>, name: String, parent_id: Option<String>) -> CmdResult<Folder> {
    if name.trim().is_empty() {
        return Err(crate::ui_message::app("native.folderName", "folder needs a name"));
    }
    state.store.create_folder(&name, parent_id.as_deref()).map_err(err)
}

#[tauri::command]
pub fn update_folder(state: State<AppState>, id: String, name: String, parent_id: Option<String>, default_template_id: Option<String>) -> CmdResult<()> {
    state.store.update_folder(&id, &name, parent_id.as_deref(), default_template_id.as_deref()).map_err(err)
}

#[tauri::command]
pub fn delete_folder(state: State<AppState>, id: String) -> CmdResult<()> {
    state.store.delete_folder(&id).map_err(err)
}

#[tauri::command]
pub fn move_meeting(state: State<AppState>, id: String, folder_id: Option<String>) -> CmdResult<()> {
    state.store.move_meeting(&id, folder_id.as_deref()).map_err(err)
}

#[tauri::command]
pub async fn list_meetings_filtered(app: AppHandle, filter: MeetingFilter) -> CmdResult<Vec<super::meetings::MeetingCommandRow>> {
    if let Some(repository) = app.try_state::<RepositoryClient>() {
        return super::meetings::list_service_meetings_filtered(&repository, filter).await;
    }
    super::blocking(move || {
        let state = app.state::<AppState>();
        let meetings = state.store.list_meetings_filtered(&filter).map_err(err)?;
        Ok(meetings.into_iter().map(super::meetings::MeetingCommandRow::legacy).collect())
    }).await
}

#[tauri::command]
pub async fn list_tags(app: AppHandle) -> CmdResult<Vec<Tag>> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        state.store.list_tags().map_err(err)
    }).await
}

#[tauri::command]
pub fn meeting_tags(state: State<AppState>, id: String) -> CmdResult<Vec<Tag>> {
    state.store.meeting_tags(&id).map_err(err)
}

#[tauri::command]
pub fn tag_meeting(state: State<AppState>, id: String, name: String) -> CmdResult<Tag> {
    if name.trim().is_empty() {
        return Err(crate::ui_message::app("native.tagName", "tag needs a name"));
    }
    state.store.tag_meeting(&id, &name).map_err(err)
}

#[tauri::command]
pub fn untag_meeting(state: State<AppState>, id: String, tag_id: String) -> CmdResult<()> {
    state.store.untag_meeting(&id, &tag_id).map_err(err)
}

// ---- clips ----

fn source_media(state: &AppState, meeting_id: &str) -> CmdResult<(PathBuf, bool)> {
    let dir = state.store.recordings_dir(meeting_id);
    let video = dir.join("screen.mp4");
    if video.exists() {
        Ok((video, true))
    } else {
        let audio = dir.join("mix.wav");
        if audio.exists() {
            Ok((audio, false))
        } else {
            Err(crate::ui_message::app("native.mediaMissing", "no media for this meeting"))
        }
    }
}

/// Cut a clip to `out` (extension decides mp4 or mp3) and write a `.txt` transcript beside it.
#[tauri::command]
pub async fn export_clip(app: AppHandle, id: String, start_ms: i64, end_ms: i64, out: String) -> CmdResult<String> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        if end_ms <= start_ms {
            return Err(crate::ui_message::app("native.clipInterval", "clip end must be after start"));
        }
        let ffmpeg = state.ffmpeg.as_ref().ok_or_else(|| crate::ui_message::app("native.ffmpegMissing", "ffmpeg not found"))?;
        let (src, _) = source_media(&state, &id)?;
        let out_path = PathBuf::from(&out);
        run_ffmpeg(ffmpeg, &ffmpeg_clip_args(&src, start_ms as f64 / 1000.0, end_ms as f64 / 1000.0, &out_path))?;
        let segs = state.store.segments(&id).map_err(err)?;
        let text: String = segs
            .iter()
            .filter(|s| s.end_ms >= start_ms && s.start_ms <= end_ms)
            .map(|s| bazza_llm::summary::transcript_line(s.start_ms - start_ms, s.speaker.as_deref(), s.clean_text.as_deref().unwrap_or(&s.text)))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(out_path.with_extension("txt"), text);
        Ok(out)
    }).await
}

// ---- shares ----

#[derive(Serialize)]
pub struct ShareInfo {
    pub share: Share,
    pub url: String,
}

fn ensure_server(state: &AppState) -> Result<u16, String> {
    let mut guard = state.share_server.lock();
    if let Some(h) = guard.as_ref() {
        return Ok(h.port);
    }
    let port = state.settings.read().share_port;
    let clips = state.store.root().join("clips");
    let _ = std::fs::create_dir_all(&clips);
    let h = ShareServer::start(state.store.clone(), clips, port).map_err(|e| format!("start share server on port {port}: {e}"))?;
    let p = h.port;
    *guard = Some(h);
    Ok(p)
}

fn stop_server_if_idle(state: &AppState) {
    let active = state.store.list_shares().map(|s| s.iter().any(|x| x.enabled)).unwrap_or(false);
    if !active {
        let server = state.share_server.lock().take();
        if let Some(h) = server {
            h.stop();
        }
    }
}

/// Create a share link for the whole meeting or a clip (rendered with ffmpeg into `clips/`).
#[tauri::command]
pub async fn create_share(app: AppHandle, id: String, start_ms: Option<i64>, end_ms: Option<i64>) -> CmdResult<ShareInfo> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        let _share_operation = state.share_ops.lock();
        let kind = if start_ms.is_some() && end_ms.is_some() { "clip" } else { "meeting" };
        let share = state.store.create_share(&id, kind, start_ms, end_ms).map_err(err)?;
        if kind == "clip" {
            let ffmpeg = state.ffmpeg.as_ref().ok_or_else(|| crate::ui_message::app("native.ffmpegMissing", "ffmpeg not found"))?;
            let (src, is_video) = source_media(&state, &id)?;
            let clips = state.store.root().join("clips");
            let _ = std::fs::create_dir_all(&clips);
            let out = clips.join(format!("{}.{}", share.token, if is_video { "mp4" } else { "mp3" }));
            if let Err(e) = run_ffmpeg(ffmpeg, &ffmpeg_clip_args(&src, start_ms.unwrap() as f64 / 1000.0, end_ms.unwrap() as f64 / 1000.0, &out)) {
                let _ = state.store.delete_share(&share.token);
                return Err(crate::ui_message::app("native.clipFailed", format!("render clip: {e}")).with_detail(e));
            }
        }
        let port = ensure_server(&state)?;
        Ok(ShareInfo { url: bazza_share::server::share_url(port, &share.token), share })
    }).await
}

#[tauri::command]
pub fn list_shares(state: State<AppState>) -> CmdResult<Vec<ShareInfo>> {
    let port = state.share_server.lock().as_ref().map(|h| h.port).unwrap_or(state.settings.read().share_port);
    Ok(state.store.list_shares().map_err(err)?.into_iter().map(|s| ShareInfo { url: bazza_share::server::share_url(port, &s.token), share: s }).collect())
}

#[tauri::command]
pub async fn set_share_enabled(app: AppHandle, token: String, enabled: bool) -> CmdResult<()> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        let _share_operation = state.share_ops.lock();
        state.store.set_share_enabled(&token, enabled).map_err(err)?;
        if enabled {
            ensure_server(&state)?;
        } else {
            stop_server_if_idle(&state);
        }
        Ok(())
    }).await
}

#[tauri::command]
pub async fn delete_share(app: AppHandle, token: String) -> CmdResult<()> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        let _share_operation = state.share_ops.lock();
        state.store.delete_share(&token).map_err(err)?;
        let clips = state.store.root().join("clips");
        for ext in ["mp4", "mp3"] {
            let _ = std::fs::remove_file(clips.join(format!("{token}.{ext}")));
        }
        stop_server_if_idle(&state);
        Ok(())
    }).await
}

// ---- export / import ----

#[tauri::command]
pub async fn export_meetings(app: AppHandle, ids: Vec<String>, out: String, include_video: bool) -> CmdResult<usize> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ids = if ids.is_empty() { state.store.list_meetings().map_err(err)?.into_iter().map(|m| m.id).collect() } else { ids };
        let manifest = export_bundle(&state.store, &ids, Path::new(&out), include_video).map_err(err)?;
        Ok(manifest.meetings.len())
    }).await.map_err(err)?
}

#[tauri::command]
pub async fn import_meetings(app: AppHandle, path: String) -> CmdResult<ImportReport> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        import_bundle(&state.store, Path::new(&path)).map_err(err)
    }).await.map_err(err)?
}

/// Write a standalone HTML page next to a copy of the media.
/// Write text the UI rendered (transcript .txt/.md/.srt, summary) to a path the user picked.
#[tauri::command]
pub async fn save_text_file(path: String, text: String) -> CmdResult<()> {
    super::blocking(move || {
        let p = PathBuf::from(&path);
        if p.file_name().is_none() {
            return Err(crate::ui_message::app("native.fileNameMissing", "no file name"));
        }
        std::fs::write(&p, text).map_err(|e| crate::ui_message::app("native.fileWrite", format!("could not write {}: {e}", p.display())).with_args(serde_json::json!({"path": p.to_string_lossy()})).with_detail(e.to_string()))
    }).await
}

#[tauri::command]
pub async fn export_html(app: AppHandle, id: String, out_dir: String) -> CmdResult<String> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        let meeting = state.store.get_meeting(&id).map_err(err)?.ok_or_else(|| crate::ui_message::app("native.meetingNotFound", "meeting not found"))?;
        let segments = state.store.segments(&id).map_err(err)?;
        let summary = state.store.get_summary(&id).map_err(err)?;
        let (src, is_video) = source_media(&state, &id)?;
        let out_dir = PathBuf::from(&out_dir);
        std::fs::create_dir_all(&out_dir).map_err(err)?;
        let media_name = if is_video { "recording.mp4" } else { "recording.wav" };
        std::fs::copy(&src, out_dir.join(media_name)).map_err(err)?;
        let html = bazza_share::render_standalone(&bazza_share::html::PageData {
            meeting: &meeting,
            segments: &segments,
            summary: summary.as_ref(),
            media_url: Some(media_name),
            is_video,
            clip: None,
        });
        let page = out_dir.join("index.html");
        std::fs::write(&page, html).map_err(err)?;
        Ok(page.to_string_lossy().into_owned())
    }).await
}

/// Move the whole data folder. Copies everything, then writes a pointer file
/// in the default location so the next launch opens the new folder.
#[tauri::command]
pub async fn change_data_dir(app: AppHandle, new_dir: String) -> CmdResult<String> {
    super::blocking(move || {
        let state = app.state::<AppState>();
        let _copy = state.data_copy.try_write().ok_or_else(|| err("A library copy or an import is already in progress."))?;
        let lifecycle = crate::recorder::try_recording_lifecycle().ok_or_else(|| {
            crate::ui_message::app("native.stopRecordingFirst", "Wait for the current recording to finish.")
        })?;
        if !state.processing_jobs.is_idle() || state.busy.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            return Err(err("Wait for meeting processing to finish before copying the library."));
        }
        if state.recording.lock().is_some() {
            return Err(crate::ui_message::app("native.stopRecordingFirst", "stop the current recording first"));
        }
        drop(lifecycle);
        let from = state.store.root().to_path_buf();
        let to = PathBuf::from(new_dir.trim());
        if !prepare_copy_destination(&from, &to)? {
            return Ok(to.to_string_lossy().into_owned());
        }
        copy_library(&state.store, &to).map_err(|e| crate::ui_message::app("native.dataCopyFailed", format!("copy: {e}")).with_detail(e.to_string()))?;
        bazza_store::write_data_dir_pointer(&to).map_err(err)?;
        Ok(to.to_string_lossy().into_owned())
    }).await
}

/// Resolve existing ancestors so aliases cannot hide a copy into the source itself.
fn resolve_destination(path: &Path) -> CmdResult<PathBuf> {
    if path.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
        return Err(crate::ui_message::app("native.dataDestinationParent", "choose a folder without parent-directory (..) components"));
    }
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(name) = path.file_name() else { return Err(err(error)) };
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
            Ok(resolve_destination(parent)?.join(name))
        }
        Err(error) => Err(err(error)),
    }
}

/// Same folder is a no-op. A move must never merge libraries or recurse into itself.
fn prepare_copy_destination(from: &Path, to: &Path) -> CmdResult<bool> {
    let source = from.canonicalize().map_err(err)?;
    let destination = resolve_destination(to)?;
    if destination == source {
        return Ok(false);
    }
    if destination.starts_with(&source) {
        return Err(crate::ui_message::app("native.dataDestinationNested", "choose a folder outside the current data folder"));
    }
    if destination.exists() && std::fs::read_dir(&destination).map_err(err)?.next().is_some() {
        return Err(crate::ui_message::app("native.dataDestinationNotEmpty", "choose an empty folder so existing data is not overwritten"));
    }
    std::fs::create_dir_all(&destination).map_err(err)?;
    Ok(true)
}

/// SQLite creates a consistent renamed snapshot; the live source and WAL stay in place.
fn copy_library(store: &bazza_store::Store, to: &Path) -> bazza_store::Result<()> {
    // Keep another current-build opener out until snapshot and media copy finish.
    // The destination owns its own persistent lock; source coordination is not data.
    let _destination_owner = bazza_store::LibraryOwnerGuard::acquire_for_create(to)?;
    store.snapshot_to(&to.join("bazza.db"))?;
    copy_dir(store.root(), to, true)?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path, library_root: bool) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if library_root && [
            bazza_store::SERVICE_OWNER_LOCK_FILE,
            bazza_service_client::ENDPOINT_FILE,
            bazza_service_client::broker::BROKER_FILE,
            ".bazza-service-broker-setup.lock",
        ].iter().any(|name| library_file_name_is(&entry.file_name(), name)) {
            continue;
        }
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&dest)?;
            copy_dir(&entry.path(), &dest, false)?;
        } else if !["bazza.db", "bazza.db-wal", "bazza.db-shm"].iter().any(|name| library_file_name_is(&entry.file_name(), name)) {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn library_file_name_is(name: &std::ffi::OsStr, expected: &str) -> bool {
    #[cfg(windows)]
    { name.to_str().is_some_and(|name| name.eq_ignore_ascii_case(expected)) }
    #[cfg(not(windows))]
    { name == std::ffi::OsStr::new(expected) }
}

#[cfg(test)]
mod data_move_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_library_copy_preserves_wal_snapshot_with_case_variant_database_and_lock_names() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        drop(bazza_store::Store::open(&source.join("bazza.db")).unwrap());
        // Rename only closed, owned fixture files; no live SQLite file or lock is moved.
        std::fs::rename(source.join("bazza.db"), source.join("BAZZA.DB")).unwrap();
        std::fs::rename(source.join(bazza_store::SERVICE_OWNER_LOCK_FILE), source.join(".BAZZA-SERVICE-OWNER.LOCK")).unwrap();
        std::fs::write(source.join(".BAZZA-SERVICE-ENDPOINT.JSON"), b"stale discovery").unwrap();
        let store = bazza_store::Store::open(&source.join("BAZZA.DB")).unwrap();
        let value = "uncheckpointed original ".repeat(4_000);
        store.set_setting("case-variant-wal", &value).unwrap();
        assert!(source.join("bazza.db-wal").metadata().unwrap().len() > 0);
        assert!(prepare_copy_destination(&source, &destination).unwrap());
        copy_library(&store, &destination).unwrap();
        assert!(!destination.join(".BAZZA-SERVICE-ENDPOINT.JSON").exists());
        let copied = bazza_store::Store::open(&destination.join("bazza.db")).unwrap();
        assert_eq!(copied.get_setting::<String>("case-variant-wal").unwrap().as_deref(), Some(value.as_str()));
        assert_eq!(store.get_setting::<String>("case-variant-wal").unwrap().as_deref(), Some(value.as_str()));
    }

    #[test]
    fn moving_library_snapshots_uncheckpointed_data_and_preserves_the_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Bazza");
        let destination = root.path().join("Bazza-moved");
        let store = bazza_store::Store::open(&source.join("bazza.db")).unwrap();
        store.set_setting("rename-marker", &"existing library").unwrap();
        let recordings = source.join("recordings/fixture");
        std::fs::create_dir_all(&recordings).unwrap();
        std::fs::write(recordings.join("mix.wav"), b"saved media").unwrap();
        for name in [bazza_service_client::ENDPOINT_FILE, bazza_service_client::broker::BROKER_FILE, ".bazza-service-broker-setup.lock"] {
            std::fs::write(source.join(name), b"source-only coordination").unwrap();
        }
        // An identical filename inside meeting data remains ordinary copied data.
        std::fs::write(recordings.join(bazza_service_client::ENDPOINT_FILE), b"meeting attachment").unwrap();
        let wal = source.join("bazza.db-wal");
        assert!(wal.metadata().unwrap().len() > 0);

        assert!(prepare_copy_destination(&source, &destination).unwrap());
        copy_library(&store, &destination).unwrap();
        assert!(wal.is_file());
        assert_eq!(store.get_setting::<String>("rename-marker").unwrap().as_deref(), Some("existing library"));
        assert!(source.join("bazza.db").is_file());
        assert!(destination.join("bazza.db").is_file());
        for name in ["bazza.db-wal", "bazza.db-shm"] {
            assert!(!destination.join(name).exists(), "copied stale {name}");
        }
        for name in [bazza_service_client::ENDPOINT_FILE, bazza_service_client::broker::BROKER_FILE, ".bazza-service-broker-setup.lock"] {
            assert!(!destination.join(name).exists(), "copied stale coordination {name}");
            assert_eq!(std::fs::read(source.join(name)).unwrap(), b"source-only coordination");
        }
        assert_eq!(std::fs::read(destination.join("recordings/fixture").join(bazza_service_client::ENDPOINT_FILE)).unwrap(), b"meeting attachment");
        let copied = bazza_store::Store::open(&bazza_store::database_path(&destination)).unwrap();
        assert_eq!(copied.get_setting::<String>("rename-marker").unwrap().as_deref(), Some("existing library"));
        store.set_setting("rename-marker", &"subsequent source change").unwrap();
        assert_eq!(copied.get_setting::<String>("rename-marker").unwrap().as_deref(), Some("existing library"));
        assert_eq!(std::fs::read(destination.join("recordings/fixture/mix.wav")).unwrap(), b"saved media");
    }

    #[test]
    fn conflicting_library_and_nested_destination_are_rejected_without_copying() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Bazza");
        let destination = root.path().join("Bazza-moved");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("bazza.db"), b"source library").unwrap();
        std::fs::write(destination.join("bazza.db"), b"separate library").unwrap();

        assert_eq!(prepare_copy_destination(&source, &destination).unwrap_err().code, "native.dataDestinationNotEmpty");
        assert_eq!(std::fs::read(destination.join("bazza.db")).unwrap(), b"separate library");
        assert_eq!(std::fs::read(source.join("bazza.db")).unwrap(), b"source library");
        let nested = source.join("new-location");
        assert_eq!(prepare_copy_destination(&source, &nested).unwrap_err().code, "native.dataDestinationNested");
        assert!(!nested.exists());
        let parent_escape = root.path().join("nonexistent").join("..").join("Bazza/new-location");
        assert_eq!(prepare_copy_destination(&source, &parent_escape).unwrap_err().code, "native.dataDestinationParent");
        assert!(!root.path().join("nonexistent").exists());
        assert!(!nested.exists());
        assert!(!prepare_copy_destination(&source, &source.join(".")).unwrap());
    }
}

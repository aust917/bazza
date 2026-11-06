//! Lets the meeting view list and open the participant WAVs from its own manifest.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use bazza_attendee::{Manifest, MANIFEST_FILE, TRACK_DIRECTORY};
use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::{attendee_settings::library_root, commands::CmdResult, ui_message::UiMessage};

const MAX_MANIFEST_BYTES: u64 = 512 * 1024;

#[derive(Serialize)]
pub struct AttendeeTrack {
    id: u32,
    name: Option<String>,
}

fn unavailable() -> UiMessage {
    UiMessage::app(
        "native.attendeeTrackUnavailable",
        "This Google Meet audio track is unavailable.",
    )
}

fn canonical_file(path: &Path) -> CmdResult<PathBuf> {
    let file = fs::canonicalize(path).map_err(|_| unavailable())?;
    if file != path || !file.is_file() {
        return Err(unavailable());
    }
    Ok(file)
}

fn read_tracks(root: &Path, meeting_id: &str) -> CmdResult<Vec<(AttendeeTrack, PathBuf)>> {
    let id = uuid::Uuid::parse_str(meeting_id).map_err(|_| unavailable())?;
    let recordings = match fs::canonicalize(root.join("recordings")) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(unavailable()),
    };
    let directory = recordings.join(id.to_string()).join(TRACK_DIRECTORY);
    let manifest_path = directory.join(MANIFEST_FILE);
    let file = match fs::canonicalize(&manifest_path) {
        Ok(path) if path == manifest_path && path.is_file() => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => return Err(unavailable()),
    };
    let mut bytes = Vec::new();
    File::open(file)
        .map_err(|_| unavailable())?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(unavailable());
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if manifest.schema_version != 1 {
        return Err(unavailable());
    }
    let mut tracks = Vec::new();
    for entry in manifest.tracks {
        let Some(id) = track_id(&entry.file) else {
            continue;
        };
        if tracks
            .iter()
            .any(|(track, _): &(AttendeeTrack, PathBuf)| track.id == id)
        {
            continue;
        }
        if let Ok(path) = canonical_file(&directory.join(&entry.file)) {
            tracks.push((
                AttendeeTrack {
                    id,
                    name: entry.display_name,
                },
                path,
            ));
        }
    }
    Ok(tracks)
}

fn track_id(name: &str) -> Option<u32> {
    let slot = name.strip_prefix("participant-")?.strip_suffix(".wav")?;
    let id = slot.parse::<u32>().ok()?;
    (id > 0 && slot == format!("{id:03}")).then_some(id)
}

#[tauri::command]
pub async fn attendee_tracks(app: AppHandle, meeting_id: String) -> CmdResult<Vec<AttendeeTrack>> {
    let root = library_root(&app)?;
    crate::commands::blocking(move || {
        Ok(read_tracks(&root, &meeting_id)?
            .into_iter()
            .map(|(track, _)| track)
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn attendee_open_track(
    app: AppHandle,
    meeting_id: String,
    track_id: u32,
) -> CmdResult<()> {
    let root = library_root(&app)?;
    crate::commands::blocking(move || {
        let (_, path) = read_tracks(&root, &meeting_id)?
            .into_iter()
            .find(|(track, _)| track.id == track_id)
            .ok_or_else(unavailable)?;
        app.opener()
            .open_path(path.to_string_lossy(), None::<&str>)
            .map_err(|_| unavailable())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_names_and_rejects_files_outside_the_track_directory() {
        let root = tempfile::tempdir().unwrap();
        let meeting_id = uuid::Uuid::new_v4().to_string();
        assert!(read_tracks(root.path(), &meeting_id).unwrap().is_empty());
        let meeting_dir = root.path().join("recordings").join(&meeting_id);
        let track_dir = meeting_dir.join(TRACK_DIRECTORY);
        fs::create_dir_all(&track_dir).unwrap();
        fs::write(track_dir.join("participant-001.wav"), []).unwrap();
        fs::write(track_dir.join("participant-002.wav"), []).unwrap();
        fs::write(meeting_dir.join("participant-003.wav"), []).unwrap();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "sample_rate": 16000,
            "duration_ms": 20,
            "tracks": [
                {"participant_id":"one", "display_name":"Анна", "file":"participant-001.wav", "received_samples":320},
                {"participant_id":"two", "display_name":"Анна", "file":"participant-002.wav", "received_samples":320},
                {"participant_id":"three", "display_name":"Outside", "file":"../participant-003.wav", "received_samples":320}
            ]
        });
        fs::write(track_dir.join(MANIFEST_FILE), manifest.to_string()).unwrap();
        let tracks = read_tracks(root.path(), &meeting_id).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].0.name.as_deref(), Some("Анна"));
        assert_eq!(tracks[1].0.name.as_deref(), Some("Анна"));
        assert_ne!(tracks[0].0.id, tracks[1].0.id);
        assert!(read_tracks(root.path(), "../outside").is_err());
    }
}

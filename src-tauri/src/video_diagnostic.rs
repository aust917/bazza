//! Durable video warnings are independent of optional AI stage failures.
//! This local sidecar is removed with the recording and excluded by bundle media allowlists.

use bazza_store::Store;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;

pub const NO_LLM: &str = "no-llm: clean-up and summary will run once the language model is downloaded";
const FILE_NAME: &str = "processing-diagnostics.json";
const MAX_BYTES: u64 = 32 * 1024;
const DETAIL_BYTES: usize = 4096;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage { Pipeline, Speakers, Cleanup, Summary }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum OptionalError { MissingLanguageModel, Failure(String) }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagnostic {
    version: u32,
    video: Option<String>,
    /// A DB-only warning left when its typed sidecar could not be published.
    /// Its origin is uncertain: optional stages may not erase or reinterpret it.
    retained_warning: Option<String>,
    optional: BTreeMap<Stage, OptionalError>,
}

impl Default for Diagnostic {
    fn default() -> Self { Self { version: 1, video: None, retained_warning: None, optional: BTreeMap::new() } }
}

impl Diagnostic {
    fn display(&self) -> Option<String> {
        let mut messages = Vec::new();
        if let Some(video) = &self.video { messages.push(format!("video: {video}")); }
        if let Some(warning) = &self.retained_warning {
            if !messages.contains(warning) { messages.push(warning.clone()); }
        }
        for issue in self.optional.values() {
            let message = match issue {
                OptionalError::MissingLanguageModel => NO_LLM.to_string(),
                OptionalError::Failure(message) => message.clone(),
            };
            if !messages.contains(&message) { messages.push(message); }
        }
        (!messages.is_empty()).then(|| messages.join("\n"))
    }
}

fn path(store: &Store, id: &str) -> Result<PathBuf, String> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| "invalid diagnostic meeting ID")?;
    if parsed.to_string() != id { return Err("invalid diagnostic meeting ID".into()); }
    Ok(store.root().join("recordings").join(id).join(FILE_NAME))
}

fn read(store: &Store, id: &str) -> Result<Option<Diagnostic>, String> {
    let file = match File::open(path(store, id)?) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read processing diagnostics: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("read processing diagnostics: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES { return Err("processing diagnostics exceed size limit".into()); }
    let diagnostic: Diagnostic = serde_json::from_slice(&bytes).map_err(|e| format!("read processing diagnostics: {e}"))?;
    if diagnostic.version != 1 { return Err("unsupported processing diagnostics version".into()); }
    Ok(Some(diagnostic))
}

fn bounded(message: &str) -> String {
    if message.len() <= DETAIL_BYTES { return message.to_string(); }
    let mut end = DETAIL_BYTES;
    while !message.is_char_boundary(end) { end -= 1; }
    format!("{}…", &message[..end])
}

fn update(store: &Store, id: &str, change: impl FnOnce(&mut Diagnostic)) -> Result<Option<String>, String> {
    let _guard = WRITE_LOCK.lock();
    let meeting = store.get_meeting(id).map_err(|e| e.to_string())?.ok_or("diagnostic meeting no longer exists")?;
    let mut diagnostic = read(store, id)?.unwrap_or_default();
    // Keep a database-only error when an earlier sidecar publication failed.
    // Treat it as an opaque whole message, never split presentation strings.
    if diagnostic.display().is_none() {
        if let Some(error) = meeting.error {
            if error == NO_LLM {
                diagnostic.optional.insert(Stage::Pipeline, OptionalError::MissingLanguageModel);
            } else { diagnostic.retained_warning = Some(bounded(&error)); }
        }
    }
    change(&mut diagnostic);
    let destination = path(store, id)?;
    let parent = destination.parent().ok_or("processing diagnostic directory is missing")?;
    fs::create_dir_all(parent).map_err(|e| format!("create processing diagnostic directory: {e}"))?;
    let bytes = serde_json::to_vec(&diagnostic).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES { return Err("processing diagnostics exceed size limit".into()); }
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("stage processing diagnostics: {e}"))?;
    temp.write_all(&bytes).and_then(|_| temp.as_file().sync_all()).map_err(|e| format!("save processing diagnostics: {e}"))?;
    temp.persist(&destination).map_err(|e| format!("publish processing diagnostics: {}", e.error))?;
    // Deletion does not hold this module's lock. A late worker must not recreate
    // a recording directory after its owner deleted the meeting.
    if store.get_meeting(id).map_err(|e| e.to_string())?.is_none() {
        let _ = fs::remove_file(&destination);
        let _ = fs::remove_dir(parent); // Only remove an empty directory, never media.
        return Err("diagnostic meeting was deleted during publication".into());
    }
    let display = diagnostic.display();
    store.set_meeting_error(id, display.as_deref()).map_err(|e| e.to_string())?;
    Ok(display)
}

/// A new processing pass can reset optional work, never an unresolved video warning.
pub fn begin_pipeline(store: &Store, id: &str) -> Result<Option<String>, String> {
    update(store, id, |diagnostic| diagnostic.optional.clear())
}

pub fn record_video_failure(store: &Store, id: &str, message: &str) -> Result<Option<String>, String> {
    update(store, id, |diagnostic| diagnostic.video = Some(bounded(message)))
}

/// Only callers that just validated or recovered the video may clear this warning.
pub fn clear_verified_video(store: &Store, id: &str) -> Result<Option<String>, String> {
    update(store, id, |diagnostic| {
        diagnostic.video = None;
        diagnostic.retained_warning = None;
    })
}

pub fn set_optional(store: &Store, id: &str, stage: Stage, error: Option<OptionalError>) -> Result<Option<String>, String> {
    update(store, id, |diagnostic| {
        if let Some(error) = error {
            let error = match error { OptionalError::Failure(message) => OptionalError::Failure(bounded(&message)), other => other };
            diagnostic.optional.insert(stage, error);
        } else { diagnostic.optional.remove(&stage); }
    })
}

pub fn current_error(store: &Store, id: &str) -> Result<Option<String>, String> {
    let _guard = WRITE_LOCK.lock();
    match read(store, id)? {
        Some(diagnostic) => match diagnostic.display() {
            Some(error) => Ok(Some(error)),
            None => Ok(store.get_meeting(id).map_err(|e| e.to_string())?.and_then(|meeting| meeting.error)),
        },
        None => Ok(store.get_meeting(id).map_err(|e| e.to_string())?.and_then(|meeting| meeting.error)),
    }
}

pub fn has_video_warning(store: &Store, id: &str) -> Result<bool, String> {
    let _guard = WRITE_LOCK.lock();
    Ok(read(store, id)?.is_some_and(|diagnostic| diagnostic.video.is_some()))
}

/// An opaque retained warning requests verification of an existing video,
/// but cannot establish that an audio-only meeting ever had screen capture.
pub fn needs_video_verification(store: &Store, id: &str) -> Result<bool, String> {
    let _guard = WRITE_LOCK.lock();
    Ok(read(store, id)?.is_some_and(|diagnostic| diagnostic.video.is_some() || diagnostic.retained_warning.is_some()))
}

/// Read typed retry state; the presentation string is never split or parsed.
pub fn needs_deferred_ai(store: &Store, id: &str, existing_error: Option<&str>) -> Result<bool, String> {
    let _guard = WRITE_LOCK.lock();
    Ok(match read(store, id)? {
        Some(diagnostic) => diagnostic.optional.values().any(|error| *error == OptionalError::MissingLanguageModel),
        None => existing_error.is_some_and(|error| error.starts_with("no-llm:")),
    })
}

pub fn clear_deferred_ai(store: &Store, id: &str) -> Result<Option<String>, String> {
    update(store, id, |diagnostic| diagnostic.optional.retain(|_, error| *error != OptionalError::MissingLanguageModel))
}

/// Repair the database projection if shutdown interrupted sidecar -> DB publication.
/// Invalid sidecars never clear an existing database warning or prevent app startup.
pub fn restore_saved(store: &Store) {
    let _guard = WRITE_LOCK.lock();
    let meetings = match store.list_meetings() {
        Ok(meetings) => meetings,
        Err(error) => { log::warn!("restore processing diagnostics: {error}"); return; }
    };
    for meeting in meetings {
        match read(store, &meeting.id) {
            Ok(Some(diagnostic)) => {
                let display = diagnostic.display();
                // An empty sidecar never erases a DB-only warning left by a
                // failed publish; the next explicit processing pass resolves it.
                if display.is_some() && display != meeting.error {
                    if let Err(error) = store.set_meeting_error(&meeting.id, display.as_deref()) {
                        log::warn!("restore processing diagnostics for {}: {error}", meeting.id);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => log::warn!("restore processing diagnostics for {}: {error}", meeting.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_store::{Meeting, MeetingStatus};

    #[test]
    fn recovery_warning_survives_optional_failures_deferred_clear_and_store_reopen() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("bazza.db");
        let id;
        {
            let store = Store::open(&db).unwrap();
            let mut meeting = Meeting::new_recording("recovered synthetic meeting", None);
            meeting.status = MeetingStatus::Ready;
            id = meeting.id.clone();
            store.create_meeting(&meeting).unwrap();
            record_video_failure(&store, &id, "strict video decode failed").unwrap();
            set_optional(&store, &id, Stage::Cleanup, Some(OptionalError::MissingLanguageModel)).unwrap();
            set_optional(&store, &id, Stage::Summary, Some(OptionalError::MissingLanguageModel)).unwrap();
            assert!(needs_deferred_ai(&store, &id, None).unwrap());
            set_optional(&store, &id, Stage::Speakers, Some(OptionalError::Failure("speakers: unavailable".into()))).unwrap();
            set_optional(&store, &id, Stage::Speakers, None).unwrap();
            assert!(needs_deferred_ai(&store, &id, None).unwrap(), "rediarization must not clear another stage's pending retry");
            // Simulate interruption after publishing JSON but before updating SQLite.
            store.set_meeting_error(&id, None).unwrap();
        }
        {
            let store = Store::open(&db).unwrap();
            restore_saved(&store);
            assert!(store.get_meeting(&id).unwrap().unwrap().error.unwrap().contains("video: strict video decode failed"));
            assert!(needs_deferred_ai(&store, &id, None).unwrap());
            clear_deferred_ai(&store, &id).unwrap();
            assert!(!needs_deferred_ai(&store, &id, None).unwrap());
            set_optional(&store, &id, Stage::Summary, Some(OptionalError::Failure("summary: artificial failure; not a delimiter".into()))).unwrap();
            set_optional(&store, &id, Stage::Summary, None).unwrap();
            assert_eq!(store.get_meeting(&id).unwrap().unwrap().error.as_deref(), Some("video: strict video decode failed"));
        }
        let store = Store::open(&db).unwrap();
        assert_eq!(current_error(&store, &id).unwrap().as_deref(), Some("video: strict video decode failed"));
        begin_pipeline(&store, &id).unwrap();
        assert!(has_video_warning(&store, &id).unwrap());
        clear_verified_video(&store, &id).unwrap();
        assert_eq!(store.get_meeting(&id).unwrap().unwrap().error, None);
    }

    #[test]
    fn invalid_sidecar_cannot_erase_saved_warning_or_escape_recording_directory() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
        let meeting = Meeting::new_recording("fixture", None);
        store.create_meeting(&meeting).unwrap();
        record_video_failure(&store, &meeting.id, "preserve this warning").unwrap();
        let file = path(&store, &meeting.id).unwrap();
        fs::write(&file, b"{interrupted invalid JSON").unwrap();
        assert!(set_optional(&store, &meeting.id, Stage::Summary, None).is_err());
        assert_eq!(store.get_meeting(&meeting.id).unwrap().unwrap().error.as_deref(), Some("video: preserve this warning"));
        assert_eq!(fs::read(&file).unwrap(), b"{interrupted invalid JSON");
        assert!(path(&store, "../outside").is_err());
        store.delete_meeting(&meeting.id).unwrap();
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
        assert!(record_video_failure(&store, &meeting.id, "late worker").is_err());
        assert!(!file.parent().unwrap().exists());
    }

    #[test]
    fn empty_sidecar_does_not_erase_database_warning_after_failed_publication() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("bazza.db");
        let id;
        {
            let store = Store::open(&db).unwrap();
            let meeting = Meeting::new_recording("fixture", None);
            id = meeting.id.clone();
            store.create_meeting(&meeting).unwrap();
            begin_pipeline(&store, &id).unwrap();
            store.set_meeting_error(&id, Some("video: diagnostic publication failed")).unwrap();
        }
        let store = Store::open(&db).unwrap();
        restore_saved(&store);
        set_optional(&store, &id, Stage::Summary, None).unwrap();
        begin_pipeline(&store, &id).unwrap();
        assert!(needs_video_verification(&store, &id).unwrap(), "a DB-only warning cannot become a clearable optional error");
        assert!(!has_video_warning(&store, &id).unwrap(), "opaque warnings cannot invent screen capture on an audio-only meeting");
        assert_eq!(store.get_meeting(&id).unwrap().unwrap().error.as_deref(), Some("video: diagnostic publication failed"));
        clear_verified_video(&store, &id).unwrap();
        assert_eq!(store.get_meeting(&id).unwrap().unwrap().error, None);
    }
}

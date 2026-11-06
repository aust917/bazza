//! Transactional service data API. Legacy Store writers are not enrolled until
//! the GUI/worker cutover. Callers must validate authorization before entering.
mod chats;
mod configuration;
mod consistency;
mod content;
mod desktop;
mod documents;
mod execution;
mod external_read_grants;
mod fixture_read;
mod library;
mod meetings;
mod notes;
mod postprocess;
pub(crate) mod postprocess_checkpoint;
mod postprocess_cleanup;
mod postprocess_complete;
mod postprocess_liveness;
mod postprocess_media;
mod postprocess_ready;
mod postprocess_result;
mod postprocess_source_install;
mod postprocess_speaker_install;
mod postprocess_summary_install;
mod postprocess_video;
mod search;
mod soniox_permission;
mod templates;
mod transcript_versions;
#[cfg(test)]
mod tests;
mod workers;

pub use chats::*;
pub use configuration::*;
pub use consistency::*;
pub use content::*;
pub use desktop::*;
pub use documents::*;
pub use execution::*;
pub use external_read_grants::*;
pub use fixture_read::*;
pub use library::*;
pub use meetings::*;
pub use notes::*;
pub use postprocess::*;
pub use postprocess_cleanup::*;
pub use postprocess_complete::*;
pub use postprocess_media::*;
pub use postprocess_ready::*;
pub use postprocess_result::*;
pub use postprocess_video::*;
pub use search::*;
pub use soniox_permission::*;
pub use templates::*;
pub use workers::*;

use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("entity not found")]
    NotFound,
    #[error("revision conflict")]
    Conflict { current_revision: String },
    #[error("idempotency key was used with different arguments")]
    IdempotencyConflict,
    #[error("invalid data: {0}")]
    Invalid(&'static str),
    #[error("resource is busy")]
    Busy,
    #[error("operation is not permitted")]
    Forbidden,
    #[error("search projection requires rebuild")]
    IndexUnavailable,
    #[error("cursor belongs to an older snapshot")]
    StaleCursor,
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
pub type DataResult<T> = Result<T, DataError>;

impl crate::Store {
    /// Explicit repository bootstrap, never a side effect of a page read.
    pub fn service_initialize_library(&self) -> DataResult<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        library::initialize_library_on(&tx)?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn revision() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(super) fn check_revision(actual: &str, expected: &str) -> DataResult<()> {
    if actual != expected {
        return Err(DataError::Conflict {
            current_revision: actual.into(),
        });
    }
    Ok(())
}

pub(super) fn live_meeting(conn: &Connection, id: &str) -> DataResult<()> {
    let status: Option<(String, bool)> = conn.query_row(
        "SELECT m.status,s.tombstoned FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id WHERE m.id=?1",
        [id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    match status {
        None | Some((_, true)) => Err(DataError::NotFound),
        Some((status, _)) if status == "recording" || status == "processing" => {
            Err(DataError::Busy)
        }
        Some(_) => Ok(()),
    }
}

/// Reads remain available while capture/processing owns a mutation reservation.
pub(super) fn visible_meeting(conn: &Connection, id: &str) -> DataResult<()> {
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id WHERE m.id=?1 AND s.tombstoned=0)",[id],|r|r.get(0))?;
    if !exists {
        return Err(DataError::NotFound);
    }
    Ok(())
}

pub(super) fn bounded_text(value: &str, min: usize, max: usize) -> DataResult<()> {
    if value.len() < min || value.len() > max || value.contains('\0') {
        return Err(DataError::Invalid("text length or NUL"));
    }
    Ok(())
}

pub(super) fn safe_id(value: &str) -> DataResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(DataError::Invalid("identifier"));
    }
    Ok(())
}

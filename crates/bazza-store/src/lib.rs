//! Bazza local storage: SQLite (+ FTS5) and the on-disk recording layout.
//!
//! The crate knows nothing about Tauri. Everything is synchronous; callers
//! wrap it in `Arc` and call from whatever thread they like.

mod ai;
mod db;
mod import;
mod meetings;
mod migrations;
mod organize;
mod owner;
mod segments;
pub mod service_data;
mod settings;
mod speakers;
mod transcript_versions;
mod vectors;
mod voice_samples;

pub use ai::{Chat, ChatMessage, ChunkHit, Summary, Template, CHUNK_MS};
pub use bazza_domain::{cosine, fuse_hits};
pub use db::{
    data_dir, database_path, default_data_dir, schema_version, write_data_dir_pointer,
    MigrationPolicy, Store,
};
pub use import::MeetingImport;
pub use meetings::{Meeting, MeetingStatus};
pub use migrations::SUPPORTED_SCHEMA;
pub use organize::{Folder, MeetingFilter, Share, Tag};
pub use owner::{LibraryOwnerGuard, SERVICE_IDENTITY_FILE, SERVICE_OWNER_LOCK_FILE};
pub use segments::{
    ManualCorrectionError, NewSegment, SearchHit, Segment, TranscriptCorrectionError,
    MAX_TRANSCRIPT_CORRECTION_BYTES,
};
pub use speakers::{MeetingSpeaker, RenameOutcome, Speaker, SpeakerStat};
pub use voice_samples::{
    NewVoiceSample, VoiceMatchProfile, VoiceProfile, VoiceSample, CURRENT_VOICE_MODEL,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("this library requires the service-aware application; legacy access is disabled")]
    ServiceRequired,
    #[error("library schema {found} is newer than supported schema {supported}")]
    FutureSchema { found: i64, supported: i64 },
    #[error("library schema {found} requires an explicit upgrade to {supported}")]
    MigrationRequired { found: i64, supported: i64 },
    #[error("library or database does not exist")]
    LibraryMissing,
    #[error("library database already exists")]
    LibraryExists,
    #[error("another service owns this library")]
    OwnerBusy,
    #[error("library path must use regular files in an existing directory")]
    InvalidLibraryPath,
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

//! Shared Bazza value models. No database, filesystem, transport or Tauri APIs.
//! These preserve the existing local application/bundle shapes; they are not
//! public MCP projections and may include private voice vectors or share tokens.

pub mod agent_summary;
mod ai;
pub mod capture_screen;
pub mod capture_stt;
mod configuration;
pub mod local_asr;
mod meetings;
mod organize;
mod retrieval;
mod segments;
pub mod speaker_result;
pub mod transcript_versions;
mod voice;
pub mod worker_summary;

pub use ai::{Chat, ChatMessage, ChunkHit, Summary, Template};
pub use configuration::{NativeSettings, NativeSettingsValidationCode, NativeSttProvider, NativeTier, NativeVideoTarget};
pub use meetings::{Meeting, MeetingStatus};
pub use organize::{Folder, MeetingFilter, Share, Tag};
pub use retrieval::{cosine, fuse_hits};
pub use segments::{
    validate_transcript_correction, ManualCorrectionError, NewSegment, SearchHit, Segment,
    MAX_TRANSCRIPT_CORRECTION_BYTES,
};
pub use voice::{MeetingSpeaker, RenameOutcome, Speaker, SpeakerStat, VoiceProfile, VoiceSample};

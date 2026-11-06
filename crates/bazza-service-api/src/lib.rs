//! Versioned contracts for the future Bazza service and MCP adapter.
//!
//! A schema catalog describes intended interfaces, not implemented handlers or
//! available service capabilities. This crate never opens a user library.

pub mod agent_summary;
mod catalog;
pub mod chats;
pub mod configuration;
pub mod content;
pub mod desktop;
pub mod external_read;
pub mod fixture_read;
pub mod inputs;
pub mod library;
pub mod outputs;
mod permissions;
pub mod postprocess;
pub mod postprocess_cleanup;
pub mod postprocess_complete;
pub mod postprocess_input;
pub mod postprocess_media;
pub mod postprocess_ready;
pub mod postprocess_result;
pub mod postprocess_source;
pub mod postprocess_speakers;
pub mod postprocess_summary;
pub mod postprocess_video;
pub mod rpc;
pub mod soniox_permission;
pub mod templates;
pub mod types;
mod validation;
pub mod worker;

pub use catalog::{tool_schemas, ToolSchema, BASE_TOOL_NAMES};
pub use permissions::{tool_permission, LibraryPermission, ToolPermission};
pub use validation::{
    slice_utf8_page, validate_tool_input, validate_tool_output, validate_tool_output_for_input,
    ContractValidationError, Utf8Page,
};

/// Initial bounded request budget, checked in addition to JSON Schema lengths.
pub const MAX_REQUEST_BYTES: usize = 1_048_576;
/// Initial bounded response budget; handlers must paginate before exceeding it.
pub const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub const MAX_PAGE_UTF8_BYTES: usize = 65_536;
pub const MAX_DOCUMENT_UTF8_BYTES: usize = 262_144;
pub const MAX_TRANSCRIPT_BATCH: usize = 1_000;
pub const MAX_CORRECTION_BATCH: usize = 100;
/// Contract target; durable deduplication is implemented by the later data core.
pub const DEDUP_RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;
pub mod transcript_versions;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Segment {
    pub id: i64,
    pub meeting_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
    pub clean_text: Option<String>,
    #[serde(default)]
    pub clean_text_manual: bool,
}

pub const MAX_TRANSCRIPT_CORRECTION_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ManualCorrectionError {
    #[error("transcript correction exceeds 65536 UTF-8 bytes")]
    TooLarge,
    #[error("invalid transcript correction")]
    Invalid,
}

/// Validate UTF-8 byte limits without changing original or cleaned text.
pub fn validate_transcript_correction(text: Option<&str>) -> Result<(), ManualCorrectionError> {
    if let Some(text) = text {
        if text.len() > MAX_TRANSCRIPT_CORRECTION_BYTES {
            return Err(ManualCorrectionError::TooLarge);
        }
        if text.contains('\0') {
            return Err(ManualCorrectionError::Invalid);
        }
    }
    Ok(())
}

impl Segment {
    /// Validate the additive bundle field without changing its exact text.
    pub fn validate_manual_correction(&self) -> std::result::Result<(), ManualCorrectionError> {
        if self.clean_text_manual {
            let text = self
                .clean_text
                .as_deref()
                .ok_or(ManualCorrectionError::Invalid)?;
            validate_transcript_correction(Some(text))?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NewSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub meeting_id: String,
    pub meeting_title: String,
    pub segment_id: i64,
    pub start_ms: i64,
    pub snippet: String,
}

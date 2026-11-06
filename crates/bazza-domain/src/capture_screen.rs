use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawScreenWriterOutcome {
    Stopped,
    Failed,
}

/// Evidence for fixed raw capture artifacts. None means the file was absent
/// when the writer closed. A zero length records a present, empty file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawScreenEvidence {
    pub writer: RawScreenWriterOutcome,
    pub raw_mp4_bytes: Option<u64>,
    pub timeline_bytes: Option<u64>,
    pub video_clock_bytes: Option<u64>,
    pub audio_clock_bytes: Option<u64>,
}

impl RawScreenEvidence {
    pub fn valid(&self) -> bool {
        [
            self.raw_mp4_bytes,
            self.timeline_bytes,
            self.video_clock_bytes,
            self.audio_clock_bytes,
        ]
        .into_iter()
        .flatten()
        .all(|bytes| bytes <= 9_007_199_254_740_991)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessVideoFailure {
    MissingRaw,
    FfmpegUnavailable,
    MuxFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostProcessVideoOutcome {
    Published { bytes: u64 },
    Unavailable { reason: PostProcessVideoFailure },
}

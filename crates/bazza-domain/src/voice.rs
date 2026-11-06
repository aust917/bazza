use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Speaker {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub embedding: Vec<f32>,
}

/// How much one speaker label actually says in a meeting, for the speaker list.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpeakerStat {
    pub label: String,
    pub segments: i64,
    /// Total speaking time in milliseconds.
    pub ms: i64,
    pub first_ms: i64,
}

/// What a rename actually did, so the UI can say "merged" rather than "renamed".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RenameOutcome {
    /// The person this label now belongs to. `None` when the label is still a
    /// placeholder ("Speaker 2"), which is not worth remembering as a person.
    pub speaker: Option<Speaker>,
    /// The label every affected segment now carries.
    pub name: String,
    /// The label that disappeared, when the new name already labelled other
    /// segments in this meeting (i.e. two detected voices became one person).
    pub merged_from: Option<String>,
    /// Segments that changed label.
    pub moved_segments: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MeetingSpeaker {
    pub meeting_id: String,
    /// Label as it appears in `segments.speaker` ("Speaker 1", "Me", or a name once renamed).
    pub label: String,
    pub speaker_id: Option<String>,
    pub suggested_id: Option<String>,
    pub suggested_name: Option<String>,
    pub suggested_score: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoiceProfile {
    pub id: String,
    pub name: String,
    pub model_id: String,
    pub sample_count: i64,
    pub has_voiceprint: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoiceSample {
    pub id: String,
    pub speaker_id: String,
    pub model_id: String,
    pub source: String,
    pub source_label: String,
    pub meeting_id: Option<String>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub has_audio: bool,
    pub created_at: String,
}

//! Version metadata and the bounded result of an explicit recognition run.
use crate::{local_asr, NewSegment, Segment};
use chrono::{DateTime, Utc};
use serde::{de::Error as _, Deserialize, Serialize};
use uuid::Uuid;

pub const INITIAL_TRANSCRIPT_VERSION_ID: &str = "original";
pub const MAX_VERSION_SPEAKER_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptVersionSource {
    Live,
    Recording,
    Meet,
    Discord,
    Existing,
    Combined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptParticipantSource {
    Meet,
    Discord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptVersion {
    pub id: String,
    pub source: TranscriptVersionSource,
    pub model: Option<String>,
    pub language: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub partial: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptVersions {
    pub versions: Vec<TranscriptVersion>,
    pub initial_id: Option<String>,
    pub participant_source: Option<TranscriptParticipantSource>,
    #[serde(default)]
    pub effective_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSegmentSource {
    Bot,
    System,
    Microphone,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSegmentProvenance {
    pub source: TranscriptSegmentSource,
    pub provider: Option<TranscriptParticipantSource>,
    pub participant_id: Option<String>,
    pub source_start_ms: i64,
    pub source_end_ms: i64,
    #[serde(default)]
    pub uncertain: bool,
}

impl TranscriptSegmentProvenance {
    pub fn valid(&self) -> bool {
        valid_timing(self.source_start_ms, self.source_end_ms)
            && self.participant_id.as_deref().is_none_or(|id| {
                !id.trim().is_empty() && id.len() <= 512 && !id.contains('\0')
            })
            && match self.source {
                TranscriptSegmentSource::Bot => self.provider.is_some() && self.participant_id.is_some(),
                TranscriptSegmentSource::System | TranscriptSegmentSource::Microphone => {
                    self.provider.is_none() && self.participant_id.is_none()
                }
            }
    }
}

/// A participant segment matched to the playable recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptPlaybackTiming {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl TranscriptPlaybackTiming {
    pub fn valid(self) -> bool {
        valid_timing(self.start_ms, self.end_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptVersionSegment {
    #[serde(flatten)]
    pub segment: Segment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<TranscriptPlaybackTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<TranscriptSegmentProvenance>,
}

impl From<Segment> for TranscriptVersionSegment {
    fn from(segment: Segment) -> Self {
        Self {
            segment,
            playback: None,
            provenance: None,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTranscriptVersionSegment {
    #[serde(flatten)]
    pub segment: NewSegment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<TranscriptPlaybackTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<TranscriptSegmentProvenance>,
}

impl From<NewSegment> for NewTranscriptVersionSegment {
    fn from(segment: NewSegment) -> Self {
        Self {
            segment,
            playback: None,
            provenance: None,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTranscriptVersion {
    pub id: String,
    pub source: TranscriptVersionSource,
    pub model: String,
    pub language: Option<String>,
    pub segments: Vec<NewTranscriptVersionSegment>,
    #[serde(default)]
    pub partial: bool,
}

impl std::fmt::Debug for NewTranscriptVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewTranscriptVersion")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("segment_count", &self.segments.len())
            .finish_non_exhaustive()
    }
}

pub fn manual_transcript_version_id_valid(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| !uuid.is_nil() && uuid.to_string() == id)
}

impl NewTranscriptVersion {
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        if bytes.len() as u64 > local_asr::MAX_LOCAL_ASR_BODY_BYTES {
            return Err(serde_json::Error::custom(
                "transcript version exceeds byte budget",
            ));
        }
        let version: Self = serde_json::from_slice(bytes)?;
        if !version.valid() {
            return Err(serde_json::Error::custom("invalid transcript version"));
        }
        Ok(version)
    }

    pub fn valid(&self) -> bool {
        manual_transcript_version_id_valid(&self.id)
            && matches!(
                self.source,
                TranscriptVersionSource::Recording
                    | TranscriptVersionSource::Meet
                    | TranscriptVersionSource::Discord
                    | TranscriptVersionSource::Combined
            )
            && local_asr::model_id_valid(&self.model)
            && self
                .language
                .as_deref()
                .is_none_or(local_asr::language_id_valid)
            && (1..=local_asr::MAX_LOCAL_ASR_CAPTIONS).contains(&self.segments.len())
            && self.segments.iter().all(|row| {
                valid_version_segment(&row.segment)
                    && row.provenance.as_ref().is_none_or(TranscriptSegmentProvenance::valid)
                    && (self.source != TranscriptVersionSource::Combined || row.provenance.is_some())
                    && (self.source != TranscriptVersionSource::Combined || self.partial
                        || row.provenance.as_ref().is_none_or(|origin| !origin.uncertain))
                    && row.playback.is_none_or(|timing| {
                        self.source != TranscriptVersionSource::Recording && timing.valid()
                    })
            })
    }
}

/// Additive, portable version history. It contains transcript data only; worker
/// registrations, provider credentials, settings and audio are never included.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptVersionSnapshot {
    pub metadata: TranscriptVersion,
    pub segments: Vec<TranscriptVersionSegment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptVersionArchive {
    pub schema: u32,
    pub original: Option<TranscriptVersionSnapshot>,
    pub alternatives: Vec<TranscriptVersionSnapshot>,
    pub effective_id: Option<String>,
}

impl TranscriptVersionArchive {
    pub fn valid(&self, meeting_id: &str, canonical: &[Segment]) -> bool {
        if self.schema != 1 || self.alternatives.len() > 128
            || serde_json::to_vec(self).map_or(true, |bytes| bytes.len() as u64 > local_asr::MAX_LOCAL_ASR_BODY_BYTES)
        { return false; }
        let mut canonical_ids = std::collections::HashSet::new();
        if canonical.iter().any(|row| row.meeting_id != meeting_id || row.id <= 0 || !canonical_ids.insert(row.id)) {
            return false;
        }
        let mut ids = std::collections::HashSet::new();
        if self.original.as_ref().is_some_and(|snapshot| {
            snapshot.metadata.id != INITIAL_TRANSCRIPT_VERSION_ID
                || !matches!(snapshot.metadata.source, TranscriptVersionSource::Live | TranscriptVersionSource::Existing | TranscriptVersionSource::Recording)
        }) { return false; }
        for snapshot in self.original.iter().chain(&self.alternatives) {
            let metadata = &snapshot.metadata;
            if !ids.insert(metadata.id.as_str()) || snapshot.segments.is_empty()
                || snapshot.segments.len() > if metadata.id == INITIAL_TRANSCRIPT_VERSION_ID { 100_000 } else { local_asr::MAX_LOCAL_ASR_CAPTIONS }
                || metadata.model.as_deref().is_some_and(|model| if metadata.id == INITIAL_TRANSCRIPT_VERSION_ID {
                    model.is_empty() || model.len() > 256 || model.contains('\0')
                } else { !local_asr::model_id_valid(model) })
                || metadata.language.as_deref().is_some_and(|language| if metadata.id == INITIAL_TRANSCRIPT_VERSION_ID {
                    language.is_empty() || language.len() > 64 || language.contains('\0')
                } else { !local_asr::language_id_valid(language) })
            { return false; }
            let original = metadata.id == INITIAL_TRANSCRIPT_VERSION_ID;
            if !original && (!manual_transcript_version_id_valid(&metadata.id)
                || metadata.model.is_none()
                || !matches!(metadata.source, TranscriptVersionSource::Recording | TranscriptVersionSource::Meet | TranscriptVersionSource::Discord | TranscriptVersionSource::Combined))
            { return false; }
            let mut row_ids = std::collections::HashSet::new();
            for row in &snapshot.segments {
                let segment = &row.segment;
                if segment.meeting_id != meeting_id || segment.id == 0 || !row_ids.insert(segment.id)
                    || !valid_timing(segment.start_ms, segment.end_ms) || (!original && segment.text.trim().is_empty())
                    || segment.speaker.as_deref().is_some_and(|name| (!original && name.trim().is_empty())
                        || name.len() > if original { 65_536 } else { MAX_VERSION_SPEAKER_BYTES } || name.contains('\0'))
                    || segment.validate_manual_correction().is_err()
                    || row.playback.is_some_and(|timing| !timing.valid() || original || metadata.source == TranscriptVersionSource::Recording)
                    || row.provenance.as_ref().is_some_and(|origin| !origin.valid())
                    || (metadata.source == TranscriptVersionSource::Combined && (row.provenance.is_none()
                        || (!metadata.partial && row.provenance.as_ref().is_some_and(|origin| origin.uncertain))))
                    || (!original && self.effective_id.as_deref() != Some(metadata.id.as_str())
                        && (segment.clean_text.is_some() || segment.clean_text_manual))
                { return false; }
            }
        }
        let effective = self.effective_id.as_deref().and_then(|id| self.original.iter().chain(&self.alternatives)
            .find(|snapshot| snapshot.metadata.id == id));
        match (&self.effective_id, effective) {
            (None, None) => canonical.is_empty(),
            (Some(_), Some(snapshot)) => {
                if snapshot.metadata.id != INITIAL_TRANSCRIPT_VERSION_ID
                    && (snapshot.metadata.source != TranscriptVersionSource::Combined || snapshot.metadata.partial)
                { return false; }
                let mut current: Vec<_> = canonical.iter().collect();
                let mut archived: Vec<_> = snapshot.segments.iter().map(|row| &row.segment).collect();
                let compare = |a: &&Segment, b: &&Segment| {
                    (a.start_ms,a.end_ms,&a.speaker,&a.text,&a.clean_text,a.clean_text_manual)
                        .cmp(&(b.start_ms,b.end_ms,&b.speaker,&b.text,&b.clean_text,b.clean_text_manual))
                };
                current.sort_by(compare);
                archived.sort_by(compare);
                current.len() == archived.len() && current.into_iter().zip(archived).all(|(a,b)| {
                    a.start_ms == b.start_ms && a.end_ms == b.end_ms && a.speaker == b.speaker
                        && a.text == b.text && a.clean_text == b.clean_text && a.clean_text_manual == b.clean_text_manual
                })
            }
            _ => false,
        }
    }
}

/// Keep exact caption text, including overlapping and zero-length timings.
pub fn valid_transcript_timing_text(segment: &NewSegment) -> bool {
    valid_timing(segment.start_ms, segment.end_ms) && !segment.text.trim().is_empty()
}

fn valid_timing(start_ms: i64, end_ms: i64) -> bool {
    const MAX_TIMELINE_MS: i64 = 9_007_199_254_740_991;
    (0..=MAX_TIMELINE_MS).contains(&start_ms) && (start_ms..=MAX_TIMELINE_MS).contains(&end_ms)
}

fn valid_version_segment(segment: &NewSegment) -> bool {
    valid_transcript_timing_text(segment)
        && segment.speaker.as_deref().is_none_or(|name| {
            !name.trim().is_empty()
                && name.len() <= MAX_VERSION_SPEAKER_BYTES
                && !name.contains('\0')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_version_payloads_keep_defaults_and_combined_requires_certain_provenance() {
        let id = Uuid::new_v4().to_string();
        let legacy = serde_json::json!({"id":id,"source":"recording","model":"whisper-small","language":null,
            "segments":[{"start_ms":1,"end_ms":20,"speaker":null,"text":"words"}]});
        let mut input = NewTranscriptVersion::parse(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(!input.partial);
        assert!(input.segments[0].provenance.is_none());
        input.source = TranscriptVersionSource::Combined;
        assert!(!input.valid());
        input.segments[0].provenance = Some(TranscriptSegmentProvenance {
            source: TranscriptSegmentSource::System, provider: None, participant_id: None,
            source_start_ms: 1, source_end_ms: 20, uncertain: true,
        });
        assert!(!input.valid());
        input.partial = true;
        assert!(input.valid());
        input.segments[0].provenance.as_mut().unwrap().source = TranscriptSegmentSource::Bot;
        assert!(!input.valid());
        let origin = input.segments[0].provenance.as_mut().unwrap();
        origin.provider = Some(TranscriptParticipantSource::Meet);
        origin.participant_id = Some("stable-id".into());
        origin.uncertain = false;
        input.partial = false;
        assert!(input.valid());
        let old_listing: TranscriptVersions = serde_json::from_value(serde_json::json!({"versions":[],"initial_id":null,"participant_source":null})).unwrap();
        assert!(old_listing.effective_id.is_none());
    }

    #[test]
    fn manual_version_parse_preserves_names_and_overlap() {
        let mut input = NewTranscriptVersion {
            partial: false,
            id: Uuid::new_v4().to_string(),
            source: TranscriptVersionSource::Discord,
            model: "whisper-small".into(),
            language: Some("ru-RU".into()),
            segments: vec![
                NewSegment {
                    start_ms: 30,
                    end_ms: 150,
                    speaker: Some("Марина".into()),
                    text: "  Исходная строка  ".into(),
                }
                .into(),
                NewSegment {
                    start_ms: 40,
                    end_ms: 120,
                    speaker: Some("Alex".into()),
                    text: "Перекрытие".into(),
                }
                .into(),
            ],
        };
        let playback = TranscriptPlaybackTiming {
            start_ms: 300,
            end_ms: 420,
        };
        input.segments[0].playback = Some(playback);
        let parsed = NewTranscriptVersion::parse(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.segments[0].playback, Some(playback));
        assert_eq!(parsed.segments[1].playback, None);
        input.source = TranscriptVersionSource::Recording;
        assert!(!input.valid());
        input.source = TranscriptVersionSource::Live;
        assert!(!input.valid());
        input.source = TranscriptVersionSource::Discord;
        input.id = INITIAL_TRANSCRIPT_VERSION_ID.into();
        assert!(!input.valid());
    }
}

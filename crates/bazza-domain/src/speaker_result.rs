//! Anonymous worker output for one selected local ASR source.
use serde::{
    de::{value::MapAccessDeserializer, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::{collections::HashSet, fmt, marker::PhantomData};

pub const MAX_SPEAKER_RESULT_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SPEAKER_ASSIGNMENTS: usize = 10_000;
const MAX_SEGMENT_ID: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerSkipReason {
    Disabled,
    EmptyTranscript,
    ModelUnavailable,
    SidecarFailed,
    NoTurns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpeakerResultOutcome {
    Applied,
    Skipped { reason: SpeakerSkipReason },
}

impl SpeakerResultOutcome {
    pub fn valid_changed_segments(self, count: u32) -> bool {
        count as usize <= MAX_SPEAKER_ASSIGNMENTS
            && (count == 0
                || self
                    != (Self::Skipped {
                        reason: SpeakerSkipReason::EmptyTranscript,
                    }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerAssignment {
    pub segment_id: i64,
    /// An opaque cluster index. Explicit null clears the label.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub cluster: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpeakerResultBody {
    Applied {
        /// Preserve immutable ASR segment order. The store verifies coverage.
        assignments: Vec<SpeakerAssignment>,
    },
    Skipped {
        reason: SpeakerSkipReason,
    },
}

impl<'de> Deserialize<'de> for SpeakerResultBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Applied,
            Skipped,
        }
        #[derive(Deserialize)]
        struct Assignments(
            #[serde(deserialize_with = "bounded_assignments")] Vec<SpeakerAssignment>,
        );
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            kind: Kind,
            #[serde(default, deserialize_with = "present")]
            assignments: Option<Assignments>,
            #[serde(default, deserialize_with = "present")]
            reason: Option<SpeakerSkipReason>,
        }
        // Decode directly as a map. The bounded array visitor runs during
        // decoding, avoiding a buffered internally tagged enum payload.
        let Object(fields) = Object::<Fields>::deserialize(deserializer)?;
        match (fields.kind, fields.assignments, fields.reason) {
            (Kind::Applied, Some(Assignments(assignments)), None) => {
                Ok(Self::Applied { assignments })
            }
            (Kind::Skipped, None, Some(reason)) => Ok(Self::Skipped { reason }),
            _ => Err(D::Error::custom("invalid speaker result fields")),
        }
    }
}

// Serde structs can also decode JSON arrays in field order. These body
// records use named fields so the wire cannot silently accept that shape.
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with named fields")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Object)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

impl fmt::Debug for SpeakerResultBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied { assignments } => f
                .debug_struct("Applied")
                .field("assignment_count", &assignments.len())
                .finish_non_exhaustive(),
            Self::Skipped { reason } => f.debug_struct("Skipped").field("reason", reason).finish(),
        }
    }
}

fn bounded_assignments<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<SpeakerAssignment>, D::Error> {
    struct Assignments;
    impl<'de> Visitor<'de> for Assignments {
        type Value = Vec<SpeakerAssignment>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "at most {MAX_SPEAKER_ASSIGNMENTS} speaker assignments")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut assignments = Vec::new();
            while assignments.len() < MAX_SPEAKER_ASSIGNMENTS {
                let Some(Object(assignment)) = sequence.next_element()? else {
                    return Ok(assignments);
                };
                assignments.push(assignment);
            }
            if sequence.next_element::<IgnoredAny>()?.is_some() {
                return Err(A::Error::custom("too many speaker assignments"));
            }
            Ok(assignments)
        }
    }
    deserializer.deserialize_seq(Assignments)
}

impl SpeakerResultBody {
    /// Decode an exact bounded JSON body after the upload owner checks its digest.
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        if bytes.len() as u64 > MAX_SPEAKER_RESULT_BODY_BYTES {
            return Err(serde_json::Error::custom(
                "speaker result exceeds byte budget",
            ));
        }
        let body: Self = serde_json::from_slice(bytes)?;
        if !body.valid() {
            return Err(serde_json::Error::custom("invalid speaker result"));
        }
        Ok(body)
    }

    pub fn valid(&self) -> bool {
        let Self::Applied { assignments } = self else {
            return true;
        };
        if assignments.is_empty() || assignments.len() > MAX_SPEAKER_ASSIGNMENTS {
            return false;
        }
        let mut ids = HashSet::with_capacity(assignments.len());
        assignments.iter().all(|assignment| {
            (1..=MAX_SEGMENT_ID).contains(&assignment.segment_id)
                && ids.insert(assignment.segment_id)
        })
    }

    pub fn outcome(&self) -> SpeakerResultOutcome {
        match self {
            Self::Applied { .. } => SpeakerResultOutcome::Applied,
            Self::Skipped { reason } => SpeakerResultOutcome::Skipped { reason: *reason },
        }
    }
}

#[cfg(test)]
#[path = "speaker_result_tests.rs"]
mod tests;

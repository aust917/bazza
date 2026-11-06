//! Worker output for automatic local recognition of an already captured meeting.
//! A native pipeline failure uses the separate job failure operation.
use serde::{
    de::{Error as _, IgnoredAny, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::fmt;

pub const MAX_LOCAL_ASR_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_LOCAL_ASR_CAPTIONS: usize = 10_000;
const MAX_TIMELINE_MS: i64 = 9_007_199_254_740_991;

pub fn model_id_valid(model: &str) -> bool {
    (1..=128).contains(&model.len())
        && model.as_bytes()[0].is_ascii_alphanumeric()
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte))
}

pub(crate) fn language_id_valid(language: &str) -> bool {
    let mut parts = language.split('-');
    language.len() <= 64
        && parts.next().is_some_and(|primary| {
            (2..=8).contains(&primary.len())
                && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
        && parts.all(|part| {
            (1..=8).contains(&part.len())
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalAsrOutcome {
    Complete,
    NoModel,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAsrCaption {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

impl fmt::Debug for LocalAsrCaption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalAsrCaption")
            .field("start_ms", &self.start_ms)
            .field("end_ms", &self.end_ms)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAsrBody {
    /// Actual engine catalog ID, or the attempted quality ID for NoModel.
    pub model: String,
    pub language: Option<String>,
    pub outcome: LocalAsrOutcome,
    #[serde(deserialize_with = "bounded_captions")]
    pub captions: Vec<LocalAsrCaption>,
}

impl fmt::Debug for LocalAsrBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalAsrBody")
            .field("outcome", &self.outcome)
            .field("caption_count", &self.captions.len())
            .finish_non_exhaustive()
    }
}

fn bounded_captions<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<LocalAsrCaption>, D::Error> {
    struct Captions;
    impl<'de> Visitor<'de> for Captions {
        type Value = Vec<LocalAsrCaption>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "at most {MAX_LOCAL_ASR_CAPTIONS} local ASR captions")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut captions = Vec::new();
            while captions.len() < MAX_LOCAL_ASR_CAPTIONS {
                let Some(caption) = sequence.next_element()? else {
                    return Ok(captions);
                };
                captions.push(caption);
            }
            if sequence.next_element::<IgnoredAny>()?.is_some() {
                return Err(A::Error::custom("too many local ASR captions"));
            }
            Ok(captions)
        }
    }
    deserializer.deserialize_seq(Captions)
}

impl LocalAsrBody {
    /// Verify the exact received byte budget and decode one closed JSON value.
    /// The upload owner must verify stream length and digest before calling this.
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        if bytes.len() as u64 > MAX_LOCAL_ASR_BODY_BYTES {
            return Err(serde_json::Error::custom(
                "local ASR body exceeds byte budget",
            ));
        }
        let body: Self = serde_json::from_slice(bytes)?;
        if !body.valid() {
            return Err(serde_json::Error::custom("invalid local ASR result"));
        }
        Ok(body)
    }

    /// Validate metadata and caption shape without rewriting text or order.
    /// The encoded byte budget applies to the original stream in parse.
    pub fn valid(&self) -> bool {
        model_id_valid(&self.model)
            && self.language.as_deref().is_none_or(language_id_valid)
            && self.captions.len() <= MAX_LOCAL_ASR_CAPTIONS
            && (self.outcome == LocalAsrOutcome::Complete || self.captions.is_empty())
            && self.captions.iter().all(|caption| {
                (0..=MAX_TIMELINE_MS).contains(&caption.start_ms)
                    && (caption.start_ms..=MAX_TIMELINE_MS).contains(&caption.end_ms)
                    && !caption.text.trim().is_empty()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_result_preserves_order_zero_duration_nul_and_selected_fallback() {
        let text = "  Орион\0🙂 ";
        let value = json!({
            "model": "whisper-fallback",
            "language": "ru-RU",
            "outcome": "complete",
            "captions": [
                {"start_ms": 100, "end_ms": 100, "text": text},
                {"start_ms": 0, "end_ms": 100, "text": " earlier "}
            ]
        });
        let body = LocalAsrBody::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(body.model, "whisper-fallback");
        assert_eq!(body.captions[0].text, text);
        assert_eq!(body.captions[1].start_ms, 0);
        assert!(!format!("{body:?} {:?}", body.captions[0]).contains("Орион"));
        assert_eq!(serde_json::to_value(body).unwrap(), value);
    }

    #[test]
    fn missing_model_and_empty_success_remain_distinct_and_unsupported_fields_fail_closed() {
        for outcome in [LocalAsrOutcome::Complete, LocalAsrOutcome::NoModel] {
            let value = json!({
                "model": "whisper-medium",
                "language": null,
                "outcome": outcome,
                "captions": []
            });
            assert_eq!(
                LocalAsrBody::parse(&serde_json::to_vec(&value).unwrap())
                    .unwrap()
                    .outcome,
                outcome
            );
        }
        let mut value = json!({
            "model": "whisper-medium",
            "outcome": "no_model",
            "captions": [{"start_ms": 0, "end_ms": 1, "text": "not allowed"}]
        });
        assert!(LocalAsrBody::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value["outcome"] = json!("complete");
        for (field, bad) in [
            ("start_ms", json!(-1)),
            ("end_ms", json!(MAX_TIMELINE_MS + 1)),
            ("text", json!("   ")),
            ("is_final", json!(false)),
            ("speaker", json!("caller supplied")),
        ] {
            let mut candidate = value.clone();
            candidate["captions"][0][field] = bad;
            assert!(LocalAsrBody::parse(&serde_json::to_vec(&candidate).unwrap()).is_err());
        }
        for (field, bad) in [
            ("model", json!("C:/private/model.bin")),
            ("language", json!("en_not_bcp")),
            ("provider", json!("soniox")),
            ("error", json!("caller supplied")),
        ] {
            let mut candidate = value.clone();
            candidate[field] = bad;
            assert!(LocalAsrBody::parse(&serde_json::to_vec(&candidate).unwrap()).is_err());
        }
    }

    #[test]
    fn exact_encoded_bytes_and_decoded_count_are_bounded_independently() {
        let mut data = br#"{"model":"local","outcome":"no_model","captions":[]}"#.to_vec();
        data.resize(MAX_LOCAL_ASR_BODY_BYTES as usize, b' ');
        assert!(LocalAsrBody::parse(&data).is_ok());
        data.push(b' ');
        assert!(LocalAsrBody::parse(&data).is_err());

        let caption = json!({"start_ms": 0, "end_ms": 1, "text": "x"});
        let mut value = json!({
            "model": "local",
            "outcome": "complete",
            "captions": vec![caption.clone(); MAX_LOCAL_ASR_CAPTIONS]
        });
        assert!(LocalAsrBody::parse(&serde_json::to_vec(&value).unwrap()).is_ok());
        value["captions"].as_array_mut().unwrap().push(caption);
        assert!(serde_json::from_value::<LocalAsrBody>(value).is_err());
    }
}

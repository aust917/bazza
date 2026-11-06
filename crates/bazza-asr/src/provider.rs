//! Provider-neutral transcript selection and provenance. Times use the recording's
//! active 16 kHz mono audio timeline (pauses contain no samples).
use crate::Caption;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    #[default]
    Local,
    Soniox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Streaming,
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub provider: SttProvider,
    pub model: String,
    pub language: Option<String>,
    pub status: TranscriptStatus,
    pub error: Option<String>,
    pub captions: Vec<Caption>,
    /// Present only for a transcript retained from completed local live captions.
    /// Labels are paired with captions by position, including unnamed rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_speaker_labels: Option<Vec<Option<String>>>,
}

#[derive(Debug, Clone)]
pub enum SttEvent {
    Caption(Caption),
    Reconnecting,
    Finished(Result<(), String>),
}

impl Transcript {
    pub fn new(provider: SttProvider, model: String, language: Option<String>) -> Self {
        Self {
            provider, model, language, status: TranscriptStatus::Streaming, error: None,
            captions: Vec::new(), live_speaker_labels: None,
        }
    }
    /// Partial text is display-only and never becomes final merely because a
    /// connection ends. Confirmed text survives failed finalization.
    pub fn apply(&mut self, event: &SttEvent) {
        match event {
            SttEvent::Caption(c) if c.is_final => self.captions.push(c.clone()),
            SttEvent::Finished(result) => {
                self.status = if result.is_ok() { TranscriptStatus::Complete } else { TranscriptStatus::Incomplete };
                self.error = result.as_ref().err().cloned();
            }
            _ => {}
        }
    }

    pub fn retained_local_live_labels(&self) -> Option<&[Option<String>]> {
        self.live_speaker_labels.as_deref().filter(|labels| {
            self.provider == SttProvider::Local && self.status == TranscriptStatus::Complete
                && !labels.is_empty() && labels.len() == self.captions.len()
        })
    }
}

/// Automatic post-processing respects the saved choice, even after a network
/// failure or a later settings change. An explicit re-transcription may replace it.
pub fn use_saved_transcript(saved: Option<&Transcript>, explicit_retranscribe: bool) -> bool {
    !explicit_retranscribe && saved.is_some_and(|t| {
        t.provider == SttProvider::Soniox || t.retained_local_live_labels().is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partials_and_network_failure_preserve_confirmed_raw_text() {
        let mut t = Transcript::new(SttProvider::Soniox, "test".into(), None);
        let cap = |text: &str, is_final| SttEvent::Caption(Caption { start_ms: 0, end_ms: 100, text: text.into(), is_final });
        t.apply(&cap("draft", false));
        t.apply(&cap("raw words", true));
        t.apply(&SttEvent::Finished(Err("network unavailable".into())));
        assert_eq!(t.captions.len(), 1);
        assert_eq!(t.captions[0].text, "raw words");
        assert_eq!(t.status, TranscriptStatus::Incomplete);
        assert!(use_saved_transcript(Some(&t), false));
        assert!(!use_saved_transcript(Some(&t), true));
        assert!(!use_saved_transcript(None, false));
    }

    #[test]
    fn only_confirmed_local_live_is_retained_during_automatic_processing() {
        let mut t = Transcript::new(SttProvider::Local, "small".into(), None);
        t.status = TranscriptStatus::Complete;
        t.captions.push(Caption { start_ms: 10, end_ms: 40, text: "hello".into(), is_final: true });
        assert!(!use_saved_transcript(Some(&t), false));

        t.live_speaker_labels = Some(vec![Some("Марина".into())]);
        let restored: Transcript = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(restored.retained_local_live_labels().unwrap()[0].as_deref(), Some("Марина"));
        assert!(use_saved_transcript(Some(&restored), false));
        assert!(!use_saved_transcript(Some(&restored), true));
    }
}

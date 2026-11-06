//! JSON contract between the app and the `bazza-diarize` sidecar.

use crate::Turn;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiarizeBackend {
    #[default]
    Pyannote,
    Nemotron,
}

/// Passed on the command line. Legacy enrollment defaults to pyannote; meeting
/// diarization can explicitly select Nemotron without changing voiceprint models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizeRequest {
    #[serde(default)]
    pub backend: DiarizeBackend,
    #[serde(default)]
    pub seg_model: String,
    #[serde(default)]
    pub nemotron_model: Option<String>,
    #[serde(default)]
    pub nemotron_runtime: Option<String>,
    pub emb_model: String,
    pub wav: String,
    #[serde(default = "default_max_embed")]
    pub max_embed_secs: usize,
    /// Diarize only this window of the file (ms); `None` = whole file.
    #[serde(default)]
    pub start_ms: Option<i64>,
    #[serde(default)]
    pub end_ms: Option<i64>,
    /// ONNX threads per model; `None` = the sidecar's default for this machine.
    #[serde(default)]
    pub threads: Option<i32>,
}

fn default_max_embed() -> usize {
    60
}

/// Printed to stdout as one JSON document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DiarizeOutput {
    pub turns: Vec<Turn>,
    /// cluster id -> L2-normalised voice embedding (clusters with < 1 s of audio are omitted)
    pub embeddings: BTreeMap<i32, Vec<f32>>,
    pub embedding_dim: usize,
    #[serde(default)]
    pub error: Option<String>,
    /// Versioned observations from original native clusters. Missing evidence
    /// remains explicit; it does not establish within-cluster consistency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_evidence: Option<crate::voice_evidence::VoiceEvidence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_requests_keep_the_enrollment_backend() {
        let request: DiarizeRequest = serde_json::from_str(r#"{"seg_model":"seg.onnx","emb_model":"tita.onnx","wav":"sample.wav"}"#).unwrap();
        assert_eq!(request.backend, DiarizeBackend::Pyannote);
        assert!(request.nemotron_model.is_none());
        assert_eq!(request.max_embed_secs, 60);
    }

    #[test]
    fn output_roundtrips_json() {
        let mut o = DiarizeOutput { turns: vec![Turn { start_ms: 0, end_ms: 10, cluster: 1 }], ..Default::default() };
        o.embeddings.insert(1, vec![0.5, 0.5]);
        o.embedding_dim = 2;
        let s = serde_json::to_string(&o).unwrap();
        assert_eq!(serde_json::from_str::<DiarizeOutput>(&s).unwrap(), o);
    }
}

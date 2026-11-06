//! Who spoke when: the pure logic to attach diarization clusters to captions,
//! spot the local user, and verify voices against independent enrollment samples.
//! The sherpa-onnx engine itself runs in the `bazza-diarize` sidecar
//! (crate `bazza-diarize-cli`); `protocol` defines its JSON output.

pub mod consolidate;
pub mod identify;
pub mod merge;
pub mod protocol;
pub mod shards;
pub mod voice_evidence;
pub mod voice_policy;

pub use consolidate::consolidate;
pub use identify::{best_match, centroid, cosine, KnownSpeaker};
pub use merge::{assign_clusters, find_me_cluster, Turn};
pub use protocol::{DiarizeBackend, DiarizeOutput, DiarizeRequest};

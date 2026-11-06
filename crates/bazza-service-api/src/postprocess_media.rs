//! Private, lease-bound access to a fixed set of captured media artifacts.
//! The grant never renews a worker registration or a post-process claim.
use crate::{
    desktop::{desktop_body_fits, desktop_scope, MAX_DESKTOP_DATA_BYTES},
    postprocess::{provider_valid, PostProcessJobLease, PostProcessSourceFence},
    worker::{CaptureSelection, WorkerProof, WorkerSecret},
};
use bazza_domain::capture_screen::{PostProcessVideoFailure, RawScreenWriterOutcome};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

pub const FIXTURE_POST_PROCESS_MEDIA_CAPABILITY: &str = "experimental_fixture_post_process_media";
pub const MAX_POST_PROCESS_MEDIA_PATH_BYTES: usize = 4096;
pub const MAX_POST_PROCESS_MEDIA_ARTIFACTS: usize = 7;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessMediaRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub owner_nonce: Uuid,
    pub expected_provider: CaptureSelection,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessMediaKind {
    AudioMix,
    AudioMic,
    AudioSystem,
    RawVideo,
    VideoTimeline,
    VideoClock,
    AudioClock,
}

impl PostProcessMediaKind {
    pub const ALL: [Self; MAX_POST_PROCESS_MEDIA_ARTIFACTS] = [
        Self::AudioMix,
        Self::AudioMic,
        Self::AudioSystem,
        Self::RawVideo,
        Self::VideoTimeline,
        Self::VideoClock,
        Self::AudioClock,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            Self::AudioMix => "mix.wav",
            Self::AudioMic => "mic.wav",
            Self::AudioSystem => "sys.wav",
            Self::RawVideo => "screen.raw.mp4",
            Self::VideoTimeline => "screen.raw.timeline.json",
            Self::VideoClock => "screen.raw.clock.txt",
            Self::AudioClock => "mix.clock.json",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessMediaArtifact {
    pub kind: PostProcessMediaKind,
    pub private_path: String,
    pub bytes: u64,
}

impl std::fmt::Debug for PostProcessMediaArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostProcessMediaArtifact")
            .field("kind", &self.kind)
            .field("private_path", &"[redacted]")
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl PostProcessMediaArtifact {
    pub fn valid(&self) -> bool {
        self.private_path.len() <= MAX_POST_PROCESS_MEDIA_PATH_BYTES
            && !self.private_path.contains('\0')
            && Path::new(&self.private_path).is_absolute()
            && Path::new(&self.private_path)
                .file_name()
                .is_some_and(|name| name == self.kind.file_name())
            && self.bytes <= MAX_SAFE_INTEGER
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PostProcessMediaGrant {
    pub library_scope: String,
    pub library_epoch: Uuid,
    pub worker_session_id: Uuid,
    pub claim_id: Uuid,
    pub job_id: String,
    pub job_revision: String,
    pub meeting_id: Uuid,
    pub input: PostProcessSourceFence,
    pub owner_nonce: Uuid,
    pub provider: CaptureSelection,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_writer: Option<RawScreenWriterOutcome>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub video_publication_candidate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_failure: Option<PostProcessVideoFailure>,
    pub artifacts: Vec<PostProcessMediaArtifact>,
}

impl PostProcessMediaRequest {
    pub fn valid(&self) -> bool {
        self.lease.valid()
            && self.lease.expires_at > Utc::now()
            && !self.proof.session_id.is_nil()
            && !self.proof.library_epoch.is_nil()
            && self.proof.session_id == self.lease.worker_session_id
            && self.proof.library_epoch == self.lease.library_epoch
            && !self.owner_nonce.is_nil()
            && provider_valid(&self.expected_provider)
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
    }
}

impl PostProcessMediaGrant {
    pub fn valid_for(&self, request: &PostProcessMediaRequest) -> bool {
        request.valid()
            && desktop_scope(&self.library_scope)
            && self.library_epoch == request.proof.library_epoch
            && self.worker_session_id == request.proof.session_id
            && self.claim_id == request.lease.claim_id
            && self.job_id == request.lease.job_id
            && self.job_revision == request.lease.job_revision
            && self.meeting_id == request.lease.meeting_id
            && self.input == request.lease.input
            && self.owner_nonce == request.owner_nonce
            && self.provider == request.expected_provider
            && self.expires_at == request.lease.expires_at
            && self.expires_at > Utc::now()
            && (self.screen_writer.is_some()
                || (!self.video_publication_candidate && self.video_failure.is_none()))
            && !(self.video_publication_candidate && self.video_failure.is_some())
            && (1..=MAX_POST_PROCESS_MEDIA_ARTIFACTS).contains(&self.artifacts.len())
            && self.artifacts.iter().all(PostProcessMediaArtifact::valid)
            && self
                .artifacts
                .iter()
                .filter(|artifact| artifact.kind == PostProcessMediaKind::AudioMix)
                .count()
                == 1
            && !self.artifacts.iter().enumerate().any(|(index, artifact)| {
                self.artifacts[..index]
                    .iter()
                    .any(|previous| previous.kind == artifact.kind)
            })
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::CaptureProvider;
    use chrono::Duration;

    fn request() -> PostProcessMediaRequest {
        let session = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        PostProcessMediaRequest {
            proof: WorkerProof {
                session_id: session,
                secret: WorkerSecret::random(),
                library_epoch: epoch,
            },
            lease: PostProcessJobLease {
                operation_id: Uuid::new_v4(),
                claim_id: Uuid::new_v4(),
                worker_session_id: session,
                library_epoch: epoch,
                job_id: "media-job".into(),
                meeting_id: Uuid::new_v4(),
                job_revision: "claimed-revision".into(),
                input: PostProcessSourceFence {
                    capture_id: Uuid::new_v4(),
                    capture_generation: Uuid::new_v4(),
                    checkpoint_revision: "checkpoint-original".into(),
                    lifecycle_revision: "capture-finalized".into(),
                    transcript_revision: "captured-transcript".into(),
                    source_id: Some("captured-source".into()),
                },
                expires_at: Utc::now() + Duration::seconds(60),
                change_sequence: 5,
                dedup_until: Utc::now() + Duration::days(7),
            },
            fence: WorkerSecret::random(),
            owner_nonce: Uuid::new_v4(),
            expected_provider: CaptureSelection {
                provider: CaptureProvider::Local,
                model: Some("whisper-small".into()),
                language: Some("ru".into()),
            },
        }
    }

    fn path(name: &str) -> String {
        #[cfg(windows)]
        let root = Path::new(r"C:\private-library\recordings\meeting");
        #[cfg(not(windows))]
        let root = Path::new("/private-library/recordings/meeting");
        root.join(name).to_str().unwrap().into()
    }

    fn grant(request: &PostProcessMediaRequest) -> PostProcessMediaGrant {
        PostProcessMediaGrant {
            library_scope: "a".repeat(32),
            library_epoch: request.proof.library_epoch,
            worker_session_id: request.proof.session_id,
            claim_id: request.lease.claim_id,
            job_id: request.lease.job_id.clone(),
            job_revision: request.lease.job_revision.clone(),
            meeting_id: request.lease.meeting_id,
            input: request.lease.input.clone(),
            owner_nonce: request.owner_nonce,
            provider: request.expected_provider.clone(),
            expires_at: request.lease.expires_at,
            screen_writer: None,
            video_publication_candidate: false,
            video_failure: None,
            artifacts: PostProcessMediaKind::ALL
                .iter()
                .map(|kind| PostProcessMediaArtifact {
                    kind: *kind,
                    private_path: path(kind.file_name()),
                    bytes: 42,
                })
                .collect(),
        }
    }

    #[test]
    fn request_requires_live_owned_lease_and_denies_arbitrary_file_selectors() {
        let request = request();
        assert!(request.valid());
        for fault in 0..5 {
            let mut bad = request.clone();
            match fault {
                0 => bad.lease.expires_at = Utc::now() - Duration::seconds(1),
                1 => bad.proof.session_id = Uuid::new_v4(),
                2 => bad.proof.library_epoch = Uuid::new_v4(),
                3 => bad.owner_nonce = Uuid::nil(),
                _ => bad.expected_provider.model = Some("../model".into()),
            }
            assert!(!bad.valid(), "fault {fault}");
        }
        for field in ["path", "filename", "kind", "role", "peer_pid"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = serde_json::json!("caller selector");
            assert!(serde_json::from_value::<PostProcessMediaRequest>(value).is_err());
        }
        let debug = format!("{request:?}");
        assert!(!debug.contains(request.proof.secret.expose()));
        assert!(!debug.contains(request.fence.expose()));
    }

    #[test]
    fn fixed_artifact_contract_is_closed_and_debug_hides_every_private_path() {
        assert_eq!(
            PostProcessMediaKind::ALL.map(PostProcessMediaKind::file_name),
            [
                "mix.wav",
                "mic.wav",
                "sys.wav",
                "screen.raw.mp4",
                "screen.raw.timeline.json",
                "screen.raw.clock.txt",
                "mix.clock.json",
            ]
        );
        assert!(serde_json::from_str::<PostProcessMediaKind>("\"preview\"").is_err());
        let request = request();
        let grant = grant(&request);
        assert!(grant.valid_for(&request));
        let debug = format!("{grant:?}");
        for artifact in &grant.artifacts {
            assert!(!debug.contains(&artifact.private_path));
        }
    }

    #[test]
    fn grant_pins_original_claim_source_provider_nonce_and_actual_expiry() {
        let request = request();
        let grant = grant(&request);
        for fault in 0..12 {
            let mut bad = grant.clone();
            match fault {
                0 => bad.library_scope = "wrong-scope".into(),
                1 => bad.library_epoch = Uuid::new_v4(),
                2 => bad.worker_session_id = Uuid::new_v4(),
                3 => bad.claim_id = Uuid::new_v4(),
                4 => bad.job_id = "other-job".into(),
                5 => bad.job_revision = "other-revision".into(),
                6 => bad.meeting_id = Uuid::new_v4(),
                7 => bad.input.capture_generation = Uuid::new_v4(),
                8 => bad.input.source_id = None,
                9 => bad.owner_nonce = Uuid::new_v4(),
                10 => bad.provider.language = Some("en".into()),
                _ => bad.expires_at += Duration::seconds(1),
            }
            assert!(!bad.valid_for(&request), "fault {fault}");
        }
    }

    #[test]
    fn artifacts_require_one_mix_unique_fixed_names_safe_sizes_and_encoded_budget() {
        let request = request();
        let grant = grant(&request);
        for fault in 0..9 {
            let mut bad = grant.clone();
            match fault {
                0 => bad.artifacts.clear(),
                1 => {
                    bad.artifacts.remove(0);
                }
                2 => bad.artifacts.push(bad.artifacts[0].clone()),
                3 => bad.artifacts[1] = bad.artifacts[0].clone(),
                4 => bad.artifacts[0].private_path = "mix.wav".into(),
                5 => bad.artifacts[0].private_path = path("wrong.wav"),
                6 => bad.artifacts[0].private_path.insert(3, '\0'),
                7 => bad.artifacts[0].bytes = MAX_SAFE_INTEGER + 1,
                _ => bad.artifacts[0].private_path = path(&format!("{}/mix.wav", "я".repeat(2048))),
            }
            assert!(!bad.valid_for(&request), "fault {fault}");
        }
        let mut bad = grant.clone();
        for artifact in &mut bad.artifacts {
            artifact.private_path = path(&format!(
                "{}/{}",
                "\u{1}".repeat(4000),
                artifact.kind.file_name()
            ));
            assert!(artifact.valid());
        }
        assert!(
            !bad.valid_for(&request),
            "JSON escaping counts against the full grant budget"
        );
        let mut single = grant;
        single.artifacts.truncate(1);
        single.artifacts[0].bytes = MAX_SAFE_INTEGER;
        assert!(single.valid_for(&request));
    }
}

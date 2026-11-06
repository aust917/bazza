//! Experimental private worker/capture messages for synthetic repositories.
//! The host supplies the authenticated OS peer PID. These are not public MCP
//! tools, a library grant, or a production GUI cutover protocol.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// A client-held random secret encoded as 64 lowercase hexadecimal characters.
/// Credentials travel only over authenticated private IPC and are redacted from
/// Debug output. Registration retries retain the same session ID and secret.
#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct WorkerSecret(String);

impl std::fmt::Debug for WorkerSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerSecret([redacted])")
    }
}

impl WorkerSecret {
    pub fn random() -> Self {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).expect("system random source unavailable");
        Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub fn from_string(value: String) -> Result<Self, &'static str> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid worker secret");
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WorkerSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_string(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCapability {
    Capture,
    PostProcess,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistration {
    pub session_id: Uuid,
    pub secret: WorkerSecret,
    pub capabilities: Vec<WorkerCapability>,
}

impl WorkerRegistration {
    pub fn new(capabilities: Vec<WorkerCapability>) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            secret: WorkerSecret::random(),
            capabilities,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerProof {
    pub session_id: Uuid,
    pub secret: WorkerSecret,
    pub library_epoch: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegisteredWorker {
    pub session_id: Uuid,
    pub library_epoch: Uuid,
    pub peer_pid: u32,
    pub capabilities: Vec<WorkerCapability>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProvider {
    Local,
    Soniox,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureSelection {
    pub provider: CaptureProvider,
    /// None preserves recording without an available ASR model.
    pub model: Option<String>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureStart {
    pub capture_id: Uuid,
    pub meeting_id: Uuid,
    pub fence: WorkerSecret,
    pub title: String,
    pub app: Option<String>,
    pub started_at: DateTime<Utc>,
    pub participants: Vec<String>,
    pub provider: CaptureSelection,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureLease {
    pub capture_id: Uuid,
    pub meeting_id: Uuid,
    pub worker_session_id: Uuid,
    pub generation: Uuid,
    pub library_epoch: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterWorkerRequest {
    pub registration: WorkerRegistration,
    pub ttl_seconds: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewWorkerRequest {
    pub proof: WorkerProof,
    pub ttl_seconds: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareCaptureRequest {
    pub proof: WorkerProof,
    pub start: CaptureStart,
}

/// Hold ArtifactGuard across this call. Raw writers and confirmed-journal
/// writes may start only after its successful ACK; retries use the same input.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginCaptureRequest {
    pub proof: WorkerProof,
    pub lease: CaptureLease,
    pub fence: WorkerSecret,
}

/// Compensate only an unused reservation. The core acquires the exclusive
/// pristine artifact guard; caller claims of emptiness are never accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbortCaptureRequest {
    pub proof: WorkerProof,
    pub lease: CaptureLease,
    pub fence: WorkerSecret,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReattachCaptureRequest {
    pub proof: WorkerProof,
    pub capture_id: Uuid,
    pub fence: WorkerSecret,
    pub ttl_seconds: i64,
}

/// Terminal evidence and confirmed captions are read and verified by the core
/// from the durable handoff while holding the released artifact lock. The wire
/// cannot supply an alternate body or claim that a writer has closed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeCaptureRequest {
    pub proof: WorkerProof,
    pub lease: CaptureLease,
    pub fence: WorkerSecret,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerMutationOutcome {
    pub entity_id: String,
    pub revision: String,
    pub change_sequence: i64,
    pub job_id: Option<String>,
    pub dedup_until: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Request;

    #[test]
    fn credential_debug_is_redacted_and_invalid_secrets_fail_without_echo() {
        let secret = WorkerSecret::random();
        assert_eq!(secret.expose().len(), 64);
        assert!(!format!("{secret:?}").contains(secret.expose()));
        for invalid in [
            "sensitive invalid credential".into(),
            "A".repeat(64),
            "a".repeat(63),
        ] {
            let value = serde_json::to_string(&invalid).unwrap();
            let error = serde_json::from_str::<WorkerSecret>(&value).unwrap_err();
            assert!(!error.to_string().contains(&invalid));
        }
    }

    #[test]
    fn private_worker_requests_cannot_choose_peer_pid_role_or_terminal_body() {
        let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
        let mut request = serde_json::to_value(Request::RegisterWorker(RegisterWorkerRequest {
            registration: registration.clone(),
            ttl_seconds: 60,
        }))
        .unwrap();
        request["params"]["registration"]["peer_pid"] = 1.into();
        assert!(serde_json::from_value::<Request>(request).is_err());
        let proof = WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: Uuid::new_v4(),
        };
        let lease = CaptureLease {
            capture_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            worker_session_id: proof.session_id,
            generation: Uuid::new_v4(),
            library_epoch: proof.library_epoch,
        };
        let mut request = serde_json::to_value(Request::FinalizeCapture(FinalizeCaptureRequest {
            proof,
            lease,
            fence: WorkerSecret::random(),
        }))
        .unwrap();
        request["params"]["captions"] = serde_json::json!([]);
        assert!(serde_json::from_value::<Request>(request).is_err());
    }

    #[test]
    fn begin_and_abort_cannot_accept_claimed_owners_emptiness_or_cleanup_bodies() {
        let epoch = Uuid::new_v4();
        let session = Uuid::new_v4();
        let request = Request::BeginCapture(BeginCaptureRequest {
            proof: WorkerProof {
                session_id: session,
                secret: WorkerSecret::random(),
                library_epoch: epoch,
            },
            lease: CaptureLease {
                capture_id: Uuid::new_v4(),
                meeting_id: Uuid::new_v4(),
                worker_session_id: session,
                generation: Uuid::new_v4(),
                library_epoch: epoch,
            },
            fence: WorkerSecret::random(),
        });
        let base = serde_json::to_value(request).unwrap();
        for method in ["begin_capture", "abort_capture"] {
            let mut valid = base.clone();
            valid["method"] = method.into();
            assert!(serde_json::from_value::<Request>(valid.clone()).is_ok());
            for key in [
                "owner_pid",
                "peer_pid",
                "empty",
                "delete_files",
                "terminal",
                "provider",
                "principal",
            ] {
                let mut invalid = valid.clone();
                invalid["params"][key] = true.into();
                assert!(
                    serde_json::from_value::<Request>(invalid).is_err(),
                    "{method}/{key}"
                );
            }
        }
    }
}

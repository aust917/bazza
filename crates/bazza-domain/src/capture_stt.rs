//! Closed evidence of Soniox finalization, independent from the media writer.

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub const SONIOX_PERMISSION_MAX_MS: u32 = 8_000;

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SonioxCredential(String);

impl SonioxCredential {
    pub fn parse(value: String) -> Option<Self> {
        (!value.trim().is_empty() && value.len() <= 4_096 && !value.chars().any(char::is_control))
            .then_some(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SonioxCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SonioxCredential([redacted])")
    }
}

impl<'de> Deserialize<'de> for SonioxCredential {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?)
            .ok_or_else(|| serde::de::Error::custom("invalid Soniox credential"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonioxPermissionPermit {
    pub capture_id: Uuid,
    pub meeting_id: Uuid,
    pub worker_session_id: Uuid,
    pub generation: Uuid,
    pub library_epoch: Uuid,
    pub permission_epoch: Uuid,
    pub model: String,
    pub language: Option<String>,
    pub valid_for_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SonioxPermissionReply {
    Issued {
        permit: SonioxPermissionPermit,
        credential: SonioxCredential,
    },
    Renewed {
        permit: SonioxPermissionPermit,
    },
    Stop {
        permission_epoch: Uuid,
        reason: SonioxIncompleteReason,
    },
    Drained {
        permission_epoch: Uuid,
    },
}

impl SonioxPermissionReply {
    pub fn permit(&self) -> Option<&SonioxPermissionPermit> {
        match self {
            Self::Issued { permit, .. } | Self::Renewed { permit } => Some(permit),
            Self::Stop { .. } | Self::Drained { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SonioxIncompleteReason {
    PermissionRevoked,
    ControlUnavailable,
    ProviderFailed,
    InputOverflow,
    FinalizationInterrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SonioxCaptureOutcome {
    Complete {},
    Incomplete { reason: SonioxIncompleteReason },
}

impl SonioxCaptureOutcome {
    /// Existing desktop transcript diagnostics are text. Keep them bounded and
    /// derive them locally from the closed reason instead of persisting a
    /// provider response, credential, network address, or arbitrary error.
    pub fn diagnostic(self) -> Option<&'static str> {
        match self {
            Self::Complete {} => None,
            Self::Incomplete { reason } => Some(match reason {
                SonioxIncompleteReason::PermissionRevoked => {
                    "Soniox permission or credentials changed."
                }
                SonioxIncompleteReason::ControlUnavailable => {
                    "Local service control became unavailable during Soniox transcription."
                }
                SonioxIncompleteReason::ProviderFailed => "Soniox did not finish successfully.",
                SonioxIncompleteReason::InputOverflow => {
                    "The Soniox audio input backlog exceeded its limit."
                }
                SonioxIncompleteReason::FinalizationInterrupted => {
                    "Soniox ended before finalization."
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json, to_value};

    #[test]
    fn outcome_is_closed_and_serializes_only_the_finalization_fact() {
        assert_eq!(
            to_value(SonioxCaptureOutcome::Complete {}).unwrap(),
            json!({"status":"complete"})
        );
        let stopped = SonioxCaptureOutcome::Incomplete {
            reason: SonioxIncompleteReason::PermissionRevoked,
        };
        assert_eq!(
            from_value::<SonioxCaptureOutcome>(
                json!({"status":"incomplete","reason":"permission_revoked"})
            )
            .unwrap(),
            stopped
        );
        for value in [
            json!({"status":"streaming"}),
            json!({"status":"complete","reason":"provider_failed"}),
            json!({"status":"incomplete"}),
            json!({"status":"incomplete","reason":"remote payload"}),
            json!({"status":"incomplete","reason":"provider_failed","credential":"forbidden"}),
        ] {
            assert!(from_value::<SonioxCaptureOutcome>(value).is_err());
        }
    }
}

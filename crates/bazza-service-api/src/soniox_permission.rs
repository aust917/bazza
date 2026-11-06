//! Private worker control for synthetic Soniox capture. The external MCP
//! protocol cannot issue or renew a permit and has no credential field.

use crate::worker::{CaptureLease, WorkerProof, WorkerSecret};
pub use bazza_domain::capture_stt::{
    SonioxCredential, SonioxIncompleteReason, SonioxPermissionPermit, SonioxPermissionReply,
    SONIOX_PERMISSION_MAX_MS,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FIXTURE_SONIOX_PERMISSION_CAPABILITY: &str = "experimental_fixture_soniox_permission";

/// Private desktop settings request. The response exposes presence only.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SonioxKeyOperation {
    Status {},
    Set { credential: SonioxCredential },
    Clear {},
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SonioxPermissionOperation {
    Issue {},
    Renew { permission_epoch: Uuid },
    AckDrained { permission_epoch: Uuid },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonioxPermissionRequest {
    pub proof: WorkerProof,
    pub lease: CaptureLease,
    pub fence: WorkerSecret,
    pub operation: SonioxPermissionOperation,
}

impl SonioxPermissionRequest {
    pub fn valid(&self) -> bool {
        let lease = &self.lease;
        lease.worker_session_id == self.proof.session_id
            && lease.library_epoch == self.proof.library_epoch
            && ![
                lease.capture_id,
                lease.meeting_id,
                lease.worker_session_id,
                lease.generation,
                lease.library_epoch,
            ]
            .iter()
            .any(Uuid::is_nil)
            && match self.operation {
                SonioxPermissionOperation::Issue {} => true,
                SonioxPermissionOperation::Renew { permission_epoch }
                | SonioxPermissionOperation::AckDrained { permission_epoch } => {
                    !permission_epoch.is_nil()
                }
            }
    }

    pub fn accepts(&self, reply: &SonioxPermissionReply) -> bool {
        if let Some(permit) = reply.permit() {
            let lease = &self.lease;
            if permit.capture_id != lease.capture_id
                || permit.meeting_id != lease.meeting_id
                || permit.worker_session_id != lease.worker_session_id
                || permit.generation != lease.generation
                || permit.library_epoch != lease.library_epoch
                || permit.permission_epoch.is_nil()
                || !(1..=SONIOX_PERMISSION_MAX_MS).contains(&permit.valid_for_ms)
                || permit.model.is_empty()
                || permit.model.len() > 128
                || !permit.model.as_bytes()[0].is_ascii_alphanumeric()
                || !permit
                    .model
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte))
                || permit.language.as_ref().is_some_and(|value| {
                    value.is_empty() || value.len() > 64 || value.chars().any(char::is_control)
                })
            {
                return false;
            }
        }
        match (self.operation, reply) {
            (SonioxPermissionOperation::Issue {}, SonioxPermissionReply::Issued { .. }) => true,
            (
                SonioxPermissionOperation::Renew { permission_epoch },
                SonioxPermissionReply::Renewed { permit },
            ) => permit.permission_epoch == permission_epoch,
            (
                SonioxPermissionOperation::Renew { permission_epoch },
                SonioxPermissionReply::Stop {
                    permission_epoch: epoch,
                    reason,
                },
            ) => {
                permission_epoch == *epoch
                    && matches!(
                        reason,
                        SonioxIncompleteReason::PermissionRevoked
                            | SonioxIncompleteReason::ControlUnavailable
                    )
            }
            (
                SonioxPermissionOperation::AckDrained { permission_epoch },
                SonioxPermissionReply::Drained {
                    permission_epoch: epoch,
                },
            ) => permission_epoch == *epoch,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_read::ExternalRequest;
    use serde_json::json;

    #[test]
    fn external_role_cannot_request_soniox_credentials() {
        assert!(serde_json::from_value::<ExternalRequest>(
            json!({"kind":"soniox_permission","data":{"operation":{"kind":"issue"}}})
        )
        .is_err());
        assert!(serde_json::from_value::<SonioxPermissionOperation>(
            json!({"kind":"issue","credential":"caller supplied"})
        )
        .is_err());
    }
}

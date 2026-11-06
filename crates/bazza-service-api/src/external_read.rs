//! Closed wire protocol for the separately authenticated external read endpoint.

use crate::{
    desktop::{desktop_body_fits, MAX_DESKTOP_DATA_BYTES},
    fixture_read::{FixtureReadReply, FixtureReadRequest},
    rpc::{ClientHello, RpcError},
    worker::WorkerSecret,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRequestEnvelope {
    pub request_id: Uuid,
    pub library_epoch: Uuid,
    pub deadline_unix_ms: u64,
    pub request: ExternalRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExternalRequest {
    Hello(ExternalHello),
    Read(FixtureReadRequest),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalHello {
    pub client: ClientHello,
    pub connection_id: Uuid,
    pub secret: WorkerSecret,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalReplyEnvelope {
    pub request_id: Uuid,
    pub library_epoch: Uuid,
    pub reply: ExternalReply,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExternalReply {
    Hello(ExternalHelloAccepted),
    Read(FixtureReadReply),
    Error(RpcError),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalHelloAccepted {
    pub library_id: Uuid,
    pub connection_id: Uuid,
    pub development_fixture_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueExternalReadGrant {
    pub client_label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalReadGrantIssued {
    pub library_id: Uuid,
    pub connection_id: Uuid,
    pub secret: WorkerSecret,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeExternalReadGrant {
    pub connection_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalReadGrantRevoked {
    pub library_id: Uuid,
    pub connection_id: Uuid,
}

pub const DEFAULT_EXTERNAL_READ_GRANT_LIMIT: u16 = 20;
pub const MAX_EXTERNAL_READ_GRANT_LIMIT: u16 = 50;

fn default_grant_limit() -> u16 {
    DEFAULT_EXTERNAL_READ_GRANT_LIMIT
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListExternalReadGrants {
    pub after: Option<Uuid>,
    #[serde(default = "default_grant_limit")]
    pub limit: u16,
}

impl ListExternalReadGrants {
    pub fn valid(&self) -> bool {
        (1..=MAX_EXTERNAL_READ_GRANT_LIMIT).contains(&self.limit)
            && self.after.is_none_or(|id| !id.is_nil())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalReadGrantPermission {
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalReadGrantMetadata {
    pub connection_id: Uuid,
    pub client_label: String,
    pub permission: ExternalReadGrantPermission,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalReadGrantPage {
    pub library_id: Uuid,
    pub grants: Vec<ExternalReadGrantMetadata>,
    pub next_after: Option<Uuid>,
}

impl ExternalReadGrantPage {
    pub fn valid_for(&self, request: &ListExternalReadGrants, library_id: Uuid) -> bool {
        let limit = usize::from(request.limit);
        request.valid()
            && !library_id.is_nil()
            && self.library_id == library_id
            && self.grants.len() <= limit
            && self.grants.iter().all(|grant| {
                !grant.connection_id.is_nil()
                    && request
                        .after
                        .is_none_or(|after| grant.connection_id > after)
                    && !grant.client_label.trim().is_empty()
                    && grant.client_label.len() <= 128
                    && !grant.client_label.chars().any(char::is_control)
            })
            && self
                .grants
                .windows(2)
                .all(|pair| pair[0].connection_id < pair[1].connection_id)
            && self.next_after.is_none_or(|id| {
                self.grants.len() == limit
                    && self
                        .grants
                        .last()
                        .is_some_and(|grant| grant.connection_id == id)
            })
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
    }
}

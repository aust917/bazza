//! Worker-only Soniox permission RPC and conservative monotonic deadline.

use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    rpc::{ErrorCode, Reply, Request, RpcError},
    soniox_permission::{SonioxPermissionReply, SonioxPermissionRequest},
};
use std::time::{Duration, Instant};

pub struct SonioxPermissionSnapshot {
    pub reply: SonioxPermissionReply,
    pub expires_at: Option<Instant>,
}

impl ServiceClient {
    pub async fn soniox_permission(
        &mut self,
        request: SonioxPermissionRequest,
    ) -> Result<SonioxPermissionSnapshot, ClientError> {
        if !request.valid() || request.lease.library_epoch != self.descriptor.library_epoch {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        // Count wire time against the permission. A reply that was delayed
        // cannot extend the local network worker's deadline.
        let sent = Instant::now();
        let Reply::SonioxPermission(reply) = self
            .call(Request::SonioxPermission(request.clone()))
            .await?
        else {
            return self.invalid_worker_reply();
        };
        if !request.accepts(&reply) {
            return self.invalid_worker_reply();
        }
        let expires_at = reply
            .permit()
            .map(|permit| sent + Duration::from_millis(u64::from(permit.valid_for_ms)));
        if expires_at.is_some_and(|deadline| deadline <= Instant::now()) {
            return Err(ClientError::Timeout);
        }
        Ok(SonioxPermissionSnapshot {
            reply: *reply,
            expires_at,
        })
    }
}

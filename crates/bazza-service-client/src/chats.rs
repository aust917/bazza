//! Bounded private chat reads. Each call returns exactly one typed response;
//! malformed replies poison the connection and are never retried implicitly.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    chats::{DesktopChatsReply, DesktopChatsRequest},
    desktop::DesktopEnvelope,
    rpc::{ErrorCode, Reply, Request, RpcError},
};

impl ServiceClient {
    pub async fn desktop_chats(
        &mut self,
        request: DesktopChatsRequest,
    ) -> Result<DesktopEnvelope<DesktopChatsReply>, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self.call(Request::DesktopChats(Box::new(request))).await? {
            Reply::DesktopChats(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

#[cfg(test)]
#[path = "chats_tests.rs"]
mod tests;

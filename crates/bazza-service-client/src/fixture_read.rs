//! Private read-only projection for the explicit synthetic MCP fixture.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    fixture_read::{FixtureReadReply, FixtureReadRequest},
    rpc::{ErrorCode, Reply, Request, RpcError},
};

impl ServiceClient {
    pub async fn fixture_read(
        &mut self,
        request: FixtureReadRequest,
    ) -> Result<FixtureReadReply, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        match self
            .call(Request::FixtureRead(Box::new(request.clone())))
            .await?
        {
            Reply::FixtureRead(reply) if reply.valid_for(&request, self.descriptor.library_id) => {
                Ok(*reply)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

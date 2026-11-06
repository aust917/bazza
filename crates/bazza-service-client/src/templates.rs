//! Closed private template transport; no implicit chunk/page/retry collection.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    rpc::{ErrorCode, Reply, Request, RpcError},
    templates::{DesktopTemplatesReply, DesktopTemplatesRequest},
};
impl ServiceClient {
    pub async fn desktop_templates(
        &mut self,
        request: DesktopTemplatesRequest,
    ) -> Result<DesktopEnvelope<DesktopTemplatesReply>, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::DesktopTemplates(Box::new(request)))
            .await?
        {
            Reply::DesktopTemplates(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

#[cfg(test)]
#[path = "templates_tests.rs"]
mod tests;

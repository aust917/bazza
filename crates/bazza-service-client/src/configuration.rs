//! Closed private native configuration calls; no public MCP settings surface.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    configuration::{
        native_configuration_revision, NativeConfiguration, UpdateNativeConfigurationRequest,
    },
    desktop::{DesktopEnvelope, DesktopMutationOutcome},
    rpc::{ErrorCode, Reply, Request, RpcError},
};

impl ServiceClient {
    pub async fn native_configuration(
        &mut self,
    ) -> Result<DesktopEnvelope<NativeConfiguration>, ClientError> {
        match self.call(Request::NativeConfiguration).await? {
            Reply::NativeConfiguration(value)
                if self.desktop_envelope_valid(&value) && value.data.valid() =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn update_native_configuration(
        &mut self,
        request: UpdateNativeConfigurationRequest,
    ) -> Result<DesktopEnvelope<DesktopMutationOutcome>, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        match self
            .call(Request::UpdateNativeConfiguration(Box::new(request)))
            .await?
        {
            Reply::NativeConfigurationUpdated(value)
                if self.desktop_envelope_valid(&value)
                    && value.data.entity_id == "native_configuration"
                    && native_configuration_revision(&value.data.revision)
                    && (1..=9_007_199_254_740_991).contains(&value.data.change_sequence)
                    && value.data.job_id.is_none() =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

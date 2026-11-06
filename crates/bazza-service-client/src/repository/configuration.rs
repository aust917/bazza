use super::{client, snapshot, RepositoryClient, RepositorySnapshot};
use crate::ClientError;
use bazza_service_api::{
    configuration::{NativeConfiguration, UpdateNativeConfigurationRequest},
    desktop::DesktopMutationOutcome,
    rpc::{Reply, Request},
    soniox_permission::SonioxKeyOperation,
};

impl RepositoryClient {
    pub async fn soniox_key(&self, operation: SonioxKeyOperation) -> Result<bool, ClientError> {
        let mut state = self.shared.state.lock().await;
        match client(&mut state)?.call(Request::SonioxKey(operation)).await? {
            Reply::SonioxKeyPresent(present) => Ok(present),
            _ => Err(ClientError::Protocol),
        }
    }

    pub async fn native_configuration(
        &self,
    ) -> Result<RepositorySnapshot<NativeConfiguration>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?.native_configuration().await?.data;
        Ok(snapshot(&state, data))
    }
    pub async fn update_native_configuration(
        &self,
        request: UpdateNativeConfigurationRequest,
    ) -> Result<RepositorySnapshot<DesktopMutationOutcome>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?
            .update_native_configuration(request)
            .await?
            .data;
        Ok(snapshot(&state, data))
    }
}

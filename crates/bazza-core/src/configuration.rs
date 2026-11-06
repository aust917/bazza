//! Private application configuration persistence. The fixture gate remains in
//! Repository::open_fixture; public MCP principals cannot select this boundary.
use crate::{desktop, CoreError, Repository};
use bazza_service_api::{
    configuration::{NativeConfiguration, UpdateNativeConfigurationRequest},
    desktop::{DesktopEnvelope, DesktopMutationOutcome},
    soniox_permission::SonioxKeyOperation,
};
use bazza_store::service_data::DataError;

impl Repository {
    pub fn soniox_key(&self, peer_pid: u32, operation: SonioxKeyOperation) -> Result<bool, CoreError> {
        desktop::authenticated_peer(peer_pid)?;
        match operation {
            SonioxKeyOperation::Status {} => Ok(self.store.service_soniox_key_present()?),
            SonioxKeyOperation::Set { credential } => {
                // The existing authority trigger revokes permits when the key
                // changes, including an explicit clear.
                self.store.set_setting(
                    "private:soniox_api_key",
                    &credential.expose(),
                )?;
                Ok(true)
            }
            SonioxKeyOperation::Clear {} => {
                self.store.set_setting("private:soniox_api_key", &"")?;
                Ok(false)
            }
        }
    }

    pub fn native_configuration(
        &self,
        peer_pid: u32,
    ) -> Result<DesktopEnvelope<NativeConfiguration>, CoreError> {
        desktop::authenticated_peer(peer_pid)?;
        let value = self.store.service_native_configuration()?;
        let value: NativeConfiguration = serde_json::from_value(serde_json::to_value(value)?)?;
        if !value.valid() {
            return Err(DataError::Invalid("native configuration reply").into());
        }
        self.desktop_envelope(value)
    }

    pub fn update_native_configuration(
        &self,
        peer_pid: u32,
        request: UpdateNativeConfigurationRequest,
    ) -> Result<DesktopEnvelope<DesktopMutationOutcome>, CoreError> {
        desktop::authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(DataError::Invalid("native configuration request").into());
        }
        let context = desktop::context(peer_pid, request.idempotency_key)?;
        let outcome = self.store.service_update_native_configuration(
            &context,
            &request.expected_revision,
            &request.settings,
        )?;
        self.desktop_envelope(serde_json::from_value(serde_json::to_value(outcome)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_test_support::create_v8;

    #[test]
    fn private_configuration_authenticates_before_read_or_write_and_bounds_changes() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let before = repo.native_configuration(42).unwrap();
        let request = UpdateNativeConfigurationRequest {
            expected_revision: before.data.revision.clone(),
            idempotency_key: "configuration-auth".into(),
            settings: before.data.settings.clone(),
        };
        assert!(matches!(
            repo.native_configuration(0),
            Err(CoreError::Data(DataError::Forbidden))
        ));
        assert!(matches!(
            repo.update_native_configuration(0, request.clone()),
            Err(CoreError::Data(DataError::Forbidden))
        ));
        let mut oversized = request;
        oversized.settings.user_name = "\0".repeat(12_000);
        assert!(matches!(
            repo.update_native_configuration(42, oversized),
            Err(CoreError::Data(DataError::Invalid(_)))
        ));
        assert_eq!(repo.native_configuration(42).unwrap().data, before.data);
    }
}

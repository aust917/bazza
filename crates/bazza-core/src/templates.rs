//! Closed private template operations. Metadata/content stays in the service;
//! no public principal, generic setting, filesystem path or SQL is selected here.
use crate::{desktop, CoreError, Repository};
use bazza_service_api::{desktop::DesktopEnvelope, templates as wire};
use bazza_store::service_data as data;

impl Repository {
    /// Private bounded cleanup; never seeds or publishes a template.
    pub fn expire_template_uploads(&self) -> Result<u32, CoreError> {
        Ok(self.store.service_expire_template_uploads(32)?)
    }

    pub fn desktop_templates(
        &self,
        peer_pid: u32,
        request: wire::DesktopTemplatesRequest,
    ) -> Result<DesktopEnvelope<wire::DesktopTemplatesReply>, CoreError> {
        desktop::authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop templates request").into());
        }
        let domain_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self
            .store
            .service_desktop_templates("desktop", &domain_request)?;
        let result: wire::DesktopTemplatesReply =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(data::DataError::Invalid("desktop templates reply").into());
        }
        self.desktop_envelope(result)
    }
}

//! Pin administrative grant operations and credential checks to the selected
//! library. The host owns role separation and the read/revoke publication gate.
use crate::{CoreError, Repository};
use bazza_store::service_data::{ExternalReadGrant, ExternalReadSecret, IssuedExternalReadGrant};
use uuid::Uuid;

impl Repository {
    pub fn issue_external_read_grant(
        &self,
        client_label: &str,
    ) -> Result<IssuedExternalReadGrant, CoreError> {
        Ok(self
            .store
            .service_issue_external_read_grant(self.library_id, client_label)?)
    }

    pub fn list_external_read_grants(
        &self,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<ExternalReadGrant>, CoreError> {
        Ok(self
            .store
            .service_list_external_read_grants(self.library_id, after, limit)?)
    }

    pub fn revoke_external_read_grant(
        &self,
        connection_id: Uuid,
    ) -> Result<ExternalReadGrant, CoreError> {
        Ok(self
            .store
            .service_revoke_external_read_grant(self.library_id, connection_id)?)
    }

    pub fn authorize_external_read(
        &self,
        connection_id: Uuid,
        secret: &ExternalReadSecret,
    ) -> Result<(), CoreError> {
        Ok(self
            .store
            .service_authorize_external_read(self.library_id, connection_id, secret)?)
    }
}

//! Closed, staged automatic overlays. Native execution remains independently
//! gated; the store commits the complete body and new worker checkpoint once.
use crate::{CoreError, Repository};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_cleanup::{PostProcessCleanupReply, PostProcessCleanupRequest},
};
use bazza_store::service_data as data;

impl Repository {
    /// Removes expired private upload bodies without starting any execution.
    pub fn expire_post_process_cleanup_uploads(&self) -> Result<usize, CoreError> {
        Ok(self.store.service_expire_post_process_cleanup_uploads(32)?)
    }

    pub fn post_process_cleanup(
        &self,
        peer_pid: u32,
        request: PostProcessCleanupRequest,
    ) -> Result<DesktopEnvelope<PostProcessCleanupReply>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process cleanup request").into());
        }
        let stored_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result =
            self.store
                .service_post_process_cleanup(peer_pid, self.epoch, &stored_request)?;
        let result: PostProcessCleanupReply =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(data::DataError::Invalid("post-process cleanup receipt").into());
        }
        self.desktop_envelope(result)
    }
}

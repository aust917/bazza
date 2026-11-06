//! Private registered-worker publication of a staged automatic local ASR result.
use crate::{CoreError, Repository};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_source::{PostProcessSourceReply, PostProcessSourceRequest},
};
use bazza_store::service_data as data;

impl Repository {
    pub fn post_process_source(
        &self,
        peer_pid: u32,
        request: PostProcessSourceRequest,
    ) -> Result<DesktopEnvelope<PostProcessSourceReply>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process source request").into());
        }
        let stored_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result =
            self.store
                .service_post_process_source(peer_pid, self.epoch, &stored_request)?;
        let result: PostProcessSourceReply = serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(data::DataError::Invalid("post-process source receipt").into());
        }
        self.desktop_envelope(result)
    }
}

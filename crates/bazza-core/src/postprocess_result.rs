//! Small registered-worker result commits. Native execution and file/body
//! publication remain separate boundaries.
use crate::{CoreError, Repository};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_result::{CommitPostProcessAutoTitleRequest, PostProcessAutoTitleReceipt},
};
use bazza_store::service_data as data;

impl Repository {
    pub fn commit_post_process_auto_title(
        &self,
        peer_pid: u32,
        request: CommitPostProcessAutoTitleRequest,
    ) -> Result<DesktopEnvelope<PostProcessAutoTitleReceipt>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process title request").into());
        }
        let stored_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self.store.service_commit_post_process_auto_title(
            peer_pid,
            self.epoch,
            &stored_request,
        )?;
        let result: PostProcessAutoTitleReceipt =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(data::DataError::Invalid("post-process title receipt").into());
        }
        self.desktop_envelope(result)
    }
}

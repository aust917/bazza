//! Fixture worker summary publication and terminal completion.
use crate::{CoreError, Repository};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_complete::{CompleteFixturePostProcessReceipt, CompleteFixturePostProcessRequest},
    postprocess_summary::{
        post_process_summary_request_valid, PostProcessSummaryReply, PostProcessSummaryRequest,
    },
};
use bazza_store::service_data::DataError;

impl Repository {
    pub fn post_process_summary(
        &self,
        peer_pid: u32,
        request: PostProcessSummaryRequest,
    ) -> Result<DesktopEnvelope<PostProcessSummaryReply>, CoreError> {
        if !post_process_summary_request_valid(&request) {
            return Err(DataError::Invalid("post-process summary request").into());
        }
        let stored = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self
            .store
            .service_post_process_summary(peer_pid, self.epoch, &stored)?;
        let result: PostProcessSummaryReply =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(DataError::Invalid("post-process summary receipt").into());
        }
        self.desktop_envelope(result)
    }

    pub fn complete_fixture_post_process(
        &self,
        peer_pid: u32,
        request: CompleteFixturePostProcessRequest,
    ) -> Result<DesktopEnvelope<CompleteFixturePostProcessReceipt>, CoreError> {
        if !request.valid() {
            return Err(DataError::Invalid("fixture completion request").into());
        }
        let stored = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self
            .store
            .service_complete_fixture_post_process(peer_pid, self.epoch, &stored)?;
        let result: CompleteFixturePostProcessReceipt =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(DataError::Invalid("fixture completion receipt").into());
        }
        self.desktop_envelope(result)
    }
}

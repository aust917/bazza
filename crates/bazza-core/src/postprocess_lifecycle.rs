//! Fixture-only anonymous speaker result and separate Ready transition.
use crate::{CoreError, Repository};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess_ready::{PostProcessReadyReceipt, PostProcessReadyRequest},
    postprocess_speakers::{
        post_process_speakers_request_valid, PostProcessSpeakersReply, PostProcessSpeakersRequest,
    },
};
use bazza_store::service_data::DataError;

impl Repository {
    pub fn post_process_speakers(
        &self,
        peer_pid: u32,
        request: PostProcessSpeakersRequest,
    ) -> Result<DesktopEnvelope<PostProcessSpeakersReply>, CoreError> {
        if !post_process_speakers_request_valid(&request) {
            return Err(DataError::Invalid("post-process speakers request").into());
        }
        let stored = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self
            .store
            .service_post_process_speakers(peer_pid, self.epoch, &stored)?;
        let result: PostProcessSpeakersReply =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(DataError::Invalid("post-process speakers reply").into());
        }
        self.desktop_envelope(result)
    }

    pub fn mark_post_process_ready(
        &self,
        peer_pid: u32,
        request: PostProcessReadyRequest,
    ) -> Result<DesktopEnvelope<PostProcessReadyReceipt>, CoreError> {
        if !request.valid() {
            return Err(DataError::Invalid("post-process ready request").into());
        }
        let stored = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self
            .store
            .service_mark_post_process_ready(peer_pid, self.epoch, &stored)?;
        let result: PostProcessReadyReceipt =
            serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(DataError::Invalid("post-process ready receipt").into());
        }
        self.desktop_envelope(result)
    }
}

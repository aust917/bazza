//! Read-only worker input. Authorization and pinned source reads share a Store
//! snapshot; this boundary does not execute native work or publish its output.
use crate::{CoreError, Repository};
use bazza_service_api::{
    content::DesktopTranscriptPage, desktop::DesktopEnvelope,
    postprocess_input::PostProcessTranscriptRequest,
};
use bazza_store::service_data as data;

impl Repository {
    pub fn post_process_transcript(
        &self,
        peer_pid: u32,
        request: PostProcessTranscriptRequest,
    ) -> Result<DesktopEnvelope<DesktopTranscriptPage>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process input request").into());
        }
        let stored_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        let page =
            self.store
                .service_post_process_transcript(peer_pid, self.epoch, &stored_request)?;
        let page: DesktopTranscriptPage = serde_json::from_value(serde_json::to_value(page)?)?;
        if !request.reply_pins_valid(&page) {
            return Err(data::DataError::Invalid("post-process input source").into());
        }
        self.desktop_envelope(page)
    }
}

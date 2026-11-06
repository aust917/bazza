//! Explicit transcript pages for a currently registered post-process owner.
//! No retry, source refresh, media path or full-document collection is implicit.
use crate::{content::transcript_page_valid, ClientError, ServiceClient};
use bazza_service_api::{
    content::DesktopTranscriptPage,
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_input::PostProcessTranscriptRequest,
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn post_process_transcript(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessTranscriptRequest,
    ) -> Result<DesktopEnvelope<DesktopTranscriptPage>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::PostProcessTranscript(Box::new(request)))
            .await?
        {
            Reply::PostProcessTranscript(value)
                if self.desktop_envelope_valid(&value)
                    && transcript_page_valid(&value.data, &original.page)
                    && original.reply_pins_valid(&value.data) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

#[cfg(test)]
#[path = "postprocess_input_tests.rs"]
mod tests;

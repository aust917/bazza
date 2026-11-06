//! Explicit anonymous speaker result uploads bound to a registered worker.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_speakers::{
        post_process_speakers_request_valid, PostProcessSpeakersReply, PostProcessSpeakersRequest,
    },
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn post_process_speakers(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessSpeakersRequest,
    ) -> Result<DesktopEnvelope<PostProcessSpeakersReply>, ClientError> {
        if !post_process_speakers_request_valid(&request)
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::PostProcessSpeakers(Box::new(request)))
            .await?
        {
            Reply::PostProcessSpeakers(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

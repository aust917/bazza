//! Explicit Ready transition. A historical receipt never renews a live claim.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_ready::{PostProcessReadyReceipt, PostProcessReadyRequest},
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn mark_post_process_ready(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessReadyRequest,
    ) -> Result<DesktopEnvelope<PostProcessReadyReceipt>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::MarkPostProcessReady(Box::new(request)))
            .await?
        {
            Reply::PostProcessReady(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

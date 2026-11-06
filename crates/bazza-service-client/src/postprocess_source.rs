//! Explicit staged local source calls. Retrying an operation uses its original
//! request and does not renew, refresh or rebind the worker claim.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_source::{PostProcessSourceReply, PostProcessSourceRequest},
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn post_process_source(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessSourceRequest,
    ) -> Result<DesktopEnvelope<PostProcessSourceReply>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::PostProcessSource(Box::new(request)))
            .await?
        {
            Reply::PostProcessSource(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

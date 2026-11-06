//! Explicit staged cleanup calls. Callers retain immutable operations, original
//! revisions and staged-body hashes; no renewal, retry or refresh is implicit.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_cleanup::{PostProcessCleanupReply, PostProcessCleanupRequest},
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn post_process_cleanup(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessCleanupRequest,
    ) -> Result<DesktopEnvelope<PostProcessCleanupReply>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::PostProcessCleanup(Box::new(request)))
            .await?
        {
            Reply::PostProcessCleanup(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

#[cfg(test)]
#[path = "postprocess_cleanup_tests.rs"]
mod tests;

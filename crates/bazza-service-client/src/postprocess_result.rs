//! A lease-bound auto-title result. A historical receipt is never a fresh lease;
//! callers explicitly retain and retry the same operation and original revision.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_result::{CommitPostProcessAutoTitleRequest, PostProcessAutoTitleReceipt},
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn commit_post_process_auto_title(
        &mut self,
        worker: &RegisteredWorker,
        request: CommitPostProcessAutoTitleRequest,
    ) -> Result<DesktopEnvelope<PostProcessAutoTitleReceipt>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::CommitPostProcessAutoTitle(Box::new(request)))
            .await?
        {
            Reply::PostProcessAutoTitle(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

#[cfg(test)]
#[path = "postprocess_result_tests.rs"]
mod tests;

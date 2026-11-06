//! Staged configured-summary output. Receipts carry a document revision.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_summary::{
        post_process_summary_request_valid, PostProcessSummaryReply, PostProcessSummaryRequest,
    },
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn post_process_summary(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessSummaryRequest,
    ) -> Result<DesktopEnvelope<PostProcessSummaryReply>, ClientError> {
        if !post_process_summary_request_valid(&request)
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::PostProcessSummary(Box::new(request)))
            .await?
        {
            Reply::PostProcessSummary(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

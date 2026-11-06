//! Complete a fixture job without deriving or extending a worker lease.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::post_process_worker_valid,
    postprocess_complete::{CompleteFixturePostProcessReceipt, CompleteFixturePostProcessRequest},
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::RegisteredWorker,
};

impl ServiceClient {
    pub async fn complete_fixture_post_process(
        &mut self,
        worker: &RegisteredWorker,
        request: CompleteFixturePostProcessRequest,
    ) -> Result<DesktopEnvelope<CompleteFixturePostProcessReceipt>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self
            .call(Request::CompleteFixturePostProcess(Box::new(request)))
            .await?
        {
            Reply::FixturePostProcessComplete(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

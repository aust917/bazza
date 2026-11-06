//! Registered-worker calls live on ServiceClient, not RepositoryClient. The
//! caller retains its verified RegisteredWorker and immutable proof/request for
//! explicit retries; a historical receipt never revives an expired lease.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::*,
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::{RegisteredWorker, WorkerProof},
};

fn bad_request() -> ClientError {
    RpcError::new(ErrorCode::InvalidArgument).into()
}

impl ServiceClient {
    fn post_process_registration_valid(
        &self,
        worker: &RegisteredWorker,
        proof: &WorkerProof,
    ) -> bool {
        worker.peer_pid == std::process::id()
            && post_process_worker_valid(worker, proof, self.descriptor.library_epoch)
    }

    pub async fn post_process_jobs(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessJobsRequest,
    ) -> Result<DesktopEnvelope<PostProcessJobPage>, ClientError> {
        if !request.valid() || !self.post_process_registration_valid(worker, &request.proof) {
            return Err(bad_request());
        }
        let expected = request.clone();
        match self
            .call(Request::PostProcessJobs(Box::new(request)))
            .await?
        {
            Reply::PostProcessJobs(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&expected) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn claim_post_process_job(
        &mut self,
        worker: &RegisteredWorker,
        request: ClaimPostProcessJobRequest,
    ) -> Result<DesktopEnvelope<PostProcessJobLease>, ClientError> {
        if !request.valid() || !self.post_process_registration_valid(worker, &request.proof) {
            return Err(bad_request());
        }
        let expected = request.clone();
        match self
            .call(Request::ClaimPostProcessJob(Box::new(request)))
            .await?
        {
            Reply::PostProcessClaimed(value)
                if self.desktop_envelope_valid(&value)
                    && value.data.claimed_for(&expected, worker) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn renew_post_process_job(
        &mut self,
        worker: &RegisteredWorker,
        request: RenewPostProcessJobRequest,
    ) -> Result<DesktopEnvelope<PostProcessJobLease>, ClientError> {
        if !request.valid() || !self.post_process_registration_valid(worker, &request.proof) {
            return Err(bad_request());
        }
        let expected = request.clone();
        match self
            .call(Request::RenewPostProcessJob(Box::new(request)))
            .await?
        {
            Reply::PostProcessRenewed(value)
                if self.desktop_envelope_valid(&value)
                    && value.data.renewed_for(&expected, worker) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn fail_post_process_job(
        &mut self,
        worker: &RegisteredWorker,
        request: FailPostProcessJobRequest,
    ) -> Result<DesktopEnvelope<PostProcessFailureReceipt>, ClientError> {
        if !request.valid() || !self.post_process_registration_valid(worker, &request.proof) {
            return Err(bad_request());
        }
        let expected = request.clone();
        match self
            .call(Request::FailPostProcessJob(Box::new(request)))
            .await?
        {
            Reply::PostProcessFailed(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&expected) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

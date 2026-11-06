//! Registered post-process control only. Native execution, output publication
//! and provider/network permission remain separate, unopened boundaries.
use crate::{CoreError, Repository};
use bazza_service_api::{desktop::DesktopEnvelope, postprocess as wire};
use bazza_store::service_data as data;
use serde::{de::DeserializeOwned, Serialize};

fn convert<T: Serialize, D: DeserializeOwned>(value: &T) -> Result<D, CoreError> {
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}

impl Repository {
    /// Bounded retention signal only. Every worker request still authenticates
    /// and checks the actual claim and source fences independently.
    pub fn has_live_post_process_claims(&self) -> Result<bool, CoreError> {
        Ok(self
            .store
            .service_has_live_post_process_claims(self.epoch)?)
    }

    pub fn post_process_jobs(
        &self,
        peer_pid: u32,
        request: wire::PostProcessJobsRequest,
    ) -> Result<DesktopEnvelope<wire::PostProcessJobPage>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process page").into());
        }
        let value: wire::PostProcessJobPage = convert(&self.store.service_post_process_jobs(
            peer_pid,
            self.epoch,
            &convert(&request)?,
        )?)?;
        if !value.valid_for(&request) {
            return Err(data::DataError::Invalid("post-process page reply").into());
        }
        self.desktop_envelope(value)
    }

    pub fn claim_post_process_job(
        &self,
        peer_pid: u32,
        request: wire::ClaimPostProcessJobRequest,
    ) -> Result<DesktopEnvelope<wire::PostProcessJobLease>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process claim").into());
        }
        let value: wire::PostProcessJobLease =
            convert(&self.store.service_claim_post_process_job(
                peer_pid,
                self.epoch,
                &convert(&request)?,
            )?)?;
        if !value.valid() {
            return Err(data::DataError::Invalid("post-process lease reply").into());
        }
        self.desktop_envelope(value)
    }

    pub fn renew_post_process_job(
        &self,
        peer_pid: u32,
        request: wire::RenewPostProcessJobRequest,
    ) -> Result<DesktopEnvelope<wire::PostProcessJobLease>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process renewal").into());
        }
        let value: wire::PostProcessJobLease =
            convert(&self.store.service_renew_post_process_job(
                peer_pid,
                self.epoch,
                &convert(&request)?,
            )?)?;
        if !value.valid() {
            return Err(data::DataError::Invalid("post-process lease reply").into());
        }
        self.desktop_envelope(value)
    }

    pub fn fail_post_process_job(
        &self,
        peer_pid: u32,
        request: wire::FailPostProcessJobRequest,
    ) -> Result<DesktopEnvelope<wire::PostProcessFailureReceipt>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("post-process failure").into());
        }
        let value: wire::PostProcessFailureReceipt = convert(
            &self
                .store
                .service_fail_post_process_job(peer_pid, self.epoch, &convert(&request)?)?,
        )?;
        if !value.valid_for(&request) {
            return Err(data::DataError::Invalid("post-process failure reply").into());
        }
        self.desktop_envelope(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_service_api::worker::{
        RegisterWorkerRequest, WorkerCapability, WorkerProof, WorkerRegistration,
    };
    use bazza_service_test_support::create_v8;

    #[test]
    fn post_process_pages_require_current_registered_capability_before_reading() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temporary.path().join("library")).unwrap();
        let repo =
            Repository::open_fixture(&fixture.root, crate::MigrationPolicy::Upgrade).unwrap();
        let registration = WorkerRegistration::new(vec![WorkerCapability::PostProcess]);
        let worker = repo
            .register_worker(
                42,
                RegisterWorkerRequest {
                    registration: registration.clone(),
                    ttl_seconds: 60,
                },
            )
            .unwrap();
        let proof = WorkerProof {
            session_id: worker.session_id,
            secret: registration.secret,
            library_epoch: worker.library_epoch,
        };
        let media_before = std::fs::read_dir(fixture.root.join("recordings"))
            .unwrap()
            .count();
        let request = wire::PostProcessJobsRequest::new(proof);
        let page = repo.post_process_jobs(42, request.clone()).unwrap();
        assert_eq!(page.library_id, repo.library_id);
        assert_eq!(page.library_epoch, repo.epoch);
        assert!(page.data.items.is_empty());
        assert_eq!(
            std::fs::read_dir(fixture.root.join("recordings"))
                .unwrap()
                .count(),
            media_before
        );
        assert!(matches!(
            repo.post_process_jobs(43, request.clone()),
            Err(CoreError::Data(data::DataError::Forbidden))
        ));
        let mut stale = request;
        stale.proof.library_epoch = uuid::Uuid::new_v4();
        assert!(matches!(
            repo.post_process_jobs(42, stale),
            Err(CoreError::Data(data::DataError::Forbidden))
        ));
    }
}

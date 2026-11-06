//! Explicit private media grants and an owner-bearing convenience boundary.
//! A grant alone is metadata; retain its owner (or an inherited gate file) while
//! consuming media. No callback, native executor, renewal or retry is started.
use crate::{ClientError, ServiceClient};
use bazza_capture_handoff::{
    media_read::{MediaReadIdentity, MediaReadOwner},
    HandoffError,
};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::{post_process_worker_valid, PostProcessJobLease},
    postprocess_media::{PostProcessMediaGrant, PostProcessMediaKind, PostProcessMediaRequest},
    postprocess_video::{
        video_staging_name, PostProcessVideoAction, PostProcessVideoFailure,
        PostProcessVideoReceipt, PostProcessVideoRequest,
    },
    rpc::{ErrorCode, Reply, Request, RpcError},
    worker::{CaptureSelection, RegisteredWorker, WorkerProof, WorkerSecret},
};
use bazza_service_transport::session_endpoint_name;
use std::{
    fs::File,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// The owner nonce is created locally after validation, never selected here.
#[derive(Clone, Debug)]
pub struct WorkerMediaInputRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub expected_provider: CaptureSelection,
}

impl WorkerMediaInputRequest {
    fn with_nonce(&self, owner_nonce: Uuid) -> PostProcessMediaRequest {
        PostProcessMediaRequest {
            proof: self.proof.clone(),
            lease: self.lease.clone(),
            fence: self.fence.clone(),
            owner_nonce,
            expected_provider: self.expected_provider.clone(),
        }
    }
}

/// Retains the proof server and shared gate for the borrowed media grant.
/// A cloned read file can be passed explicitly to a child before this is dropped.
pub struct OwnedPostProcessMedia {
    envelope: DesktopEnvelope<PostProcessMediaGrant>,
    owner: MediaReadOwner,
}

impl OwnedPostProcessMedia {
    pub fn grant(&self) -> &PostProcessMediaGrant {
        &self.envelope.data
    }

    pub fn envelope(&self) -> &DesktopEnvelope<PostProcessMediaGrant> {
        &self.envelope
    }

    pub fn try_clone_file(&self) -> Result<File, ClientError> {
        self.owner.try_clone_file().map_err(|_| unavailable())
    }

    /// Give the trusted muxer its fixed same-directory output name. Keep this
    /// owner through child exit and publication; the returned path is staging.
    pub fn video_staging_path(&self) -> Result<PathBuf, ClientError> {
        let grant = self.grant();
        if grant.screen_writer.is_none() {
            return Err(bad_request());
        }
        let audio = grant
            .artifacts
            .iter()
            .find(|file| file.kind == PostProcessMediaKind::AudioMix)
            .ok_or_else(bad_request)?;
        let directory = Path::new(&audio.private_path)
            .parent()
            .ok_or_else(bad_request)?;
        Ok(directory.join(video_staging_name(grant.claim_id, self.owner.nonce())))
    }

    /// A durable previous publication attempt can be recovered directly. Only
    /// the publication RPC verifies that its recorded bytes are still present.
    pub fn has_video_publication_candidate(&self) -> Result<bool, ClientError> {
        Ok(self.grant().video_publication_candidate)
    }
}

impl std::fmt::Debug for OwnedPostProcessMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedPostProcessMedia")
            .field("library_id", &self.envelope.library_id)
            .field("claim_id", &self.grant().claim_id)
            .field("meeting_id", &self.grant().meeting_id)
            .field("expires_at", &self.grant().expires_at)
            .field("artifact_count", &self.grant().artifacts.len())
            .finish_non_exhaustive()
    }
}

fn bad_request() -> ClientError {
    RpcError::new(ErrorCode::InvalidArgument).into()
}

fn unavailable() -> ClientError {
    RpcError::new(ErrorCode::BackendUnavailable).into()
}

fn ownership_error(error: HandoffError) -> ClientError {
    RpcError::new(if matches!(error, HandoffError::Busy) {
        ErrorCode::ResourceBusy
    } else {
        ErrorCode::BackendUnavailable
    })
    .into()
}

fn ordinary_directory(path: &Path) -> Result<(), ClientError> {
    let metadata = path.symlink_metadata().map_err(|_| unavailable())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(bad_request());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(bad_request());
        }
    }
    Ok(())
}

fn selected_root(root: &Path, epoch: Uuid, endpoint: &str) -> Result<PathBuf, ClientError> {
    if !root.is_absolute() {
        return Err(bad_request());
    }
    ordinary_directory(root)?;
    let canonical = root.canonicalize().map_err(|_| unavailable())?;
    ordinary_directory(&canonical)?;
    if canonical.to_str().is_none()
        || session_endpoint_name(&canonical, epoch.as_bytes()).map_err(|_| unavailable())?
            != endpoint
    {
        return Err(bad_request());
    }
    Ok(canonical)
}

impl ServiceClient {
    pub async fn publish_post_process_video(
        &mut self,
        worker: &RegisteredWorker,
        media: &OwnedPostProcessMedia,
        proof: &WorkerProof,
        lease: &PostProcessJobLease,
        fence: &WorkerSecret,
    ) -> Result<DesktopEnvelope<PostProcessVideoReceipt>, ClientError> {
        self.post_process_video(
            worker,
            media,
            proof,
            lease,
            fence,
            PostProcessVideoAction::Publish {},
        )
        .await
    }

    pub async fn record_post_process_video_failure(
        &mut self,
        worker: &RegisteredWorker,
        media: &OwnedPostProcessMedia,
        proof: &WorkerProof,
        lease: &PostProcessJobLease,
        fence: &WorkerSecret,
        reason: PostProcessVideoFailure,
    ) -> Result<DesktopEnvelope<PostProcessVideoReceipt>, ClientError> {
        self.post_process_video(
            worker,
            media,
            proof,
            lease,
            fence,
            PostProcessVideoAction::Unavailable { reason },
        )
        .await
    }

    async fn post_process_video(
        &mut self,
        worker: &RegisteredWorker,
        media: &OwnedPostProcessMedia,
        proof: &WorkerProof,
        lease: &PostProcessJobLease,
        fence: &WorkerSecret,
        result: PostProcessVideoAction,
    ) -> Result<DesktopEnvelope<PostProcessVideoReceipt>, ClientError> {
        let grant = media.grant();
        let request = PostProcessVideoRequest {
            media: PostProcessMediaRequest {
                proof: proof.clone(),
                lease: lease.clone(),
                fence: fence.clone(),
                owner_nonce: media.owner.nonce(),
                expected_provider: grant.provider.clone(),
            },
            result,
        };
        if grant.screen_writer.is_none()
            || !self.desktop_envelope_valid(media.envelope())
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, proof, self.descriptor.library_epoch)
            || !request.media.valid()
            || grant.claim_id != lease.claim_id
            || grant.job_id != lease.job_id
            || grant.meeting_id != lease.meeting_id
            || grant.input != lease.input
        {
            return Err(bad_request());
        }
        match self
            .call(Request::PostProcessVideo(Box::new(request.clone())))
            .await?
        {
            Reply::PostProcessVideo(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&request) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    /// Low-level call. Its caller owns the matching MediaReadOwner separately.
    pub async fn post_process_media(
        &mut self,
        worker: &RegisteredWorker,
        request: PostProcessMediaRequest,
    ) -> Result<DesktopEnvelope<PostProcessMediaGrant>, ClientError> {
        if !request.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &request.proof, self.descriptor.library_epoch)
        {
            return Err(bad_request());
        }
        let original = request.clone();
        match self
            .call(Request::PostProcessMedia(Box::new(request)))
            .await?
        {
            Reply::PostProcessMedia(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn acquire_post_process_media(
        &mut self,
        worker: &RegisteredWorker,
        root: &Path,
        input: WorkerMediaInputRequest,
    ) -> Result<OwnedPostProcessMedia, ClientError> {
        // This non-wire nonce only reuses the complete DTO shape validator
        // before any filesystem access. The owner supplies the real nonce.
        let shape = input.with_nonce(Uuid::from_u128(1));
        if !shape.valid()
            || worker.peer_pid != std::process::id()
            || !post_process_worker_valid(worker, &input.proof, self.descriptor.library_epoch)
        {
            return Err(bad_request());
        }
        let identity = MediaReadIdentity {
            library_id: self.descriptor.library_id,
            library_epoch: self.descriptor.library_epoch,
            worker_session_id: input.lease.worker_session_id,
            claim_id: input.lease.claim_id,
            job_id: input.lease.job_id.clone(),
            meeting_id: input.lease.meeting_id,
            capture_id: input.lease.input.capture_id,
            capture_generation: input.lease.input.capture_generation,
            checkpoint_revision: input.lease.input.checkpoint_revision.clone(),
            lifecycle_revision: input.lease.input.lifecycle_revision.clone(),
            transcript_revision: input.lease.input.transcript_revision.clone(),
            source_id: input.lease.input.source_id.clone(),
        };
        let root = root.to_path_buf();
        let epoch = self.descriptor.library_epoch;
        let endpoint = self.descriptor.endpoint.clone();
        let (canonical, owner) = tokio::task::spawn_blocking(move || {
            let canonical = selected_root(&root, epoch, &endpoint)?;
            let owner = MediaReadOwner::acquire(&canonical, &identity).map_err(ownership_error)?;
            Ok::<_, ClientError>((canonical, owner))
        })
        .await
        .map_err(|_| unavailable())??;
        let request = input.with_nonce(owner.nonce());
        let envelope = self.post_process_media(worker, request).await?;
        let directory = canonical
            .join("recordings")
            .join(input.lease.meeting_id.to_string());
        if envelope.data.artifacts.iter().any(|artifact| {
            directory.join(artifact.kind.file_name()).to_str()
                != Some(artifact.private_path.as_str())
        }) {
            return self.invalid_worker_reply();
        }
        Ok(OwnedPostProcessMedia { envelope, owner })
    }
}

#[cfg(test)]
#[path = "postprocess_media_tests.rs"]
mod tests;

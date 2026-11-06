//! Typed local service client. No SQLite opener, GUI or inference dependency.
mod atomic_publish;
pub mod broker;
pub mod chats;
pub mod configuration;
pub mod content;
pub mod desktop;
pub mod external_read;
pub mod external_read_credentials;
mod fixture;
mod fixture_read;
pub mod installed;
pub mod library;
pub mod postprocess;
pub mod postprocess_cleanup;
mod postprocess_complete;
pub mod postprocess_input;
pub mod postprocess_media;
mod postprocess_ready;
pub mod postprocess_result;
mod postprocess_source;
mod postprocess_speakers;
mod postprocess_summary;
pub mod repository;
pub mod soniox_permission;
pub mod templates;
pub use bazza_service_api as api;
pub use fixture::{validate_fixture_root, validate_managed_root};
pub use repository::RepositoryClient;

#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod desktop_tests;
#[cfg(test)]
mod library_tests;
#[cfg(test)]
mod postprocess_tests;
#[cfg(test)]
mod tests;

use bazza_service_api::rpc::*;
use bazza_service_api::worker::*;
use bazza_service_transport::{
    connect, read_frame, session_endpoint_name, write_frame, PeerStream,
};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

pub const ENDPOINT_FILE: &str = ".bazza-service-endpoint.json";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("local service transport unavailable: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid service discovery")]
    Discovery,
    #[error("local service call deadline exceeded")]
    Timeout,
    #[error("invalid service reply correlation or kind")]
    Protocol,
    #[error("{0}")]
    Remote(#[from] RpcError),
    #[error("local user-session broker is not configured or does not match")]
    BrokerUnavailable,
    #[error("the selected root is not an explicit synthetic fixture")]
    FixtureUnavailable,
    #[error("the selected root is not an initialized managed library")]
    ManagedLibraryUnavailable,
}

pub fn read_endpoint(root: &Path) -> Result<EndpointDescriptor, ClientError> {
    let path = root.join(ENDPOINT_FILE);
    regular_file(&path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(ClientError::Discovery);
    }
    let descriptor: EndpointDescriptor =
        serde_json::from_slice(&bytes).map_err(|_| ClientError::Discovery)?;
    if descriptor.discovery_version != DISCOVERY_VERSION
        || descriptor.process_id == 0
        || descriptor.library_id.is_nil()
        || descriptor.library_epoch.is_nil()
        || descriptor.endpoint != session_endpoint_name(root, descriptor.library_epoch.as_bytes())?
    {
        return Err(ClientError::Discovery);
    }
    Ok(descriptor)
}

pub fn publish_endpoint(root: &Path, descriptor: &EndpointDescriptor) -> Result<(), ClientError> {
    let bytes = serde_json::to_vec(descriptor).map_err(|_| ClientError::Discovery)?;
    if bytes.len() > 8192
        || descriptor.endpoint != session_endpoint_name(root, descriptor.library_epoch.as_bytes())?
    {
        return Err(ClientError::Discovery);
    }
    let path = root.join(ENDPOINT_FILE);
    if path.symlink_metadata().is_ok() {
        regular_file(&path)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.as_file().sync_all()?;
    atomic_publish::persist_atomic(file, &path)?;
    Ok(())
}

fn regular_file(path: &Path) -> Result<(), ClientError> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ClientError::Discovery);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(ClientError::Discovery);
        }
    }
    Ok(())
}

pub struct ServiceClient {
    stream: Option<PeerStream>,
    descriptor: EndpointDescriptor,
    timeout: Duration,
}

impl ServiceClient {
    pub async fn connect(root: &Path) -> Result<Self, ClientError> {
        Self::connect_with(root, 1, STORAGE_SCHEMA_MAX, build_id()).await
    }

    pub async fn connect_with(
        root: &Path,
        schema_min: i64,
        schema_max: i64,
        expected_build: &str,
    ) -> Result<Self, ClientError> {
        Self::connect_with_mode(root, schema_min, schema_max, expected_build, None).await
    }

    pub async fn connect_managed(root: &Path) -> Result<Self, ClientError> {
        let (root, library_id) = validate_managed_root(root)?;
        Self::connect_with_mode(&root, STORAGE_SCHEMA_MAX, STORAGE_SCHEMA_MAX, build_id(), Some(library_id)).await
    }

    /// Open the pinned managed service, starting its registered broker when absent.
    pub async fn connect_or_start_managed(root: &Path) -> Result<Self, ClientError> {
        let (root, library_id) = validate_managed_root(root)?;
        Self::connect_or_start_managed_pinned(&root, library_id).await
    }

    async fn connect_or_start_managed_pinned(
        root: &Path,
        library_id: uuid::Uuid,
    ) -> Result<Self, ClientError> {
        let (selected_root, selected_id) = validate_managed_root(root)?;
        if selected_root != root || selected_id != library_id {
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        broker::connect_or_start_managed(root, library_id, || {
            Self::connect_with_mode(
                root,
                STORAGE_SCHEMA_MAX,
                STORAGE_SCHEMA_MAX,
                build_id(),
                Some(library_id),
            )
        })
        .await
    }

    async fn connect_with_mode(
        root: &Path,
        schema_min: i64,
        schema_max: i64,
        expected_build: &str,
        managed_id: Option<uuid::Uuid>,
    ) -> Result<Self, ClientError> {
        let descriptor = read_endpoint(root)?;
        if descriptor.api.major != API_MAJOR
            || descriptor.api.minor > API_MINOR
            || descriptor.build_id != expected_build
            || managed_id.is_some_and(|id| id != descriptor.library_id)
        {
            return Err(RpcError::new(ErrorCode::UpgradeRequired).into());
        }
        let stream = tokio::time::timeout(
            Duration::from_millis(MAX_CALL_MS),
            connect(&descriptor.endpoint, descriptor.process_id),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        let mut client = Self {
            stream: Some(stream),
            descriptor,
            timeout: Duration::from_millis(MAX_CALL_MS),
        };
        let hello = ClientHello {
            api: ApiVersion::CURRENT,
            schema_min,
            schema_max,
            build_id: expected_build.into(),
            library_id: client.descriptor.library_id,
        };
        match client.call(Request::Hello(hello.clone())).await? {
            Reply::Hello(status) if !hello.compatible_with_mode(&status, managed_id.is_none()) => {
                Err(RpcError::new(ErrorCode::UpgradeRequired).into())
            }
            Reply::Hello(status)
                if status.library_id == client.descriptor.library_id
                    && status.library_epoch == client.descriptor.library_epoch
                    && status.process_id == client.descriptor.process_id
                    && status.build_id == expected_build =>
            {
                Ok(client)
            }
            _ => Err(ClientError::Protocol),
        }
    }

    async fn call(&mut self, request: Request) -> Result<Reply, ClientError> {
        let timeout = if matches!(&request, Request::PostProcessVideo(_)) {
            Duration::from_millis(bazza_service_api::postprocess_video::MAX_POST_PROCESS_VIDEO_CALL_MS)
        } else {
            self.timeout
        };
        let expected_kind = match &request {
            Request::Hello(_) => 0,
            Request::GetStatus => 1,
            Request::GetMeeting(_) => 2,
            Request::Shutdown => 3,
            Request::RegisterWorker(_) | Request::RenewWorker(_) => 4,
            Request::PrepareCapture(_) | Request::ReattachCapture(_) => 5,
            Request::FinalizeCapture(_) => 6,
            Request::DesktopBootstrap => 7,
            Request::DesktopPreferences => 8,
            Request::DesktopMeetings(_) => 9,
            Request::DesktopMeeting(_) => 10,
            Request::DesktopMeetingDecorations(_) => 100,
            Request::DesktopChanges(_) => 11,
            Request::UpdateDesktopPreferences(_)
            | Request::RenameDesktopMeeting(_)
            | Request::DeleteDesktopMeeting(_)
            | Request::CommitDesktopAgentSummary(_) => 12,
            Request::DesktopTranscript(_) => 13,
            Request::DesktopSummary(_) => 14,
            Request::BeginCapture(_) => 15,
            Request::AbortCapture(_) => 16,
            Request::DesktopLibrary(_) => 17,
            Request::NativeConfiguration => 18,
            Request::UpdateNativeConfiguration(_) => 19,
            Request::PostProcessJobs(_) => 20,
            Request::ClaimPostProcessJob(_) => 21,
            Request::RenewPostProcessJob(_) => 22,
            Request::FailPostProcessJob(_) => 23,
            Request::DesktopTemplates(_) => 24,
            Request::PostProcessTranscript(_) => 25,
            Request::CommitPostProcessAutoTitle(_) => 26,
            Request::PostProcessMedia(_) => 27,
            Request::PostProcessCleanup(_) => 28,
            Request::DesktopChats(_) => 29,
            Request::FixtureRead(_) => 30,
            Request::PostProcessSource(_) => 31,
            Request::PostProcessSpeakers(_) => 32,
            Request::MarkPostProcessReady(_) => 33,
            Request::PostProcessSummary(_) => 34,
            Request::CompleteFixturePostProcess(_) => 35,
            Request::IssueExternalReadGrant(_) => 36,
            Request::RevokeExternalReadGrant(_) => 37,
            Request::ListExternalReadGrants(_) => 38,
            Request::SonioxPermission(_) => 39,
            Request::SonioxKey(_) => 101,
            Request::PostProcessVideo(_) => 40,
            Request::DesktopTranscriptVersion(_) => 41,
        };
        let request_id = uuid::Uuid::new_v4();
        let envelope = RequestEnvelope {
            request_id,
            library_epoch: self.descriptor.library_epoch,
            deadline_unix_ms: unix_ms().saturating_add(timeout.as_millis() as u64),
            request,
        };
        // The in-flight future owns the stream. Dropping it from an outer
        // timeout/select also closes a partial frame and leaves this client poisoned.
        let mut stream = self.stream.take().ok_or(ClientError::Protocol)?;
        let result = tokio::time::timeout(timeout, async {
            write_frame(&mut stream, &envelope).await?;
            read_frame(&mut stream).await
        })
        .await;
        let reply: ReplyEnvelope = match result {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                self.stream.take();
                return Err(error.into());
            }
            Err(_) => {
                self.stream.take();
                return Err(ClientError::Timeout);
            }
        };
        if reply.request_id != request_id {
            self.stream.take();
            return Err(ClientError::Protocol);
        }
        if reply.library_epoch != self.descriptor.library_epoch {
            self.stream.take();
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        if !matches!(&reply.reply, Reply::Error(_))
            && !matches!(
                (expected_kind, &reply.reply),
                (0, Reply::Hello(_))
                    | (1, Reply::Status(_))
                    | (2, Reply::Meeting(_))
                    | (3, Reply::ShutdownAccepted)
                    | (4, Reply::RegisteredWorker(_))
                    | (5, Reply::CaptureLease(_))
                    | (6, Reply::WorkerMutationOutcome(_))
                    | (7, Reply::DesktopBootstrap(_))
                    | (8, Reply::DesktopPreferences(_))
                    | (9, Reply::DesktopMeetings(_))
                    | (10, Reply::DesktopMeeting(_))
                    | (100, Reply::DesktopMeetingDecorations(_))
                    | (11, Reply::DesktopChanges(_))
                    | (12, Reply::DesktopMutation(_))
                    | (13, Reply::DesktopTranscript(_))
                    | (14, Reply::DesktopSummary(_))
                    | (15, Reply::CaptureBegun(_))
                    | (16, Reply::CaptureAborted(_))
                    | (17, Reply::DesktopLibrary(_))
                    | (18, Reply::NativeConfiguration(_))
                    | (19, Reply::NativeConfigurationUpdated(_))
                    | (20, Reply::PostProcessJobs(_))
                    | (21, Reply::PostProcessClaimed(_))
                    | (22, Reply::PostProcessRenewed(_))
                    | (23, Reply::PostProcessFailed(_))
                    | (24, Reply::DesktopTemplates(_))
                    | (25, Reply::PostProcessTranscript(_))
                    | (26, Reply::PostProcessAutoTitle(_))
                    | (27, Reply::PostProcessMedia(_))
                    | (28, Reply::PostProcessCleanup(_))
                    | (29, Reply::DesktopChats(_))
                    | (30, Reply::FixtureRead(_))
                    | (31, Reply::PostProcessSource(_))
                    | (32, Reply::PostProcessSpeakers(_))
                    | (33, Reply::PostProcessReady(_))
                    | (34, Reply::PostProcessSummary(_))
                    | (35, Reply::FixturePostProcessComplete(_))
                    | (36, Reply::ExternalReadGrantIssued(_))
                    | (37, Reply::ExternalReadGrantRevoked(_))
                    | (38, Reply::ExternalReadGrantPage(_))
                    | (39, Reply::SonioxPermission(_))
                    | (101, Reply::SonioxKeyPresent(_))
                    | (40, Reply::PostProcessVideo(_))
                    | (41, Reply::DesktopTranscriptVersion(_))
            )
        {
            return Err(ClientError::Protocol);
        }
        self.stream = Some(stream);
        match reply.reply {
            Reply::Error(error) => Err(error.into()),
            reply => Ok(reply),
        }
    }

    pub async fn status(&mut self) -> Result<ServiceStatus, ClientError> {
        match self.call(Request::GetStatus).await? {
            Reply::Status(status) => Ok(status),
            _ => Err(ClientError::Protocol),
        }
    }
    pub async fn meeting(
        &mut self,
        id: uuid::Uuid,
    ) -> Result<Option<MeetingMetadata>, ClientError> {
        match self
            .call(Request::GetMeeting(MeetingRequest { meeting_id: id }))
            .await?
        {
            Reply::Meeting(meeting) => Ok(meeting),
            _ => Err(ClientError::Protocol),
        }
    }
    pub async fn shutdown(&mut self) -> Result<(), ClientError> {
        match self.call(Request::Shutdown).await? {
            Reply::ShutdownAccepted => Ok(()),
            _ => Err(ClientError::Protocol),
        }
    }

    fn invalid_worker_reply<T>(&mut self) -> Result<T, ClientError> {
        self.stream.take();
        Err(ClientError::Protocol)
    }

    /// Caller retains this registration's secret across retries/reconnects.
    /// The service binds it to the authenticated peer, never a nominated PID.
    pub async fn register_worker(
        &mut self,
        request: RegisterWorkerRequest,
    ) -> Result<RegisteredWorker, ClientError> {
        let session_id = request.registration.session_id;
        let mut capabilities = request.registration.capabilities.clone();
        capabilities.sort();
        match self.call(Request::RegisterWorker(request)).await? {
            Reply::RegisteredWorker(worker)
                if worker.session_id == session_id
                    && worker.peer_pid == std::process::id()
                    && worker.library_epoch == self.descriptor.library_epoch
                    && worker.capabilities == capabilities =>
            {
                Ok(worker)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn renew_worker(
        &mut self,
        request: RenewWorkerRequest,
    ) -> Result<RegisteredWorker, ClientError> {
        let session_id = request.proof.session_id;
        match self.call(Request::RenewWorker(request)).await? {
            Reply::RegisteredWorker(worker)
                if worker.session_id == session_id
                    && worker.peer_pid == std::process::id()
                    && worker.library_epoch == self.descriptor.library_epoch =>
            {
                Ok(worker)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn prepare_capture(
        &mut self,
        request: PrepareCaptureRequest,
    ) -> Result<CaptureLease, ClientError> {
        let capture_id = request.start.capture_id;
        let meeting_id = request.start.meeting_id;
        let session_id = request.proof.session_id;
        match self.call(Request::PrepareCapture(request)).await? {
            Reply::CaptureLease(lease)
                if lease.capture_id == capture_id
                    && lease.meeting_id == meeting_id
                    && lease.worker_session_id == session_id
                    && !lease.generation.is_nil()
                    && lease.library_epoch == self.descriptor.library_epoch =>
            {
                Ok(lease)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    /// The worker must hold its ArtifactGuard through this call and may start
    /// raw/journal writers only after ACK. There is no automatic retry.
    pub async fn begin_capture(
        &mut self,
        request: BeginCaptureRequest,
    ) -> Result<CaptureLease, ClientError> {
        if request.proof.session_id != request.lease.worker_session_id
            || request.proof.library_epoch != request.lease.library_epoch
            || request.lease.library_epoch != self.descriptor.library_epoch
            || [
                request.lease.capture_id,
                request.lease.meeting_id,
                request.lease.generation,
                request.proof.session_id,
            ]
            .iter()
            .any(uuid::Uuid::is_nil)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let expected = request.lease.clone();
        match self.call(Request::BeginCapture(request)).await? {
            Reply::CaptureBegun(lease) if lease == expected => Ok(lease),
            _ => self.invalid_worker_reply(),
        }
    }
    /// Non-destructive compensation for the same unused reservation. Original
    /// proof/lease may replay across service restart; active capture cannot abort.
    pub async fn abort_capture(
        &mut self,
        request: AbortCaptureRequest,
    ) -> Result<WorkerMutationOutcome, ClientError> {
        if request.proof.session_id != request.lease.worker_session_id
            || request.proof.library_epoch != request.lease.library_epoch
            || [
                request.lease.capture_id,
                request.lease.meeting_id,
                request.lease.generation,
                request.lease.library_epoch,
                request.proof.session_id,
            ]
            .iter()
            .any(uuid::Uuid::is_nil)
        {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let meeting_id = request.lease.meeting_id.to_string();
        match self.call(Request::AbortCapture(request)).await? {
            Reply::CaptureAborted(outcome)
                if outcome.entity_id == meeting_id
                    && bazza_service_api::desktop::desktop_revision(&outcome.revision)
                    && outcome.change_sequence > 0
                    && outcome.job_id.is_none() =>
            {
                Ok(outcome)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    /// Reconnect with the old proof after a service restart; the returned lease
    /// belongs to the new service epoch. The caller then updates its proof.
    pub async fn reattach_capture(
        &mut self,
        request: ReattachCaptureRequest,
    ) -> Result<CaptureLease, ClientError> {
        let capture_id = request.capture_id;
        let session_id = request.proof.session_id;
        match self.call(Request::ReattachCapture(request)).await? {
            Reply::CaptureLease(lease)
                if lease.capture_id == capture_id
                    && !lease.meeting_id.is_nil()
                    && lease.worker_session_id == session_id
                    && !lease.generation.is_nil()
                    && lease.library_epoch == self.descriptor.library_epoch =>
            {
                Ok(lease)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    /// Finalize carries identities/fence only. The core verifies the durable
    /// terminal and confirmed-STT journal after the artifact owner has released.
    pub async fn finalize_capture(
        &mut self,
        request: FinalizeCaptureRequest,
    ) -> Result<WorkerMutationOutcome, ClientError> {
        let meeting_id = request.lease.meeting_id;
        match self.call(Request::FinalizeCapture(request)).await? {
            Reply::WorkerMutationOutcome(outcome)
                if outcome.entity_id == meeting_id.to_string()
                    // Finalization advances lifecycle revision. It is not the
                    // old capture generation carried by the lease.
                    && !outcome.revision.is_empty()
                    && outcome.revision.len()<=128
                    && outcome.revision.bytes().all(|c|c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
                    && outcome.change_sequence > 0
                    // A failed capture keeps its evidence without scheduling work.
                    && outcome.job_id.as_deref().is_none_or(|id| {
                        uuid::Uuid::parse_str(id).is_ok_and(|id| !id.is_nil())
                    }) =>
            {
                Ok(outcome)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    /// Broker launch is explicit and never falls back to a DB opener or a child.
    pub async fn connect_or_start_fixture(root: &Path) -> Result<Self, ClientError> {
        match Self::connect(root).await {
            Ok(client) => return Ok(client),
            Err(error @ ClientError::Remote(_)) => return Err(error),
            Err(_) => {}
        }
        broker::start_fixture(root).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(MAX_CALL_MS);
        loop {
            match Self::connect(root).await {
                Ok(client) => return Ok(client),
                Err(error @ ClientError::Remote(_)) => return Err(error),
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(30)).await
                }
                Err(_) => return Err(ClientError::Timeout),
            }
        }
    }
}

//! Experimental stage-1 host. Production library and GUI cutover are later gates.
mod external_read_endpoint;
pub mod external_read_gate;
use anyhow::Context;
use bazza_core::data::DataError;
use bazza_core::{CoreError, MigrationPolicy, Repository};
use bazza_service_api::external_read::{
    ExternalReadGrantIssued, ExternalReadGrantMetadata, ExternalReadGrantPage,
    ExternalReadGrantPermission, ExternalReadGrantRevoked,
};
use bazza_service_api::rpc::*;
use bazza_service_api::worker::WorkerSecret;
use bazza_service_client::{publish_endpoint, read_endpoint, ENDPOINT_FILE};
use bazza_service_transport::{
    external_session_endpoint_name, read_frame, session_endpoint_name, write_frame, Listener,
    PeerIdentity, PeerStream,
};
use external_read_gate::{ExternalReadError, ExternalReadGate};
use std::{
    io,
    path::Path,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::Notify, task::JoinSet, time::Instant};

struct Activity {
    clients: AtomicUsize,
    last_activity: AtomicU64,
}
struct ClientLease(Arc<Activity>);
impl Drop for ClientLease {
    fn drop(&mut self) {
        self.0.last_activity.store(unix_ms(), Ordering::SeqCst);
        self.0.clients.fetch_sub(1, Ordering::SeqCst);
    }
}

enum ClientRole {
    Application,
    ExternalRead,
}

async fn accept_client(
    private: &mut Listener,
    external: Option<&mut Listener>,
) -> io::Result<(PeerStream, PeerIdentity, ClientRole)> {
    if let Some(external) = external {
        tokio::select! {
            client = private.accept() => client.map(|(stream, peer)| (stream, peer, ClientRole::Application)),
            client = external.accept() => client.map(|(stream, peer)| (stream, peer, ClientRole::ExternalRead)),
        }
    } else {
        private
            .accept()
            .await
            .map(|(stream, peer)| (stream, peer, ClientRole::Application))
    }
}

/// Owns a synthetic Store for the whole listener/task lifetime. There is no GUI
/// fallback, inference, capture, migrations on start, or public external grant.
pub async fn serve_fixture(root: &Path, idle_timeout: Duration) -> anyhow::Result<()> {
    serve_library_mode(root, idle_timeout, false, false).await
}

/// Opt in to the external read listener on a marked disposable library.
pub async fn serve_fixture_with_external_read(
    root: &Path,
    idle_timeout: Duration,
) -> anyhow::Result<()> {
    if !root.is_absolute() {
        anyhow::bail!("ABSOLUTE_FIXTURE_REQUIRED");
    }
    serve_library_mode(root, idle_timeout, true, false).await
}

pub async fn serve_managed(
    root: &Path,
    idle_timeout: Duration,
    external_read: bool,
) -> anyhow::Result<()> {
    if !root.is_absolute() {
        anyhow::bail!("ABSOLUTE_LIBRARY_REQUIRED");
    }
    serve_library_mode(root, idle_timeout, external_read, true).await
}

async fn serve_library_mode(
    root: &Path,
    idle_timeout: Duration,
    external_read: bool,
    managed: bool,
) -> anyhow::Result<()> {
    if idle_timeout < Duration::from_millis(100) || idle_timeout > Duration::from_secs(60) {
        anyhow::bail!("INVALID_IDLE_TIMEOUT");
    }
    let repository = Arc::new(if managed {
        Repository::open_managed(root)?
    } else {
        Repository::open_fixture(root, MigrationPolicy::RequireCurrent)?
    });
    {
        let repository = repository.clone();
        tokio::task::spawn_blocking(move || {
            repository.expire_template_uploads()?;
            repository.expire_post_process_cleanup_uploads()?;
            repository.reconcile_capture_reservations()?;
            repository.recover_capture_handoffs()
        })
        .await??;
    }
    let root = root.canonicalize()?;
    let status = repository.status();
    let endpoint = session_endpoint_name(&root, status.library_epoch.as_bytes())?;
    let mut listener = Listener::bind(&endpoint)?;
    let (mut external_listener, external_gate) = if external_read {
        let endpoint = external_session_endpoint_name(&root, status.library_epoch.as_bytes())?;
        (
            Some(Listener::bind(&endpoint)?),
            Some(Arc::new(ExternalReadGate::new(repository.clone()))),
        )
    } else {
        (None, None)
    };
    let descriptor = EndpointDescriptor {
        discovery_version: DISCOVERY_VERSION,
        api: status.api,
        process_id: status.process_id,
        library_id: status.library_id,
        library_epoch: status.library_epoch,
        build_id: status.build_id.clone(),
        endpoint,
    };
    publish_endpoint(&root, &descriptor).context("publish service discovery")?;
    let shutdown = Arc::new(Notify::new());
    let activity = Arc::new(Activity {
        clients: AtomicUsize::new(0),
        last_activity: AtomicU64::new(unix_ms()),
    });
    let monitor_stop = Arc::new(Notify::new());
    let monitor = {
        let repository = repository.clone();
        let stop = monitor_stop.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _=stop.notified()=>break,
                    _=timer.tick()=>{
                        let repository=repository.clone();
                        let _=tokio::task::spawn_blocking(move||{
                            // Expiry removes only private staged upload bytes.
                            // Keep capture recovery independent of cleanup errors.
                            let _=repository.expire_template_uploads();
                            let _=repository.expire_post_process_cleanup_uploads();
                            repository.recover_capture_handoffs()
                        }).await;
                    }
                }
            }
        })
    };
    let purge_stop = Arc::new(Notify::new());
    let purge_sweep = {
        let repository = repository.clone();
        let stop = purge_stop.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(500));
            let mut after: Option<String> = None;
            loop {
                tokio::select! {
                    _ = stop.notified() => break,
                    _ = timer.tick() => {
                        let repository = repository.clone();
                        let cursor = after.take();
                        after = tokio::task::spawn_blocking(move || {
                            let next = repository
                                .next_desktop_meeting_purge(cursor.as_deref())
                                .ok()
                                .flatten();
                            if let Some(job) = next.as_deref() {
                                let _ = repository.data().run_purge(job);
                            }
                            next
                        }).await.unwrap_or_default();
                    }
                }
            }
        })
    };
    let mut tasks = JoinSet::new();
    let mut timer = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = timer.tick() => {
                if activity.clients.load(Ordering::SeqCst) == 0
                    && unix_ms().saturating_sub(activity.last_activity.load(Ordering::SeqCst)) >= idle_timeout.as_millis() as u64 {
                    // The periodic sample may predate a just-prepared worker.
                    // Recheck actual artifact ownership at the idle boundary.
                    let repository=repository.clone();
                    match tokio::task::spawn_blocking(move||{
                        let report=repository.recover_capture_handoffs()?;
                        let processing=repository.has_live_post_process_claims()?;
                        Ok::<_,CoreError>((report,processing))
                    }).await {
                        Ok(Ok((report,processing)))=>{
                            if report.active==0 && !processing {break;}
                            activity.last_activity.store(unix_ms(),Ordering::SeqCst);
                        },
                        _=>{activity.last_activity.store(unix_ms(),Ordering::SeqCst);},
                    }
                }
            }
            accepted = accept_client(&mut listener, external_listener.as_mut()) => {
                match accepted {
                    Ok((stream, authenticated_peer, role)) => {
                        // Transport authentication is necessary for both roles.
                        // Only the external endpoint also requires a read grant.
                        while tasks.try_join_next().is_some() {}
                        if tasks.len() >= MAX_CONNECTIONS || activity.clients.load(Ordering::SeqCst) >= MAX_CONNECTIONS { drop(stream); continue; }
                        activity.clients.fetch_add(1, Ordering::SeqCst);
                        let lease = ClientLease(activity.clone());
                        let repository = repository.clone();
                        let shutdown = shutdown.clone();
                        let external_gate = external_gate.clone();
                        let status = status.clone();
                        tasks.spawn(async move {
                            let _lease = lease;
                            match role {
                                ClientRole::Application => handle(
                                    stream, authenticated_peer.process_id, repository, shutdown, external_gate,
                                ).await,
                                ClientRole::ExternalRead => {
                                    if let Some(gate) = external_gate {
                                        external_read_endpoint::handle(stream, status, gate).await;
                                    }
                                }
                            }
                        });
                    }
                    Err(_) => { tokio::time::sleep(Duration::from_millis(10)).await; }
                }
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {},
        }
    }
    drop(listener);
    drop(external_listener);
    // Let the acknowledged shutdown caller leave before dropping remaining peers.
    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    // Drain any verified handoff commit before removing discovery or releasing
    // the repository owner. Waiting native jobs do not keep this host alive.
    monitor_stop.notify_one();
    purge_stop.notify_one();
    let _ = monitor.await;
    let _ = purge_sweep.await;
    // Only remove this exact instance's public discovery while ownership is held.
    if read_endpoint(&root)
        .ok()
        .is_some_and(|current| current.library_epoch == descriptor.library_epoch)
    {
        std::fs::remove_file(root.join(ENDPOINT_FILE))?;
    }
    drop(external_gate);
    drop(repository);
    Ok(())
}

fn worker_error(error: CoreError) -> RpcError {
    let code = match error {
        CoreError::Data(DataError::Forbidden) => ErrorCode::PermissionDenied,
        CoreError::Data(DataError::Busy) => ErrorCode::ResourceBusy,
        CoreError::Data(DataError::Invalid(_)) => ErrorCode::InvalidArgument,
        CoreError::Data(DataError::NotFound) => ErrorCode::NotFound,
        CoreError::Data(DataError::Conflict { .. }) => ErrorCode::Conflict,
        CoreError::Data(DataError::StaleCursor) => ErrorCode::ResyncRequired,
        CoreError::Data(DataError::IdempotencyConflict) => ErrorCode::IdempotencyConflict,
        _ => ErrorCode::BackendUnavailable,
    };
    RpcError::new(code)
}

async fn worker_operation<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
    project: fn(T) -> Reply,
) -> Reply {
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => project(value),
        Ok(Err(error)) => Reply::Error(worker_error(error)),
        Err(_) => Reply::Error(RpcError::new(ErrorCode::BackendUnavailable)),
    }
}

async fn desktop_operation<T: serde::Serialize + Send + 'static>(
    operation: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
    project: fn(T) -> Reply,
) -> Reply {
    worker_operation(
        move || {
            let reply = operation()?;
            if !bazza_service_api::desktop::desktop_body_fits(
                &reply,
                bazza_service_api::desktop::MAX_DESKTOP_REPLY_BYTES,
            ) {
                return Err(DataError::Invalid("desktop reply budget").into());
            }
            Ok(reply)
        },
        project,
    )
    .await
}

async fn handle(
    mut stream: PeerStream,
    peer_pid: u32,
    repository: Arc<Repository>,
    shutdown: Arc<Notify>,
    external_gate: Option<Arc<ExternalReadGate>>,
) {
    let mut authenticated = false;
    let transcript_transfer = Arc::new(std::sync::Mutex::new(bazza_core::TranscriptVersionTransfer::default()));
    loop {
        let request: RequestEnvelope =
            match tokio::time::timeout(Duration::from_millis(MAX_CALL_MS), read_frame(&mut stream))
                .await
            {
                Ok(Ok(request)) => request,
                _ => return,
            };
        let mut status = repository.status();
        if external_gate.is_some() || !status.development_fixture_only {
            status.capabilities.retain(|capability| {
                capability != bazza_service_api::fixture_read::FIXTURE_READ_CAPABILITY
            });
        }
        let now = unix_ms();
        let max_call_ms = if matches!(&request.request, Request::PostProcessVideo(_)) {
            bazza_service_api::postprocess_video::MAX_POST_PROCESS_VIDEO_CALL_MS
        } else {
            MAX_CALL_MS
        };
        let mut should_shutdown = false;
        let reply = if request.library_epoch != status.library_epoch {
            Reply::Error(RpcError::new(ErrorCode::ResyncRequired))
        } else if request.deadline_unix_ms <= now {
            Reply::Error(RpcError::new(ErrorCode::DeadlineExceeded))
        } else if request.request_id.is_nil()
            || request.deadline_unix_ms > now.saturating_add(max_call_ms)
        {
            Reply::Error(RpcError::new(ErrorCode::InvalidArgument))
        } else {
            match request.request {
                Request::Hello(hello) => {
                    if hello.compatible_with_mode(&status, status.development_fixture_only) {
                        authenticated = true;
                        Reply::Hello(status.clone())
                    } else {
                        Reply::Error(RpcError::new(ErrorCode::UpgradeRequired))
                    }
                }
                _ if !authenticated => Reply::Error(RpcError::new(ErrorCode::AuthRequired)),
                Request::IssueExternalReadGrant(request) => {
                    if external_gate.is_some() {
                        let repository = repository.clone();
                        worker_operation(
                            move || {
                                let issued =
                                    repository.issue_external_read_grant(&request.client_label)?;
                                let secret =
                                    WorkerSecret::from_string(issued.secret.expose().to_owned())
                                        .map_err(|_| {
                                            DataError::Invalid("external read credential")
                                        })?;
                                Ok(ExternalReadGrantIssued {
                                    library_id: issued.grant.library_id,
                                    connection_id: issued.grant.connection_id,
                                    secret,
                                })
                            },
                            Reply::ExternalReadGrantIssued,
                        )
                        .await
                    } else {
                        Reply::Error(RpcError::new(ErrorCode::PermissionDenied))
                    }
                }
                Request::RevokeExternalReadGrant(input) => {
                    if let Some(gate) = &external_gate {
                        let deadline =
                            Instant::now() + Duration::from_millis(request.deadline_unix_ms - now);
                        match gate.revoke(input.connection_id, deadline).await {
                            Ok(grant) => {
                                Reply::ExternalReadGrantRevoked(ExternalReadGrantRevoked {
                                    library_id: grant.library_id,
                                    connection_id: grant.connection_id,
                                })
                            }
                            Err(error) => Reply::Error(RpcError::new(match error {
                                ExternalReadError::AccessDenied => ErrorCode::PermissionDenied,
                                ExternalReadError::GrantNotFound => ErrorCode::NotFound,
                                ExternalReadError::DeadlineExceeded => ErrorCode::DeadlineExceeded,
                                _ => ErrorCode::BackendUnavailable,
                            })),
                        }
                    } else {
                        Reply::Error(RpcError::new(ErrorCode::PermissionDenied))
                    }
                }
                Request::ListExternalReadGrants(input) => {
                    if external_gate.is_some() {
                        let repository = repository.clone();
                        desktop_operation(
                            move || {
                                if !input.valid() {
                                    return Err(
                                        DataError::Invalid("external read grant page").into()
                                    );
                                }
                                let grants = repository
                                    .list_external_read_grants(input.after, input.limit + 1)?;
                                let limit = usize::from(input.limit);
                                let next_after =
                                    grants.get(limit).map(|_| grants[limit - 1].connection_id);
                                let page = ExternalReadGrantPage {
                                    library_id: status.library_id,
                                    grants: grants
                                        .into_iter()
                                        .take(limit)
                                        .map(|grant| ExternalReadGrantMetadata {
                                            connection_id: grant.connection_id,
                                            client_label: grant.client_label,
                                            permission: ExternalReadGrantPermission::Read,
                                            created_at: grant.created_at,
                                            revoked_at: grant.revoked_at,
                                        })
                                        .collect(),
                                    next_after,
                                };
                                if !page.valid_for(&input, status.library_id) {
                                    return Err(DataError::Invalid(
                                        "external read grant page result",
                                    )
                                    .into());
                                }
                                Ok(page)
                            },
                            Reply::ExternalReadGrantPage,
                        )
                        .await
                    } else {
                        Reply::Error(RpcError::new(ErrorCode::PermissionDenied))
                    }
                }
                Request::GetStatus => Reply::Status(status.clone()),
                Request::GetMeeting(meeting) => {
                    let repository = repository.clone();
                    match tokio::task::spawn_blocking(move || {
                        repository.meeting(meeting.meeting_id)
                    })
                    .await
                    {
                        Ok(Ok(meeting)) => Reply::Meeting(meeting),
                        _ => Reply::Error(RpcError::new(ErrorCode::BackendUnavailable)),
                    }
                }
                Request::RegisterWorker(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.register_worker(peer_pid, request),
                        Reply::RegisteredWorker,
                    )
                    .await
                }
                Request::RenewWorker(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.renew_worker(peer_pid, request),
                        Reply::RegisteredWorker,
                    )
                    .await
                }
                Request::PrepareCapture(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.prepare_capture(peer_pid, request),
                        Reply::CaptureLease,
                    )
                    .await
                }
                Request::ReattachCapture(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.reattach_capture(peer_pid, request),
                        Reply::CaptureLease,
                    )
                    .await
                }
                Request::BeginCapture(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.begin_capture(peer_pid, request),
                        Reply::CaptureBegun,
                    )
                    .await
                }
                Request::AbortCapture(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.abort_capture(peer_pid, request),
                        Reply::CaptureAborted,
                    )
                    .await
                }
                Request::FinalizeCapture(request) => {
                    let repository = repository.clone();
                    worker_operation(
                        move || repository.finalize_capture(peer_pid, request),
                        Reply::WorkerMutationOutcome,
                    )
                    .await
                }
                Request::SonioxPermission(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.soniox_permission(peer_pid, request),
                        |reply| Reply::SonioxPermission(Box::new(reply)),
                    )
                    .await
                }
                Request::SonioxKey(operation) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.soniox_key(peer_pid, operation),
                        Reply::SonioxKeyPresent,
                    )
                    .await
                }
                Request::DesktopBootstrap => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_bootstrap(peer_pid),
                        Reply::DesktopBootstrap,
                    )
                    .await
                }
                Request::DesktopPreferences => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_preferences(peer_pid),
                        Reply::DesktopPreferences,
                    )
                    .await
                }
                Request::UpdateDesktopPreferences(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.update_desktop_preferences(peer_pid, request),
                        Reply::DesktopMutation,
                    )
                    .await
                }
                Request::DesktopMeetings(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_meetings(peer_pid, request),
                        Reply::DesktopMeetings,
                    )
                    .await
                }
                Request::DesktopMeeting(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_meeting(peer_pid, request),
                        |value| Reply::DesktopMeeting(Box::new(value)),
                    )
                    .await
                }
                Request::DesktopMeetingDecorations(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_meeting_decorations(peer_pid, request),
                        |value| Reply::DesktopMeetingDecorations(Box::new(value)),
                    )
                    .await
                }
                Request::DesktopChanges(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_changes(peer_pid, request),
                        Reply::DesktopChanges,
                    )
                    .await
                }
                Request::DesktopLibrary(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_library(peer_pid, request),
                        |value| Reply::DesktopLibrary(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessJobs(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_jobs(peer_pid, *request),
                        |value| Reply::PostProcessJobs(Box::new(value)),
                    )
                    .await
                }
                Request::ClaimPostProcessJob(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.claim_post_process_job(peer_pid, *request),
                        |value| Reply::PostProcessClaimed(Box::new(value)),
                    )
                    .await
                }
                Request::RenewPostProcessJob(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.renew_post_process_job(peer_pid, *request),
                        |value| Reply::PostProcessRenewed(Box::new(value)),
                    )
                    .await
                }
                Request::FailPostProcessJob(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.fail_post_process_job(peer_pid, *request),
                        |value| Reply::PostProcessFailed(Box::new(value)),
                    )
                    .await
                }
                Request::DesktopTemplates(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_templates(peer_pid, *request),
                        |value| Reply::DesktopTemplates(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessTranscript(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_transcript(peer_pid, *request),
                        |value| Reply::PostProcessTranscript(Box::new(value)),
                    )
                    .await
                }
                Request::CommitPostProcessAutoTitle(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.commit_post_process_auto_title(peer_pid, *request),
                        |value| Reply::PostProcessAutoTitle(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessMedia(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_media(peer_pid, *request),
                        |value| Reply::PostProcessMedia(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessVideo(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_video(peer_pid, *request),
                        |value| Reply::PostProcessVideo(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessCleanup(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_cleanup(peer_pid, *request),
                        |value| Reply::PostProcessCleanup(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessSource(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_source(peer_pid, *request),
                        |value| Reply::PostProcessSource(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessSpeakers(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_speakers(peer_pid, *request),
                        |value| Reply::PostProcessSpeakers(Box::new(value)),
                    )
                    .await
                }
                Request::MarkPostProcessReady(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.mark_post_process_ready(peer_pid, *request),
                        |value| Reply::PostProcessReady(Box::new(value)),
                    )
                    .await
                }
                Request::PostProcessSummary(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.post_process_summary(peer_pid, *request),
                        |value| Reply::PostProcessSummary(Box::new(value)),
                    )
                    .await
                }
                Request::CompleteFixturePostProcess(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.complete_fixture_post_process(peer_pid, *request),
                        |value| Reply::FixturePostProcessComplete(Box::new(value)),
                    )
                    .await
                }
                Request::DesktopChats(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_chats(peer_pid, *request),
                        |value| Reply::DesktopChats(Box::new(value)),
                    )
                    .await
                }
                Request::FixtureRead(_)
                    if external_gate.is_some() || !status.development_fixture_only => {
                    Reply::Error(RpcError::new(ErrorCode::PermissionDenied))
                }
                Request::FixtureRead(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || Ok(repository.fixture_read(*request)),
                        |value| Reply::FixtureRead(Box::new(value)),
                    )
                    .await
                }
                Request::NativeConfiguration => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.native_configuration(peer_pid),
                        |value| Reply::NativeConfiguration(Box::new(value)),
                    )
                    .await
                }
                Request::UpdateNativeConfiguration(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.update_native_configuration(peer_pid, *request),
                        Reply::NativeConfigurationUpdated,
                    )
                    .await
                }
                Request::RenameDesktopMeeting(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.rename_desktop_meeting(peer_pid, request),
                        Reply::DesktopMutation,
                    )
                    .await
                }
                Request::DeleteDesktopMeeting(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.delete_desktop_meeting(peer_pid, request),
                        Reply::DesktopMutation,
                    )
                    .await
                }
                Request::DesktopTranscript(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_transcript(peer_pid, request),
                        |value| Reply::DesktopTranscript(Box::new(value)),
                    )
                    .await
                }
                Request::DesktopTranscriptVersion(request) => {
                    let repository = repository.clone();
                    let transfer = transcript_transfer.clone();
                    desktop_operation(
                        move || {
                            let mut transfer = transfer.lock().map_err(|_| DataError::Busy)?;
                            repository.desktop_transcript_version(peer_pid, *request, &mut transfer)
                        },
                        |value| Reply::DesktopTranscriptVersion(Box::new(value)),
                    )
                    .await
                }
                Request::DesktopSummary(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.desktop_summary(peer_pid, request),
                        |value| Reply::DesktopSummary(Box::new(value)),
                    )
                    .await
                }
                Request::CommitDesktopAgentSummary(request) => {
                    let repository = repository.clone();
                    desktop_operation(
                        move || repository.commit_desktop_agent_summary(peer_pid, *request),
                        Reply::DesktopMutation,
                    )
                    .await
                }
                Request::Shutdown => {
                    should_shutdown = true;
                    Reply::ShutdownAccepted
                }
            }
        };
        if request.deadline_unix_ms <= unix_ms() && !should_shutdown {
            let reply = ReplyEnvelope {
                request_id: request.request_id,
                library_epoch: status.library_epoch,
                reply: Reply::Error(RpcError::new(ErrorCode::DeadlineExceeded)),
            };
            let _ = tokio::time::timeout(
                Duration::from_millis(MAX_CALL_MS),
                write_frame(&mut stream, &reply),
            )
            .await;
            continue;
        }
        let envelope = ReplyEnvelope {
            request_id: request.request_id,
            library_epoch: status.library_epoch,
            reply,
        };
        if !matches!(
            tokio::time::timeout(
                Duration::from_millis(MAX_CALL_MS),
                write_frame(&mut stream, &envelope)
            )
            .await,
            Ok(Ok(()))
        ) {
            return;
        }
        if should_shutdown {
            shutdown.notify_one();
            return;
        }
    }
}

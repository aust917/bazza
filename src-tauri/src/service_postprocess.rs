//! Native executor for durable post-processing owned by the service desktop.
//! The service owns every transcript, checkpoint and lifecycle write.
use crate::{events, recorder_service::ServiceCapture, state::RuntimeState};
use bazza_service_client::{
    api::{
        content::{DesktopTranscriptRepresentation, DesktopTranscriptRequest},
        desktop::{MAX_DESKTOP_DATA_BYTES, MAX_DESKTOP_ITEMS},
        postprocess::{
            ClaimPostProcessJobRequest, FailPostProcessJobRequest, PostProcessFailureCode,
            PostProcessJob, PostProcessJobLease, PostProcessJobsRequest, PostProcessStage,
            RenewPostProcessJobRequest, FIXTURE_POST_PROCESS_CAPABILITY,
        },
        postprocess_input::PostProcessTranscriptRequest,
        postprocess_media::PostProcessMediaKind,
        postprocess_source::{LocalAsrBody, LocalAsrCaption, LocalAsrOutcome},
        worker::{
            CaptureProvider, CaptureSelection, RegisterWorkerRequest, RegisteredWorker,
            RenewWorkerRequest, WorkerCapability, WorkerProof, WorkerRegistration, WorkerSecret,
        },
    },
    postprocess_media::{OwnedPostProcessMedia, WorkerMediaInputRequest},
    validate_fixture_root, validate_managed_root, ClientError, ServiceClient,
};
use chrono::Utc;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

#[path = "service_postprocess_llm.rs"]
mod llm;
#[path = "service_postprocess_stages.rs"]
mod stages;
#[path = "service_postprocess_upload.rs"]
mod upload;
#[path = "service_postprocess_video.rs"]
mod video;

const WORKER_TTL: i64 = 180;
const CLAIM_TTL: i64 = 90;
const RENEW_WINDOW: i64 = 25;
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_JOB_PAGES: usize = 128;

pub(crate) struct ServicePostprocess(Arc<AtomicBool>);

impl ServicePostprocess {
    pub(crate) fn shutdown(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Drop for ServicePostprocess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn start(
    app: AppHandle,
    root: PathBuf,
    library_id: Uuid,
    managed: bool,
) -> Result<ServicePostprocess, String> {
    let root = selected_root(&root, library_id, managed)
        .map_err(|_| "Invalid post-process library root.")?;
    if library_id.is_nil()
        || app.try_state::<RuntimeState>().is_none()
        || app.try_state::<ServiceCapture>().is_none()
    {
        return Err("The service desktop post-process runtime is unavailable.".into());
    }
    let stopping = Arc::new(AtomicBool::new(false));
    tauri::async_runtime::spawn(run(app, root, library_id, managed, stopping.clone()));
    Ok(ServicePostprocess(stopping))
}

#[derive(Debug)]
enum WorkError {
    Service(ClientError),
    Deferred(&'static str),
    Failed(PostProcessFailureCode),
    Stopping,
}

impl From<ClientError> for WorkError {
    fn from(error: ClientError) -> Self {
        Self::Service(error)
    }
}

type WorkResult<T> = Result<T, WorkError>;

struct Worker {
    client: ServiceClient,
    registered: RegisteredWorker,
    proof: WorkerProof,
}

struct Claim {
    lease: PostProcessJobLease,
    fence: WorkerSecret,
}

fn selected_root(root: &Path, library_id: Uuid, managed: bool) -> Result<PathBuf, ClientError> {
    if managed {
        let (root, marker_id) = validate_managed_root(root)?;
        if marker_id != library_id {
            return Err(ClientError::ManagedLibraryUnavailable);
        }
        return Ok(root);
    }
    validate_fixture_root(root)
}

async fn run(
    app: AppHandle,
    root: PathBuf,
    library_id: Uuid,
    managed: bool,
    stopping: Arc<AtomicBool>,
) {
    let mut worker = None;
    while !stopping.load(Ordering::Acquire) {
        let capture_idle = app.state::<ServiceCapture>().try_admit_postprocess();
        if !capture_idle {
            // The service's private socket has an idle deadline. Reconnect
            // when admission opens instead of polling through capture.
            worker = None;
        } else {
            if worker.is_none() {
                worker = Worker::connect(&root, library_id, managed).await.ok();
            }
            if let Some(connected) = worker.as_mut() {
                if connected.run_one(&app, &root, &stopping).await.is_err() {
                    worker = None;
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

impl Worker {
    async fn connect(root: &Path, library_id: Uuid, managed: bool) -> WorkResult<Self> {
        // Revalidate the marker on reconnect. This executor never creates a
        // service process or follows the ordinary profile pointer.
        if selected_root(root, library_id, managed)?.as_path() != root {
            return Err(ClientError::Discovery.into());
        }
        let mut client = if managed {
            ServiceClient::connect_managed(root).await?
        } else {
            ServiceClient::connect(root).await?
        };
        let status = client.status().await?;
        if status.library_id != library_id
            || status.development_fixture_only == managed
            || !status
                .capabilities
                .iter()
                .any(|value| value == FIXTURE_POST_PROCESS_CAPABILITY)
        {
            return Err(ClientError::Protocol.into());
        }
        let registration = WorkerRegistration::new(vec![WorkerCapability::PostProcess]);
        let registered = client
            .register_worker(RegisterWorkerRequest {
                registration: registration.clone(),
                ttl_seconds: WORKER_TTL,
            })
            .await?;
        let proof = WorkerProof {
            session_id: registered.session_id,
            library_epoch: registered.library_epoch,
            secret: registration.secret,
        };
        Ok(Self {
            client,
            registered,
            proof,
        })
    }

    async fn keep_registration(&mut self) -> WorkResult<()> {
        if self.registered.expires_at
            <= Utc::now() + chrono::Duration::seconds(CLAIM_TTL + RENEW_WINDOW)
        {
            let updated = self
                .client
                .renew_worker(RenewWorkerRequest {
                    proof: self.proof.clone(),
                    ttl_seconds: WORKER_TTL,
                })
                .await?;
            if updated.capabilities != self.registered.capabilities {
                return Err(ClientError::Protocol.into());
            }
            self.registered = updated;
        }
        Ok(())
    }

    async fn keep_claim(&mut self, claim: &mut Claim) -> WorkResult<bool> {
        self.keep_claim_for(claim, RENEW_WINDOW).await
    }

    async fn keep_claim_for(&mut self, claim: &mut Claim, seconds: i64) -> WorkResult<bool> {
        self.keep_registration().await?;
        if claim.lease.expires_at > Utc::now() + chrono::Duration::seconds(seconds) {
            return Ok(false);
        }
        claim.lease = self
            .client
            .renew_post_process_job(
                &self.registered,
                RenewPostProcessJobRequest {
                    proof: self.proof.clone(),
                    lease: claim.lease.clone(),
                    operation_id: Uuid::new_v4(),
                    fence: claim.fence.clone(),
                    ttl_seconds: CLAIM_TTL,
                },
            )
            .await?
            .data;
        Ok(true)
    }

    async fn next_job(&mut self) -> WorkResult<Option<PostProcessJob>> {
        self.keep_registration().await?;
        let mut request = PostProcessJobsRequest::new(self.proof.clone());
        request.limit = MAX_DESKTOP_ITEMS;
        for _ in 0..MAX_JOB_PAGES {
            let page = self
                .client
                .post_process_jobs(&self.registered, request.clone())
                .await?
                .data;
            if let Some(job) = page.items.into_iter().find(can_attempt) {
                return Ok(Some(job));
            }
            let Some(next) = page.next else {
                break;
            };
            request.cursor = Some(next);
        }
        Ok(None)
    }

    async fn run_one(
        &mut self,
        app: &AppHandle,
        root: &Path,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        let Some(job) = self.next_job().await? else {
            return Ok(());
        };
        if stopping.load(Ordering::Acquire)
            || !app.state::<ServiceCapture>().try_admit_postprocess()
        {
            return Ok(());
        }
        let fence = WorkerSecret::random();
        let lease = self
            .client
            .claim_post_process_job(
                &self.registered,
                ClaimPostProcessJobRequest {
                    proof: self.proof.clone(),
                    job: job.clone(),
                    claim_id: Uuid::new_v4(),
                    operation_id: Uuid::new_v4(),
                    fence: fence.clone(),
                    ttl_seconds: CLAIM_TTL,
                },
            )
            .await?
            .data;
        let mut claim = Claim { lease, fence };
        match self.execute(app, root, job, &mut claim, stopping).await {
            Err(WorkError::Failed(error_code)) if !stopping.load(Ordering::Acquire) => {
                self.keep_claim(&mut claim).await?;
                self.client
                    .fail_post_process_job(
                        &self.registered,
                        FailPostProcessJobRequest {
                            proof: self.proof.clone(),
                            lease: claim.lease.clone(),
                            fence: claim.fence.clone(),
                            operation_id: Uuid::new_v4(),
                            error_code,
                        },
                    )
                    .await?;
                let meeting_id = claim.lease.meeting_id;
                emit(
                    app,
                    meeting_id,
                    "failed",
                    1.0,
                    Some("Post-processing failed. Capture media was kept."),
                );
            }
            Err(WorkError::Deferred(reason)) => {
                log::debug!("service post-process deferred: {reason}")
            }
            Err(WorkError::Service(error)) => return Err(error.into()),
            Err(WorkError::Failed(_) | WorkError::Stopping) | Ok(()) => {}
        }
        Ok(())
    }

    async fn media(
        &mut self,
        root: &Path,
        claim: &Claim,
        provider: &CaptureSelection,
    ) -> WorkResult<OwnedPostProcessMedia> {
        self.client
            .acquire_post_process_media(
                &self.registered,
                root,
                WorkerMediaInputRequest {
                    proof: self.proof.clone(),
                    lease: claim.lease.clone(),
                    fence: claim.fence.clone(),
                    expected_provider: provider.clone(),
                },
            )
            .await
            .map_err(Into::into)
    }

    async fn compute<T: Send + 'static>(
        &mut self,
        claim: &mut Claim,
        stopping: &AtomicBool,
        work: impl FnOnce() -> WorkResult<T> + Send + 'static,
    ) -> WorkResult<T> {
        self.compute_with_cancel(claim, stopping, None, work).await
    }

    async fn compute_with_cancel<T: Send + 'static>(
        &mut self,
        claim: &mut Claim,
        stopping: &AtomicBool,
        cancel: Option<Arc<AtomicBool>>,
        work: impl FnOnce() -> WorkResult<T> + Send + 'static,
    ) -> WorkResult<T> {
        if stopping.load(Ordering::Acquire) {
            return Err(WorkError::Stopping);
        }
        let mut task = tokio::task::spawn_blocking(work);
        let heartbeat_error = loop {
            tokio::select! {
                result = &mut task => {
                    if stopping.load(Ordering::Acquire) {
                        return Err(WorkError::Stopping);
                    }
                    self.keep_claim(claim).await?;
                    return result.map_err(|_| WorkError::Failed(PostProcessFailureCode::PostProcessFailed))?;
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {
                    if stopping.load(Ordering::Acquire) {
                        break WorkError::Stopping;
                    }
                    match self.keep_claim(claim).await {
                        Ok(true) => {}
                        Ok(false) => {
                            // The private transport closes after idle time.
                            if let Err(error) = self.client.status().await {
                                break error.into();
                            }
                        }
                        Err(error) => break error,
                    }
                }
            }
        };
        if let Some(cancel) = cancel { cancel.store(true, Ordering::Release); }
        // The native engine may still be running. Wait for the task to drop
        // its media gate before another job can start on this worker.
        let _ = task.await;
        Err(heartbeat_error)
    }

    async fn read_transcript(
        &mut self,
        claim: &mut Claim,
        representation: DesktopTranscriptRepresentation,
        stopping: &AtomicBool,
    ) -> WorkResult<Vec<TranscriptRow>> {
        let mut request = DesktopTranscriptRequest {
            meeting_id: claim.lease.meeting_id.to_string(),
            source_id: claim.lease.input.source_id.clone(),
            expected_revision: claim.lease.input.transcript_revision.clone(),
            representation,
            cursor: None,
            limit: MAX_DESKTOP_ITEMS,
            max_bytes: MAX_DESKTOP_DATA_BYTES,
        };
        let mut transcript = Transcript::default();
        // A maximally fragmented document remains bounded independently of
        // the number of bytes that each request is allowed to return.
        for _ in 0..2048 {
            if stopping.load(Ordering::Acquire) {
                return Err(WorkError::Stopping);
            }
            self.keep_claim(claim).await?;
            let page = self
                .client
                .post_process_transcript(
                    &self.registered,
                    PostProcessTranscriptRequest {
                        proof: self.proof.clone(),
                        lease: claim.lease.clone(),
                        fence: claim.fence.clone(),
                        page: request.clone(),
                    },
                )
                .await?
                .data;
            for fragment in page.items {
                transcript.push(fragment)?;
            }
            let Some(next) = page.next else {
                return transcript.finish();
            };
            request.cursor = Some(next);
        }
        Err(WorkError::Deferred(
            "transcript exceeds the native worker page limit",
        ))
    }
}

fn can_attempt(job: &PostProcessJob) -> bool {
    match job.stage {
        PostProcessStage::Captured => {
            job.provider.provider == CaptureProvider::Local || job.input.source_id.is_some()
        }
        PostProcessStage::AutomaticCleanup { .. } => false,
        _ => true,
    }
}

fn emit(
    app: &AppHandle,
    meeting_id: Uuid,
    stage: &'static str,
    progress: f32,
    error: Option<&str>,
) {
    let _ = app.emit(
        events::PROCESSING,
        events::ProcessingPayload {
            meeting_id: meeting_id.to_string(),
            stage,
            progress,
            error: error.map(str::to_owned),
        },
    );
}

fn local_source(
    app: &AppHandle,
    meeting_id: Uuid,
    media: &OwnedPostProcessMedia,
    provider: &CaptureSelection,
) -> WorkResult<LocalAsrBody> {
    let runtime = app.state::<RuntimeState>();
    let _asr_work = runtime.local_asr_work.lock();
    let _busy = runtime.busy_guard();
    let meeting_id = meeting_id.to_string();
    events::emit_transcription(app, &meeting_id, events::TranscriptionPhase::Reading, None, None);
    let mix = media_path(media, PostProcessMediaKind::AudioMix).ok_or(WorkError::Failed(
        PostProcessFailureCode::PostProcessInputUnavailable,
    ))?;
    let settings = runtime.settings.read().clone();
    let tier = settings.tier(&runtime.hardware);
    let (live_id, quality_id, _) = settings.model_ids(&runtime.catalog, tier);
    if mix.parent().is_some_and(crate::bot_supplement::marker_exists) {
        // No live rows (for example, late admission) is not permission to
        // transcribe the mix. The separate supplement recovers local speech.
        return Ok(LocalAsrBody { model: quality_id, language: provider.language.clone(),
            outcome: LocalAsrOutcome::Complete, captions: Vec::new() });
    }
    let pcm = crate::recorder::read_wav_as_16k_mono(mix)
        .map_err(|_| WorkError::Failed(PostProcessFailureCode::PostProcessInputUnavailable))?;
    let audio_ms = (pcm.len() as i64) * 1000 / 16_000;
    let preferred = provider.model.as_deref().unwrap_or(&quality_id);
    let quality_id =
        crate::asr_pick::pick_with_measurements(&runtime, preferred, audio_ms, |_| None);
    events::emit_transcription(app, &meeting_id, events::TranscriptionPhase::Loading, None, None);
    let Some(engine) = runtime.whisper_or_any(&[&quality_id, &live_id]) else {
        let has_model = runtime.catalog.all().iter().any(|model| {
            model.kind == bazza_models::ModelKind::Asr
                && bazza_models::is_present(&runtime.models_dir(), model)
        });
        if has_model {
            return Err(WorkError::Failed(PostProcessFailureCode::PostProcessFailed));
        }
        return Ok(LocalAsrBody {
            model: quality_id,
            language: provider.language.clone(),
            outcome: LocalAsrOutcome::NoModel,
            captions: Vec::new(),
        });
    };
    let decoding = |percent| events::emit_transcription(
        app, &meeting_id, events::TranscriptionPhase::Decoding, Some(engine.model_id()), percent,
    );
    decoding(None);
    let captions = bazza_asr::mix_prefix::transcribe_with_progress(
        &engine,
        &pcm,
        provider.language.as_deref(),
        |percent| decoding(Some(percent)),
    )
    .map_err(|_| WorkError::Failed(PostProcessFailureCode::PostProcessFailed))?;
    Ok(LocalAsrBody {
        model: engine.model_id().to_owned(),
        language: provider.language.clone(),
        outcome: LocalAsrOutcome::Complete,
        captions: captions
            .into_iter()
            .filter(|caption| !caption.text.trim().is_empty())
            .map(|caption| LocalAsrCaption {
                start_ms: caption.start_ms,
                end_ms: caption.end_ms,
                text: caption.text,
            })
            .collect(),
    })
}

fn media_path(media: &OwnedPostProcessMedia, kind: PostProcessMediaKind) -> Option<&Path> {
    media
        .grant()
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == kind)
        .map(|artifact| Path::new(&artifact.private_path))
}

#[path = "service_postprocess_transcript.rs"]
mod transcript;
use transcript::{Transcript, TranscriptRow};

#[path = "service_postprocess_supplement.rs"]
mod supplement;

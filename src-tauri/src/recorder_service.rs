//! Native capture acceptance for the explicitly selected service fixture.
//! A service lease owns metadata. The desktop owns the native writer and
//! holds the artifact lock from Begin through Stop and the terminal seal.

use crate::{
    events::{self, LevelsPayload, RecordingStatePayload},
    recorder::StartOptions,
    settings::Settings,
    state::RuntimeState,
    stt_handoff::{
        SonioxCaptureControl, SonioxCapturePermit, SonioxCaptureSession, SonioxPermissionBinding,
    },
};
use bazza_asr::{soniox::SonioxConfig, Caption};
use bazza_capture::{
    CaptureEvent, MonitorInfo, RecordConfig, RecordOutput, Recorder, VideoRecorder,
};
use bazza_capture_handoff::{
    ArtifactGuard, CaptureIdentity, CaptureTerminalStatus, ConfirmedCaption, HandoffError,
    ProviderProvenance, RawScreenWriterOutcome, SttProvider, TerminalManifest, MANIFEST_VERSION,
};
use bazza_domain::{capture_stt::SonioxCaptureOutcome, Meeting, MeetingStatus};
use bazza_service_client::{
    api::{
        desktop::{DesktopMeeting, DesktopMeetingLookup},
        rpc::{
            ErrorCode, RpcError, ServiceStatus, FIXTURE_CAPTURE_LOCAL_LIVE_CAPABILITY,
            FIXTURE_CAPTURE_TERMINAL_V2_CAPABILITY, FIXTURE_WORKER_CAPABILITY,
        },
        soniox_permission::{
            SonioxIncompleteReason, SonioxPermissionOperation, SonioxPermissionPermit,
            SonioxPermissionReply, SonioxPermissionRequest, FIXTURE_SONIOX_PERMISSION_CAPABILITY,
        },
        worker::*,
    },
    validate_fixture_root, ClientError, ServiceClient,
};
use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::{async_runtime, AppHandle, Emitter, Manager, State};
use uuid::Uuid;

#[cfg(all(test, windows))]
#[path = "recorder_service_soniox_live_test.rs"]
mod soniox_live_test;

const REGISTRATION_TTL_SECONDS: i64 = 120;
const SONIOX_RENEW_INTERVAL: Duration = Duration::from_secs(2);
const WORKER_RENEW_INTERVAL: Duration = Duration::from_secs(30);
type SharedArtifact = Arc<Mutex<ArtifactGuard>>;

// Match the service's terminal publication budget. The live view can only
// confirm rows the repository will be able to publish.
const MAX_CAPTURE_CAPTIONS: u64 = 10_000;
const MAX_CAPTURE_TERMINAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ServiceCapture(Arc<CaptureOwner>);

struct CaptureOwner {
    root: PathBuf,
    library_id: Uuid,
    managed: bool,
    operation: Mutex<()>,
    closing: AtomicBool,
    phase: Mutex<Phase>,
}

enum Phase {
    Idle(FixtureCaptureView),
    Working(FixtureCaptureView),
    Recording(Box<ActiveCapture>),
    Pending(Box<PendingCapture>),
}

enum PendingCapture {
    Start(Reservation),
    Finish(Completion),
}

struct Reservation {
    request: PrepareCaptureRequest,
    lease: Option<CaptureLease>,
    guard: Option<SharedArtifact>,
    prepare_replied: bool,
    begin_sent: bool,
}

struct ActiveCapture {
    recorder: Option<Recorder>,
    discord: Option<crate::discord_recording::DiscordRecording>,
    attendee: Option<crate::attendee_recording::AttendeeRecording>,
    screen: Option<ScreenCapture>,
    drain_stop: Arc<AtomicBool>,
    drain: Option<JoinHandle<()>>,
    transcription: CaptureTranscription,
    participant_live: Option<crate::participant_live::ParticipantSession>,
    live_journal: Option<Arc<LocalCaptionJournal>>,
    reservation: Option<Reservation>,
}

struct LocalCaptionJournal {
    artifact: Weak<Mutex<ArtifactGuard>>,
    provenance: ProviderProvenance,
    text_bytes: Mutex<Option<usize>>,
    warned: AtomicBool,
}

struct Completion {
    request: FinalizeCaptureRequest,
    guard: Option<SharedArtifact>,
    manifest: TerminalManifest,
    screen: Option<RawScreenWriterOutcome>,
    soniox_outcome: Option<SonioxCaptureOutcome>,
}

enum CaptureTranscription {
    Local(Option<crate::stt_stage::LiveSession>),
    Unavailable(SonioxIncompleteReason),
    Soniox(SonioxHandoff),
}

enum CaptionInput {
    Local(Sender<Vec<f32>>),
    Soniox(SonioxCaptureControl),
}

struct SonioxHandoff {
    control: SonioxCaptureControl,
    renew_stop: Sender<()>,
    renewal: Option<JoinHandle<()>>,
    drained: JoinHandle<SonioxCaptureOutcome>,
}

struct ScreenCapture {
    recorder: Option<VideoRecorder>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ScreenState {
    Recording,
    Stopped,
    Failed,
}

impl From<RawScreenWriterOutcome> for ScreenState {
    fn from(outcome: RawScreenWriterOutcome) -> Self {
        match outcome {
            RawScreenWriterOutcome::Stopped => Self::Stopped,
            RawScreenWriterOutcome::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CaptureState {
    Idle,
    Starting,
    Recording,
    Paused,
    Finishing,
    Pending,
}

impl CaptureState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Paused => "paused",
            Self::Finishing => "finishing",
            Self::Pending => "pending",
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct FixtureCaptureView {
    state: CaptureState,
    meeting_id: Option<Uuid>,
    elapsed_ms: u64,
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screen: Option<ScreenState>,
}

impl FixtureCaptureView {
    fn new(state: CaptureState, meeting_id: Option<Uuid>, elapsed_ms: u64) -> Self {
        Self {
            state,
            meeting_id,
            elapsed_ms,
            job_id: None,
            screen: None,
        }
    }

    fn with_screen(mut self, screen: Option<ScreenState>) -> Self {
        self.screen = screen;
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixtureCaptureStartRequest {
    title: Option<String>,
    #[serde(default)]
    capture_screen: bool,
    screen_id: Option<String>,
    #[serde(default)]
    use_configured_audio: bool,
}

struct CapturePlan {
    title: String,
    app: Option<String>,
    video_target: Option<MonitorInfo>,
    audio: CaptureAudio,
    provider: CaptureSelection,
    discord: Option<crate::discord_recording::DiscordRecordingSelection>,
    attendee: Option<crate::attendee_recording::AttendeeRecordingSelection>,
    prepared_bot: Option<crate::bot_connection::PreparedBot>,
    bot_provider: Option<&'static str>,
}

enum CaptureAudio {
    Fixture { use_configured_audio: bool },
    Desktop,
}

impl CapturePlan {
    fn fixture(request: FixtureCaptureStartRequest, video_target: Option<MonitorInfo>) -> Self {
        Self {
            title: request.title.unwrap_or_else(|| {
                format!(
                    "Service microphone {}",
                    chrono::Local::now().format("%Y-%m-%d %H:%M")
                )
            }),
            app: None,
            video_target,
            audio: CaptureAudio::Fixture {
                use_configured_audio: request.use_configured_audio,
            },
            provider: fixture_provider(),
            discord: None,
            attendee: None,
            prepared_bot: None,
            bot_provider: None,
        }
    }

    fn desktop(
        options: StartOptions,
        settings: &Settings,
        prepared_bot: Option<crate::bot_connection::PreparedBot>,
        video_target: Option<MonitorInfo>,
    ) -> Self {
        let bot_provider = options.bot_provider(prepared_bot.as_ref().map(|bot| bot.provider()));
        let title = options
            .title
            .unwrap_or_else(|| crate::recorder::default_title(options.app.as_deref()));
        // Named live captions come from the local recognizer on separate
        // bot tracks. The stored source follows that displayed lane.
        let soniox = bot_provider.is_none() && settings.stt_provider == bazza_asr::provider::SttProvider::Soniox;
        Self {
            title,
            app: options.app,
            video_target,
            audio: CaptureAudio::Desktop,
            provider: CaptureSelection {
                provider: if soniox {
                    CaptureProvider::Soniox
                } else {
                    CaptureProvider::Local
                },
                model: soniox.then(|| settings.soniox_model.clone()),
                language: settings.language.clone(),
            },
            discord: options.discord,
            attendee: options.attendee,
            prepared_bot,
            bot_provider,
        }
    }
}

impl CaptureAudio {
    fn config(self, directory: &Path, settings: &Settings) -> RecordConfig {
        match self {
            CaptureAudio::Fixture {
                use_configured_audio,
            } => fixture_audio_config(directory, settings, use_configured_audio),
            CaptureAudio::Desktop => crate::recorder::audio_writer_config(directory, settings),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CaptureError {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

impl CaptureError {
    const fn new(code: &'static str, message: &'static str, retryable: bool) -> Self {
        Self {
            code,
            message,
            retryable,
        }
    }

    fn unavailable() -> Self {
        Self::new(
            "SERVICE_UNAVAILABLE",
            "The selected fixture capture service is unavailable.",
            true,
        )
    }

    fn pending() -> Self {
        Self::new(
            "CAPTURE_PENDING",
            "Capture needs service confirmation. Select Stop again to retry.",
            true,
        )
    }

    fn writer_failed() -> Self {
        Self::new(
            "CAPTURE_FAILED",
            "The microphone capture failed. Available recording files were kept.",
            false,
        )
    }

    fn busy() -> Self {
        Self::new(
            "RESOURCE_BUSY",
            "The fixture recorder is already active or is closing.",
            false,
        )
    }
}

impl From<CaptureError> for crate::ui_message::UiMessage {
    fn from(error: CaptureError) -> Self {
        crate::ui_message::app(error.code, error.message)
    }
}

impl ServiceCapture {
    pub(crate) fn new(root: PathBuf, library_id: Uuid) -> Self {
        Self::for_root(root, library_id, false)
    }

    pub(crate) fn for_root(root: PathBuf, library_id: Uuid, managed: bool) -> Self {
        Self(Arc::new(CaptureOwner {
            root,
            library_id,
            managed,
            operation: Mutex::new(()),
            closing: AtomicBool::new(false),
            phase: Mutex::new(Phase::Idle(FixtureCaptureView::new(
                CaptureState::Idle,
                None,
                0,
            ))),
        }))
    }

    /// Admission for the dedicated fixture window's close event. Once the
    /// close is admitted, a queued Start cannot open a device behind it.
    pub(crate) fn may_close(&self) -> bool {
        let Some(_operation) = self.0.operation.try_lock() else {
            return false;
        };
        let Some(phase) = self.0.phase.try_lock() else {
            return false;
        };
        let can_recover = matches!(
            &*phase,
            Phase::Pending(pending)
                if matches!(pending.as_ref(), PendingCapture::Finish(completion) if completion.guard.is_none())
        );
        if !matches!(*phase, Phase::Idle(_)) && !can_recover {
            return false;
        }
        self.0.closing.store(true, Ordering::Release);
        true
    }

    pub(crate) fn try_admit_postprocess(&self) -> bool {
        let Some(_operation) = self.0.operation.try_lock() else {
            return false;
        };
        !self.0.closing.load(Ordering::Acquire)
            && self.0.phase.try_lock().is_some_and(|phase| matches!(*phase, Phase::Idle(_)))
    }

    fn connect(&self) -> Result<(ServiceClient, ServiceStatus), CaptureError> {
        if !self.0.managed
            && validate_fixture_root(&self.0.root).map_err(|_| CaptureError::unavailable())?
                != self.0.root
        {
            return Err(CaptureError::unavailable());
        }
        async_runtime::block_on(async {
            let mut client = if self.0.managed {
                ServiceClient::connect_managed(&self.0.root).await
            } else {
                ServiceClient::connect_or_start_fixture(&self.0.root).await
            }
                .map_err(|_| CaptureError::unavailable())?;
            let status = client
                .status()
                .await
                .map_err(|_| CaptureError::unavailable())?;
            if status.development_fixture_only == self.0.managed
                || status.library_id != self.0.library_id
                || !status
                    .capabilities
                    .iter()
                    .any(|capability| capability == FIXTURE_WORKER_CAPABILITY)
            {
                return Err(CaptureError::unavailable());
            }
            Ok((client, status))
        })
    }

    fn view(&self) -> FixtureCaptureView {
        match &*self.0.phase.lock() {
            Phase::Idle(view) | Phase::Working(view) => view.clone(),
            Phase::Recording(active) => active.status(),
            Phase::Pending(pending) => pending.view(CaptureState::Pending),
        }
    }

    pub(crate) fn recording_state(&self) -> RecordingStatePayload {
        let view = self.view();
        let idle = matches!(view.state, CaptureState::Idle);
        RecordingStatePayload {
            state: view.state.as_str(),
            meeting_id: view.meeting_id.filter(|_| !idle).map(|id| id.to_string()),
            elapsed_ms: if idle { 0 } else { view.elapsed_ms },
        }
    }

    pub(crate) fn discord_recording_status(&self) -> Option<crate::discord_recording::DiscordRecordingStatus> {
        let phase = self.0.phase.lock();
        let Phase::Recording(active) = &*phase else { return None };
        active.discord.as_ref().map(|discord| discord.status())
    }

    pub(crate) fn attendee_recording_status(&self) -> Option<crate::attendee_recording::AttendeeRecordingStatus> {
        let phase = self.0.phase.lock();
        let Phase::Recording(active) = &*phase else { return None };
        active.attendee.as_ref().map(|attendee| attendee.status())
    }

    pub(crate) fn publish_desktop_state(&self, app: &AppHandle, update_windows: bool) {
        let state = self.recording_state();
        if update_windows {
            match state.state {
                "idle" => crate::windows::hide_recordbar(app),
                "recording" | "paused" | "pending" => {
                    crate::windows::hide_popup(app);
                    crate::windows::show_recordbar(app);
                }
                _ => {}
            }
        }
        let _ = app.emit(events::RECORDING_STATE, state);
    }

    pub(crate) fn start_desktop(
        &self,
        options: StartOptions,
        settings: &Settings,
        ffmpeg: Option<&Path>,
        app: &AppHandle,
        prepared_bot: Option<crate::bot_connection::PreparedBot>,
        screen: Option<MonitorInfo>,
    ) -> Result<Meeting, CaptureError> {
        let plan = CapturePlan::desktop(options, settings, prepared_bot, screen);
        let view = self.start_plan(plan, settings, ffmpeg, Some(app.clone()))?;
        if matches!(view.screen, Some(ScreenState::Failed)) {
            let (code, fallback) = if ffmpeg.is_none() {
                ("native.recordingAudioOnly", "ffmpeg not found; recording audio only.")
            } else {
                (
                    "native.screenStartFailed",
                    "Screen recording could not start. Recording audio only.",
                )
            };
            crate::recorder::notice_message(
                app,
                "warning",
                crate::ui_message::app(code, fallback),
            );
        }
        self.desktop_meeting(view)
    }

    pub(crate) fn stop_desktop(&self, app: &AppHandle) -> Result<Meeting, CaptureError> {
        let view = self.stop_matching(None)?;
        if let (Some(id), Some(runtime)) = (view.meeting_id, app.try_state::<crate::state::RuntimeState>()) {
            crate::live_feed::LiveFeed::end_for(&runtime, &id.to_string());
        }
        if matches!(view.screen, Some(ScreenState::Failed)) {
            crate::recorder::notice_message(
                app,
                "warning",
                crate::ui_message::app(
                    "native.screenFinishFailed",
                    "Screen recording did not finish cleanly. Audio and recoverable video were kept.",
                ),
            );
        }
        self.desktop_meeting(view)
    }

    fn desktop_meeting(&self, view: FixtureCaptureView) -> Result<Meeting, CaptureError> {
        let meeting_id = view
            .meeting_id
            .ok_or_else(|| CaptureError::new("native.notRecording", "Not recording.", false))?;
        let (mut client, _) = self.connect()?;
        let meeting = async_runtime::block_on(client.desktop_meeting(DesktopMeetingLookup {
            meeting_id: meeting_id.to_string(),
        }))
        .map_err(|_| CaptureError::unavailable())?
        .data
        .ok_or_else(CaptureError::unavailable)?;
        native_meeting(meeting)
    }

    fn start(
        &self,
        request: FixtureCaptureStartRequest,
        settings: &Settings,
        ffmpeg: Option<&Path>,
    ) -> Result<FixtureCaptureView, CaptureError> {
        let screen = crate::recorder::resolve_recording_screen(
            request.capture_screen, None, request.screen_id.as_deref(), ffmpeg,
        ).map_err(|error| {
            let (code, message) = error.message();
            CaptureError::new(code, message, false)
        })?;
        self.start_plan(CapturePlan::fixture(request, screen), settings, ffmpeg, None)
    }

    fn start_plan(
        &self,
        plan: CapturePlan,
        settings: &Settings,
        ffmpeg: Option<&Path>,
        desktop: Option<AppHandle>,
    ) -> Result<FixtureCaptureView, CaptureError> {
        let _operation = self.0.operation.try_lock().ok_or_else(CaptureError::busy)?;
        if self.0.closing.load(Ordering::Acquire) || !matches!(*self.0.phase.lock(), Phase::Idle(_))
        {
            return Err(CaptureError::busy());
        }
        let CapturePlan {
            title,
            app,
            video_target,
            audio,
            provider,
            discord,
            attendee,
            prepared_bot,
            bot_provider,
        } = plan;
        if prepared_bot.as_ref().is_some_and(|bot| bot.validate().is_err()) {
            return Err(CaptureError::new("setup.botConnectionLost", "The bot connection is unavailable. Connect the bot again before recording.", false));
        }
        if title.trim().is_empty() || title.len() > 512 || title.contains('\0') {
            return Err(CaptureError::new(
                "INVALID_ARGUMENT",
                "Enter a recording title of up to 512 UTF-8 bytes.",
                false,
            ));
        }
        let (mut client, status) = self.connect()?;
        let soniox = provider.provider == CaptureProvider::Soniox;
        let evidence_supported = status
            .capabilities
            .iter()
            .any(|capability| capability == FIXTURE_CAPTURE_TERMINAL_V2_CAPABILITY);
        if (video_target.is_some() || soniox) && !evidence_supported {
            return Err(CaptureError::new(
                "CAPTURE_EVIDENCE_UNAVAILABLE",
                "The selected service cannot seal the requested capture evidence.",
                false,
            ));
        }
        let permission_supported = status
            .capabilities
            .iter()
            .any(|capability| capability == FIXTURE_SONIOX_PERMISSION_CAPABILITY);
        let local_live_supported = status
            .capabilities
            .iter()
            .any(|capability| capability == FIXTURE_CAPTURE_LOCAL_LIVE_CAPABILITY);
        let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
        let worker = async_runtime::block_on(client.register_worker(RegisterWorkerRequest {
            registration: registration.clone(),
            ttl_seconds: REGISTRATION_TTL_SECONDS,
        }))
        .map_err(|_| CaptureError::unavailable())?;
        let mut reservation = Reservation {
            request: PrepareCaptureRequest {
                proof: WorkerProof {
                    session_id: worker.session_id,
                    secret: registration.secret,
                    library_epoch: worker.library_epoch,
                },
                start: CaptureStart {
                    capture_id: Uuid::new_v4(),
                    meeting_id: Uuid::new_v4(),
                    fence: WorkerSecret::random(),
                    title,
                    app,
                    started_at: Utc::now(),
                    participants: vec![],
                    provider,
                },
            },
            lease: None,
            guard: None,
            prepare_replied: false,
            begin_sent: false,
        };
        let id = reservation.meeting_id();
        *self.0.phase.lock() =
            Phase::Working(FixtureCaptureView::new(CaptureState::Starting, Some(id), 0));
        if reservation
            .begin(self, &mut client, worker.library_epoch)
            .is_err()
        {
            self.finish(PendingCapture::Start(reservation))?;
            return Err(CaptureError::unavailable());
        }

        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (forwarder_ready, wait_for_forwarder) =
            crossbeam_channel::bounded::<Option<CaptionInput>>(1);
        let drain_stop = Arc::new(AtomicBool::new(false));
        let drain = {
            let owner = Arc::downgrade(&self.0);
            let stop = drain_stop.clone();
            let desktop = desktop.clone();
            thread::Builder::new()
                .name("bazza-service-microphone-events".into())
                .spawn(move || {
                    let Ok(mut caption_input) = wait_for_forwarder.recv() else {
                        return;
                    };
                    let mut next_video_check = Instant::now();
                    loop {
                        let event = if stop.load(Ordering::Acquire) {
                            // Recorder::stop already joined the PCM producer.
                            // Device event senders may still be disconnecting.
                            events_rx.try_recv().ok()
                        } else {
                            let now = Instant::now();
                            if let Some(app) = desktop.as_ref().filter(|_| now >= next_video_check) {
                                next_video_check = now + Duration::from_secs(1);
                                let warning = owner.upgrade().and_then(|owner| {
                                    // Stop joins the drain, so skip a busy phase transition.
                                    let mut phase = owner.phase.try_lock()?;
                                    let Phase::Recording(active) = &mut *phase else {
                                        return None;
                                    };
                                    if active.meeting_id() != id {
                                        return None;
                                    }
                                    active.screen.as_mut()?.recorder.as_mut()?.poll_warning()
                                });
                                if let Some(warning) = warning {
                                    crate::recorder::notice_message(
                                        app,
                                        "warning",
                                        crate::ui_message::app("native.screenCaptureInterrupted", warning.clone())
                                            .with_detail(warning),
                                    );
                                }
                            }
                            match events_rx.recv_timeout(Duration::from_millis(100)) {
                                Ok(event) => Some(event),
                                Err(RecvTimeoutError::Timeout) => continue,
                                Err(RecvTimeoutError::Disconnected) => None,
                            }
                        };
                        let Some(event) = event else {
                            break;
                        };
                        match event {
                            CaptureEvent::WriterError(_) => {
                                if let Some(owner) = owner.upgrade() {
                                    let desktop = desktop.clone();
                                    // This thread cannot join itself during Stop.
                                    let _ = thread::Builder::new()
                                        .name("bazza-service-microphone-error".into())
                                        .spawn(move || {
                                            let capture = ServiceCapture(owner);
                                            let _ = capture.stop_matching(Some(id));
                                            if let Some(app) = desktop {
                                                if let Some(runtime) = app.try_state::<crate::state::RuntimeState>() {
                                                    crate::live_feed::LiveFeed::end_for(&runtime, &id.to_string());
                                                }
                                                capture.publish_desktop_state(&app, true);
                                            }
                                        });
                                }
                                break;
                            }
                            CaptureEvent::Levels { mic_db, sys_db } => {
                                if let Some(app) = desktop.as_ref() {
                                    let _ =
                                        app.emit(events::LEVELS, LevelsPayload { mic_db, sys_db });
                                }
                            }
                            CaptureEvent::Stalled { mic, sys } => {
                                if let Some(owner) = owner.upgrade() {
                                    let _ =
                                        ServiceCapture(owner).control_active(Some(id), |active| {
                                            active.reopen_audio(mic, sys);
                                            Ok(())
                                        });
                                }
                            }
                            notice @ (CaptureEvent::Warning(_) | CaptureEvent::Error(_) | CaptureEvent::AudioUnavailable { .. }
                                | CaptureEvent::AudioReconnected { .. }) => crate::recorder::report_audio_notice(desktop.as_ref(), notice),
                            CaptureEvent::Pcm16k(pcm) => {
                                match caption_input.as_mut() {
                                    Some(CaptionInput::Soniox(control)) => { control.try_send_pcm(pcm); }
                                    Some(CaptionInput::Local(input)) => { let _ = input.send(pcm); }
                                    _ => {}
                                }
                            }
                            CaptureEvent::SystemPcm16k(_) => {}
                        }
                    }
                })
        };
        let drain = match drain {
            Ok(drain) => drain,
            Err(_) => {
                return self.finish(PendingCapture::Finish(reservation.complete(
                    Err(()),
                    None,
                    None,
                )))
            }
        };
        let directory = reservation
            .guard
            .as_ref()
            .expect("Begin holds artifact guard")
            .lock()
            .directory()
            .to_path_buf();
        let config = audio.config(&directory, settings);
        let bot_only = bot_provider.is_some();
        let recorder = bot_provider.map_or(Ok(()), |provider| {
            crate::bot_connection::persist_live_policy(&directory, provider, config.capture_system).map_err(|_| {
                CaptureError::new("native.captureUnavailable", "Could not save the bot recording source policy.", false)
            })
        }).and_then(|()| Recorder::start(config, events_tx).map_err(|_| CaptureError::writer_failed()));
        match recorder {
            Ok(recorder) => {
                let meeting_id = id.to_string();
                let mut participant_live = desktop.as_ref().filter(|_| bot_only).map(|app| {
                    crate::participant_live::ParticipantSession::new(app, &meeting_id)
                });
                let named_input = participant_live.as_ref().map(|session| session.sender());
                let origin = recorder.time_origin();
                let (discord, attendee) = match (&desktop, prepared_bot) {
                    (Some(app), Some(prepared)) => {
                        match prepared.adopt(&meeting_id, &directory, origin, named_input) {
                            Ok(recordings) => recordings,
                            Err(_) => {
                                // A disconnect racing device startup cannot turn
                                // this into an ordinary mixed recording.
                                let _ = recorder.stop();
                                drop(forwarder_ready);
                                drain_stop.store(true, Ordering::Release);
                                let _ = drain.join();
                                if let Some(runtime) = app.try_state::<RuntimeState>() {
                                    crate::live_feed::LiveFeed::end_for(&runtime, &meeting_id);
                                }
                                let result = self.finish(PendingCapture::Finish(reservation.complete(Err(()), None, None)));
                                if let Err(error) = result {
                                    if error.code == "CAPTURE_PENDING" { return Err(error); }
                                }
                                return Err(CaptureError::new("setup.botConnectionLost", "The bot connection is unavailable. Available recording files were kept.", false));
                            }
                        }
                    }
                    (Some(app), None) => {
                        crate::bot_connection::cancel_pending(app);
                        let discord = discord.map(|selection| {
                            crate::discord_recording::DiscordRecording::start(app, &meeting_id, &directory, selection, origin, named_input.clone())
                        });
                        let attendee = attendee.map(|selection| {
                            crate::attendee_recording::AttendeeRecording::start(app, &meeting_id, &directory, selection, origin, named_input)
                        });
                        (discord, attendee)
                    }
                    (None, _) => (None, None),
                };
                let live_journal = desktop.as_ref().and_then(|app| {
                    let runtime = app.try_state::<RuntimeState>()?;
                    if soniox || !local_live_supported {
                        crate::live_feed::LiveFeed::begin(&runtime, &meeting_id);
                        return None;
                    }
                    let journal = Arc::new(LocalCaptionJournal {
                        artifact: Arc::downgrade(
                            reservation
                                .guard
                                .as_ref()
                                .expect("Begin holds artifact guard"),
                        ),
                        provenance: ProviderProvenance {
                            provider: SttProvider::Local,
                            model_id: reservation.request.start.provider.model.clone(),
                        },
                        text_bytes: Mutex::new(Some(0)),
                        warned: AtomicBool::new(false),
                    });
                    let sink = Arc::clone(&journal);
                    let app = app.clone();
                    crate::live_feed::LiveFeed::begin_with_confirmed(
                        &runtime,
                        &meeting_id,
                        Arc::new(move |caption, speaker| sink.append(&app, caption, speaker)),
                    );
                    Some(journal)
                });
                let screen = video_target
                    .as_ref()
                    .map(|target| ScreenCapture::start(&directory, settings, ffmpeg, target));
                if let Some(live) = &mut participant_live { live.start(settings); }
                let mut transcription = if soniox {
                    if permission_supported {
                        self.start_soniox(client, &reservation, desktop)
                    } else {
                        CaptureTranscription::Unavailable(
                            SonioxIncompleteReason::ControlUnavailable,
                        )
                    }
                } else {
                    CaptureTranscription::Local(desktop.as_ref().filter(|_| !bot_only).and_then(|app| {
                        let runtime = app.try_state::<crate::state::RuntimeState>()?;
                        let id = reservation.lease.as_ref()?.meeting_id.to_string();
                        Some(crate::stt_stage::start_local_live(app, &runtime, &id, settings))
                    }))
                };
                let caption_input = transcription.take_input();
                *self.0.phase.lock() = Phase::Recording(Box::new(ActiveCapture {
                    recorder: Some(recorder),
                    discord,
                    attendee,
                    screen,
                    drain_stop,
                    drain: Some(drain),
                    transcription,
                    participant_live,
                    live_journal,
                    reservation: Some(reservation),
                }));
                if let Err(error) = forwarder_ready.send(caption_input) {
                    if let Some(CaptionInput::Soniox(control)) = error.0 {
                        control.control_lost();
                    }
                }
                Ok(self.view())
            }
            Err(start_error) => {
                drop(forwarder_ready);
                drain_stop.store(true, Ordering::Release);
                let _ = drain.join();
                let result = self.finish(PendingCapture::Finish(reservation.complete(
                    Err(()),
                    None,
                    None,
                )));
                match result {
                    Err(error) if error.code == "CAPTURE_PENDING" => Err(error),
                    _ => Err(start_error),
                }
            }
        }
    }

    fn start_soniox(
        &self,
        mut client: ServiceClient,
        reservation: &Reservation,
        desktop: Option<AppHandle>,
    ) -> CaptureTranscription {
        let Some(lease) = reservation.lease.as_ref() else {
            return CaptureTranscription::Unavailable(SonioxIncompleteReason::ControlUnavailable);
        };
        let Some(guard) = reservation.guard.as_ref().cloned() else {
            return CaptureTranscription::Unavailable(SonioxIncompleteReason::ControlUnavailable);
        };
        let request = SonioxPermissionRequest {
            proof: reservation.request.proof.clone(),
            lease: lease.clone(),
            fence: reservation.request.start.fence.clone(),
            operation: SonioxPermissionOperation::Issue {},
        };
        let snapshot = match async_runtime::block_on(client.soniox_permission(request.clone())) {
            Ok(snapshot) => snapshot,
            Err(error) => return CaptureTranscription::Unavailable(issue_error_reason(error)),
        };
        let SonioxPermissionReply::Issued { permit, credential } = snapshot.reply else {
            return CaptureTranscription::Unavailable(SonioxIncompleteReason::ControlUnavailable);
        };
        let permission_epoch = permit.permission_epoch;
        let ack = SonioxPermissionRequest {
            operation: SonioxPermissionOperation::AckDrained { permission_epoch },
            ..request.clone()
        };
        let identity = guard.lock().identity().clone();
        let selected = &reservation.request.start.provider;
        let Some(permit) = snapshot
            .expires_at
            .and_then(|expires_at| native_soniox_permit(permit, expires_at, &identity, selected))
        else {
            let _ = async_runtime::block_on(client.soniox_permission(ack));
            return CaptureTranscription::Unavailable(SonioxIncompleteReason::ControlUnavailable);
        };
        let binding = permit.binding.clone();
        let config = SonioxConfig {
            api_key: credential.expose().to_owned(),
            model: binding.model.clone(),
            language: binding.language.clone(),
        };
        let (session_tx, session_rx) = crossbeam_channel::bounded::<SonioxCaptureSession>(1);
        let (renew_stop, wait_for_drain) = crossbeam_channel::bounded(1);
        let stop_after_drain = renew_stop.clone();
        let root = self.0.root.clone();
        let managed = self.0.managed;
        let drained = match thread::Builder::new()
            .name("bazza-service-soniox-drain".into())
            .spawn(move || {
                let outcome = session_rx
                    .recv()
                    .ok()
                    .and_then(|session| session.join().outcome.ok())
                    .unwrap_or(SonioxCaptureOutcome::Incomplete {
                        reason: SonioxIncompleteReason::FinalizationInterrupted,
                    });
                let _ = stop_after_drain.try_send(());
                // The network and confirmed journal are both drained. The
                // artifact owner is still held by the local recording.
                let _ = async_runtime::block_on(async {
                    let mut client = if managed {
                        ServiceClient::connect_managed(&root).await?
                    } else {
                        ServiceClient::connect(&root).await?
                    };
                    client.soniox_permission(ack).await
                });
                outcome
            }) {
            Ok(drained) => drained,
            Err(_) => {
                let ack = SonioxPermissionRequest {
                    operation: SonioxPermissionOperation::AckDrained { permission_epoch },
                    ..request
                };
                let _ = async_runtime::block_on(client.soniox_permission(ack));
                return CaptureTranscription::Unavailable(
                    SonioxIncompleteReason::ControlUnavailable,
                );
            }
        };
        let meeting_id = lease.meeting_id.to_string();
        let session = match SonioxCaptureSession::start(guard, permit, config, move |caption| {
            if let Some(app) = desktop.as_ref() {
                if let Some(runtime) = app.try_state::<crate::state::RuntimeState>() {
                    crate::live_feed::LiveFeed::push_for(&runtime, &meeting_id, caption);
                }
                let _ = app.emit(events::CAPTION, caption);
            }
        }) {
            Ok(session) => session,
            Err(_) => {
                drop(session_tx);
                let _ = drained.join();
                return CaptureTranscription::Unavailable(
                    SonioxIncompleteReason::ControlUnavailable,
                );
            }
        };
        let control = session.control();
        if let Err(error) = session_tx.send(session) {
            control.control_lost();
            let _ = error.0.finish();
            let _ = drained.join();
            return CaptureTranscription::Unavailable(SonioxIncompleteReason::ControlUnavailable);
        }

        let renew = SonioxPermissionRequest {
            operation: SonioxPermissionOperation::Renew { permission_epoch },
            ..request
        };
        let renew_control = control.clone();
        let selection = selected.clone();
        let renewal = thread::Builder::new()
            .name("bazza-service-soniox-renew".into())
            .spawn(move || {
                renew_soniox(
                    &mut client,
                    renew,
                    binding,
                    selection,
                    renew_control,
                    wait_for_drain,
                );
            })
            .ok();
        if renewal.is_none() {
            control.control_lost();
        }
        CaptureTranscription::Soniox(SonioxHandoff {
            control,
            renew_stop,
            renewal,
            drained,
        })
    }

    pub(crate) fn change_pause(&self, paused: bool) -> Result<FixtureCaptureView, CaptureError> {
        self.control_active(None, |active| active.set_paused(paused))
    }

    fn control_active(
        &self,
        expected: Option<Uuid>,
        action: impl FnOnce(&mut ActiveCapture) -> Result<(), CaptureError>,
    ) -> Result<FixtureCaptureView, CaptureError> {
        // The event drain must never wait for Stop, which joins that drain.
        // The watchdog reports another stall if an operation is in progress.
        let _operation = self.0.operation.try_lock().ok_or_else(CaptureError::busy)?;
        let mut active = {
            let mut phase = self.0.phase.lock();
            let view = match &*phase {
                Phase::Idle(view) if expected.is_none() => return Ok(view.clone()),
                Phase::Recording(active)
                    if expected.is_none_or(|meeting| active.meeting_id() == meeting) =>
                {
                    active.status()
                }
                _ => return Err(CaptureError::busy()),
            };
            let Phase::Recording(active) = std::mem::replace(&mut *phase, Phase::Working(view))
            else {
                unreachable!("an active capture was checked while locked");
            };
            active
        };
        // Timeline writes and device reopen can block. Serialize with Stop
        // while allowing the status command to return the previous snapshot.
        let result = action(&mut active);
        let view = active.status();
        *self.0.phase.lock() = Phase::Recording(active);
        // The writer may already have changed state when the final write fails.
        result.map(|()| view)
    }

    fn stop_matching(&self, expected: Option<Uuid>) -> Result<FixtureCaptureView, CaptureError> {
        let _operation = self.0.operation.lock();
        let previous = {
            let mut phase = self.0.phase.lock();
            let view = match &*phase {
                Phase::Idle(view) => return Ok(view.clone()),
                Phase::Working(_) => return Err(CaptureError::busy()),
                Phase::Recording(active) => active.view(CaptureState::Finishing),
                Phase::Pending(pending) => pending.view(CaptureState::Finishing),
            };
            if expected.is_some()
                && (!matches!(*phase, Phase::Recording(_)) || view.meeting_id != expected)
            {
                return Err(CaptureError::busy());
            }
            std::mem::replace(&mut *phase, Phase::Working(view))
        };
        let pending = match previous {
            Phase::Recording(active) => PendingCapture::Finish(active.stop()),
            Phase::Pending(pending) => *pending,
            _ => unreachable!("only an active capture or pending terminal can stop"),
        };
        self.finish(pending)
    }

    fn finish(&self, mut pending: PendingCapture) -> Result<FixtureCaptureView, CaptureError> {
        if let PendingCapture::Start(reservation) = &mut pending {
            match reservation.cancel(self) {
                Ok(Some(completion)) => pending = PendingCapture::Finish(completion),
                Ok(None) => {
                    let view = FixtureCaptureView::new(CaptureState::Idle, None, 0);
                    *self.0.phase.lock() = Phase::Idle(view.clone());
                    return Ok(view);
                }
                Err(_) => {
                    *self.0.phase.lock() = Phase::Pending(Box::new(pending));
                    return Err(CaptureError::pending());
                }
            }
        }
        let PendingCapture::Finish(completion) = &mut pending else {
            unreachable!("an unresolved reservation has already returned");
        };
        match completion.commit(self) {
            Ok(view) => {
                *self.0.phase.lock() = Phase::Idle(view.clone());
                if completion.manifest.status == CaptureTerminalStatus::Failed {
                    Err(CaptureError::writer_failed())
                } else {
                    Ok(view)
                }
            }
            Err(_) => {
                *self.0.phase.lock() = Phase::Pending(Box::new(pending));
                Err(CaptureError::pending())
            }
        }
    }
}

fn issue_error_reason(error: ClientError) -> SonioxIncompleteReason {
    match error {
        ClientError::Remote(RpcError {
            code: ErrorCode::PermissionDenied,
            ..
        }) => SonioxIncompleteReason::PermissionRevoked,
        _ => SonioxIncompleteReason::ControlUnavailable,
    }
}

fn native_soniox_permit(
    permit: SonioxPermissionPermit,
    expires_at: Instant,
    identity: &CaptureIdentity,
    selection: &CaptureSelection,
) -> Option<SonioxCapturePermit> {
    if permit.meeting_id != identity.meeting_id
        || permit.worker_session_id != identity.worker_session_id
        || permit.generation != identity.generation
        || selection.provider != CaptureProvider::Soniox
        || selection.model.as_deref() != Some(permit.model.as_str())
        || selection.language != permit.language
        || expires_at <= Instant::now()
    {
        return None;
    }
    Some(SonioxCapturePermit {
        binding: SonioxPermissionBinding {
            capture_id: permit.capture_id,
            artifact: identity.clone(),
            service_epoch: permit.library_epoch,
            permission_epoch: permit.permission_epoch,
            model: permit.model,
            language: permit.language,
        },
        expires_at,
    })
}

fn renew_soniox(
    client: &mut ServiceClient,
    renew: SonioxPermissionRequest,
    binding: SonioxPermissionBinding,
    selection: CaptureSelection,
    control: SonioxCaptureControl,
    stop: Receiver<()>,
) {
    let mut worker_renewed_at = Instant::now();
    while matches!(
        stop.recv_timeout(SONIOX_RENEW_INTERVAL),
        Err(RecvTimeoutError::Timeout)
    ) {
        if worker_renewed_at.elapsed() >= WORKER_RENEW_INTERVAL {
            if async_runtime::block_on(client.renew_worker(RenewWorkerRequest {
                proof: renew.proof.clone(),
                ttl_seconds: REGISTRATION_TTL_SECONDS,
            }))
            .is_err()
            {
                control.control_lost();
                return;
            }
            worker_renewed_at = Instant::now();
        }
        let Ok(snapshot) = async_runtime::block_on(client.soniox_permission(renew.clone())) else {
            control.control_lost();
            return;
        };
        match snapshot.reply {
            SonioxPermissionReply::Renewed { permit } => {
                let accepted = snapshot
                    .expires_at
                    .and_then(|expires_at| {
                        native_soniox_permit(permit, expires_at, &binding.artifact, &selection)
                    })
                    .is_some_and(|permit| control.renew(permit));
                if !accepted {
                    control.control_lost();
                    return;
                }
            }
            SonioxPermissionReply::Stop {
                reason: SonioxIncompleteReason::PermissionRevoked,
                ..
            } => {
                control.revoke(&binding);
                return;
            }
            _ => {
                control.control_lost();
                return;
            }
        }
    }
}

impl CaptureTranscription {
    fn take_input(&mut self) -> Option<CaptionInput> {
        match self {
            Self::Soniox(handoff) => Some(CaptionInput::Soniox(handoff.control.clone())),
            Self::Local(Some(session)) => session.input.take().map(CaptionInput::Local),
            _ => None,
        }
    }

    fn finish(self) -> Option<SonioxCaptureOutcome> {
        match self {
            Self::Local(mut session) => {
                if let Some(session) = session.as_mut() {
                    drop(session.input.take());
                    if let Some(worker) = session.worker.take() {
                        worker.join();
                    }
                    if let Some(captions) = session.captions.take() {
                        let _ = captions.join();
                    }
                }
                None
            }
            Self::Unavailable(reason) => Some(SonioxCaptureOutcome::Incomplete { reason }),
            Self::Soniox(handoff) => {
                handoff.control.close_input();
                let outcome = handoff
                    .drained
                    .join()
                    .unwrap_or(SonioxCaptureOutcome::Incomplete {
                        reason: SonioxIncompleteReason::FinalizationInterrupted,
                    });
                let _ = handoff.renew_stop.try_send(());
                if let Some(renewal) = handoff.renewal {
                    let _ = renewal.join();
                }
                Some(outcome)
            }
        }
    }
}

impl LocalCaptionJournal {
    fn append(&self, app: &AppHandle, caption: &Caption, speaker: Option<&str>) -> bool {
        let result = {
            let mut text_bytes = self.text_bytes.lock();
            let (Some(text_bytes), Some(artifact)) = (text_bytes.as_mut(), self.artifact.upgrade())
            else {
                return false;
            };
            let mut guard = artifact.lock();
            append_live_caption(&mut guard, &self.provenance, text_bytes, caption, speaker)
        };
        if let Err(error) = result {
            if !self.warned.swap(true, Ordering::AcqRel) {
                crate::recorder::notice_message(
                    app,
                    "warning",
                    crate::ui_message::app(
                        "native.transcriptSaveFailed",
                        "A live caption could not be saved. Recording continues.",
                    )
                    .with_detail(error.to_string()),
                );
            }
            return false;
        }
        true
    }

    fn close(&self) {
        // Also waits for any append that started just before the named worker stopped.
        *self.text_bytes.lock() = None;
    }
}

fn append_live_caption(
    guard: &mut ArtifactGuard,
    provenance: &ProviderProvenance,
    text_bytes: &mut usize,
    caption: &Caption,
    speaker: Option<&str>,
) -> Result<(), HandoffError> {
    if guard.last_sequence() >= MAX_CAPTURE_CAPTIONS {
        return Err(HandoffError::TooLarge);
    }
    let record = ConfirmedCaption {
        sequence: guard.last_sequence() + 1,
        start_ms: u64::try_from(caption.start_ms).map_err(|_| HandoffError::Invalid)?,
        end_ms: u64::try_from(caption.end_ms).map_err(|_| HandoffError::Invalid)?,
        speaker: speaker.map(str::to_owned),
        text: caption.text.clone(),
        provenance: provenance.clone(),
    };
    // The journal row includes extra provenance bytes beyond the service row.
    // Counting it as JSON also covers escaping and reserves the seal envelope.
    let bytes = *text_bytes
        + serde_json::to_vec(&record)
            .map_err(|_| HandoffError::Invalid)?
            .len()
        + 1;
    if bytes > MAX_CAPTURE_TERMINAL_BYTES - bazza_capture_handoff::MAX_MANIFEST_BYTES {
        return Err(HandoffError::TooLarge);
    }
    guard
        .append_local_live_confirmed(&record)
        .or_else(|error| match error {
            HandoffError::Io(_) => guard.append_local_live_confirmed(&record),
            error => Err(error),
        })?;
    *text_bytes = bytes;
    Ok(())
}

impl ActiveCapture {
    fn meeting_id(&self) -> Uuid {
        self.reservation
            .as_ref()
            .expect("active capture owns reservation")
            .meeting_id()
    }

    fn elapsed_ms(&self) -> u64 {
        self.recorder
            .as_ref()
            .expect("active capture owns recorder")
            .elapsed()
            .as_millis() as u64
    }

    fn view(&self, state: CaptureState) -> FixtureCaptureView {
        FixtureCaptureView::new(state, Some(self.meeting_id()), self.elapsed_ms())
            .with_screen(self.screen.as_ref().map(ScreenCapture::state))
    }

    fn status(&self) -> FixtureCaptureView {
        let recorder = self
            .recorder
            .as_ref()
            .expect("active capture owns recorder");
        self.view(if recorder.is_paused() {
            CaptureState::Paused
        } else {
            CaptureState::Recording
        })
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), CaptureError> {
        let recorder = self
            .recorder
            .as_ref()
            .expect("active capture owns recorder");
        let video = self
            .screen
            .as_mut()
            .and_then(|screen| screen.recorder.as_mut());
        let result = crate::recorder::set_capture_paused(recorder, video, paused);
        if let Some(live) = &self.participant_live {
            live.set_paused(recorder.is_paused());
        }
        if let Some(discord) = &self.discord {
            discord.set_paused(recorder.is_paused());
        }
        if let Some(attendee) = &self.attendee {
            attendee.set_paused(recorder.is_paused());
        }
        result.map_err(|_| {
            CaptureError::new(
                "VIDEO_TIMELINE_UNAVAILABLE",
                "The screen timing update could not be saved. Check the current capture state.",
                true,
            )
        })
    }

    fn reopen_audio(&mut self, mic: bool, sys: bool) {
        let recorder = self
            .recorder
            .as_mut()
            .expect("active capture owns recorder");
        recorder.reopen_stalled(mic, sys);
    }

    fn close_writers(
        &mut self,
        finish_named: bool,
    ) -> (
        Result<RecordOutput, ()>,
        Option<RawScreenWriterOutcome>,
        Option<SonioxCaptureOutcome>,
    ) {
        if !finish_named {
            drop(self.participant_live.take());
        }
        if let Some(discord) = &self.discord {
            discord.request_stop();
        }
        if let Some(attendee) = &self.attendee {
            attendee.request_stop();
        }
        let output = self
            .recorder
            .take()
            .ok_or(())
            .and_then(|recorder| recorder.stop().map_err(|_| ()));
        // Stop the optional screen writer even if the audio writer failed.
        // The reservation still owns the artifact lock throughout shutdown.
        let screen = self.screen.take().map(ScreenCapture::stop);
        self.drain_stop.store(true, Ordering::Release);
        if let Some(drain) = self.drain.take() {
            if drain.thread().id() != thread::current().id() {
                let _ = drain.join();
            }
        }
        // The final PCM is already forwarded. Wait for the network and the
        // journal without holding their shared artifact mutex.
        let soniox_outcome =
            std::mem::replace(&mut self.transcription, CaptureTranscription::Local(None)).finish();
        if let Some(discord) = self.discord.take() {
            discord.finish();
        }
        if let Some(attendee) = self.attendee.take() {
            attendee.finish();
        }
        if let Some(live) = self.participant_live.take() {
            live.finish();
        }
        if let Some(journal) = self.live_journal.take() {
            journal.close();
        }
        (output, screen, soniox_outcome)
    }

    fn stop(mut self) -> Completion {
        let (output, screen, soniox_outcome) = self.close_writers(true);
        self.reservation
            .take()
            .expect("active capture owns reservation")
            .complete(output, screen, soniox_outcome)
    }
}

impl Drop for ActiveCapture {
    fn drop(&mut self) {
        if self.recorder.is_some()
            || self.screen.is_some()
            || matches!(self.transcription, CaptureTranscription::Soniox(_))
        {
            let (output, screen, soniox_outcome) = self.close_writers(false);
            if let Some(reservation) = self.reservation.take() {
                // An unexpected native teardown still drains the writer before
                // releasing the lock. The service can recover a durable seal.
                let _ = reservation.complete(output, screen, soniox_outcome).seal();
            }
        }
    }
}

impl ScreenCapture {
    fn start(
        directory: &Path,
        settings: &Settings,
        ffmpeg: Option<&Path>,
        target: &MonitorInfo,
    ) -> Self {
        let recorder = ffmpeg.and_then(|ffmpeg| {
            crate::recorder::start_screen_writer(ffmpeg, directory, settings, target).ok()
        });
        if recorder.is_none() {
            log::warn!("Fixture screen capture could not start. Microphone capture continues.");
        }
        Self { recorder }
    }

    fn state(&self) -> ScreenState {
        if self.recorder.is_some() {
            ScreenState::Recording
        } else {
            ScreenState::Failed
        }
    }

    fn stop(self) -> RawScreenWriterOutcome {
        let Some(recorder) = self.recorder else {
            return RawScreenWriterOutcome::Failed;
        };
        if recorder.stop().is_ok() {
            RawScreenWriterOutcome::Stopped
        } else {
            log::warn!("Fixture screen capture did not finish cleanly. Its files were kept.");
            RawScreenWriterOutcome::Failed
        }
    }
}

impl PendingCapture {
    fn view(&self, state: CaptureState) -> FixtureCaptureView {
        match self {
            Self::Start(reservation) => {
                FixtureCaptureView::new(state, Some(reservation.meeting_id()), 0)
            }
            Self::Finish(completion) => completion.view(state),
        }
    }
}

impl Reservation {
    fn meeting_id(&self) -> Uuid {
        self.request.start.meeting_id
    }

    fn acquire(&self, owner: &ServiceCapture) -> Result<SharedArtifact, CaptureError> {
        let lease = self.lease.as_ref().ok_or_else(CaptureError::pending)?;
        let directory = owner
            .0
            .root
            .join("recordings")
            .join(lease.meeting_id.to_string());
        let identity = CaptureIdentity {
            library_id: owner.0.library_id,
            meeting_id: lease.meeting_id,
            worker_session_id: lease.worker_session_id,
            generation: lease.generation,
        };
        ArtifactGuard::acquire(&directory, &identity)
            .map(|guard| Arc::new(Mutex::new(guard)))
            .map_err(|_| CaptureError::unavailable())
    }

    fn begin(
        &mut self,
        owner: &ServiceCapture,
        client: &mut ServiceClient,
        epoch: Uuid,
    ) -> Result<(), CaptureError> {
        if self.lease.is_none() {
            let result = async_runtime::block_on(client.prepare_capture(self.request.clone()));
            self.prepare_replied = matches!(
                &result,
                Ok(_)
                    | Err(ClientError::Remote(RpcError {
                        code: ErrorCode::ResourceBusy
                            | ErrorCode::PermissionDenied
                            | ErrorCode::InvalidArgument
                            | ErrorCode::IdempotencyConflict,
                        ..
                    }))
            );
            self.lease = Some(result.map_err(|_| CaptureError::unavailable())?);
        }
        if self.guard.is_none() {
            self.guard = Some(self.acquire(owner)?);
        }
        let current = self.lease.as_ref().ok_or_else(CaptureError::pending)?;
        if current.library_epoch != epoch {
            let fresh = async_runtime::block_on(client.reattach_capture(ReattachCaptureRequest {
                proof: self.request.proof.clone(),
                capture_id: current.capture_id,
                fence: self.request.start.fence.clone(),
                ttl_seconds: REGISTRATION_TTL_SECONDS,
            }))
            .map_err(|_| CaptureError::pending())?;
            let mut expected = current.clone();
            expected.library_epoch = epoch;
            if fresh != expected {
                return Err(CaptureError::pending());
            }
            self.request.proof.library_epoch = epoch;
            self.lease = Some(fresh);
        }
        let lease = self.lease.clone().ok_or_else(CaptureError::pending)?;
        self.begin_sent = true;
        async_runtime::block_on(client.begin_capture(BeginCaptureRequest {
            proof: self.request.proof.clone(),
            lease,
            fence: self.request.start.fence.clone(),
        }))
        .map_err(|_| CaptureError::pending())?;
        Ok(())
    }

    /// A lost Prepare or Begin reply never permits opening the microphone.
    /// Cancel a pristine reservation, or confirm an already attempted Begin
    /// under the lock and close it as a failed empty recording.
    fn cancel(&mut self, owner: &ServiceCapture) -> Result<Option<Completion>, CaptureError> {
        let (mut client, status) = owner.connect()?;
        let epoch = status.library_epoch;
        if self.lease.is_none() {
            let same_epoch = self.request.proof.library_epoch == epoch;
            if self.prepare_replied || !same_epoch {
                let meeting = async_runtime::block_on(client.meeting(self.meeting_id()))
                    .map_err(|_| CaptureError::pending())?;
                if meeting.is_none_or(|meeting| meeting.status == "failed") {
                    return Ok(None);
                }
            }
            if !same_epoch {
                return Err(CaptureError::pending());
            }
            // In this epoch a timed-out Prepare may still be about to commit.
            // Replay its exact identity and fence before reasoning about the
            // row. Once Abort commits, a late original Prepare cannot reopen it.
            self.lease = Some(
                async_runtime::block_on(client.prepare_capture(self.request.clone()))
                    .map_err(|_| CaptureError::pending())?,
            );
        }
        if self.begin_sent {
            // Begin may have committed before its reply was lost. Keep the
            // guard while replaying it and sealing the empty failed capture.
            // Releasing it here would admit interrupted-capture recovery.
            self.begin(owner, &mut client, epoch)?;
            return Ok(Some(self.take_completion(Err(()), None, None)));
        }
        drop(self.guard.take());
        let lease = self.lease.clone().ok_or_else(CaptureError::pending)?;
        async_runtime::block_on(client.abort_capture(AbortCaptureRequest {
            proof: self.request.proof.clone(),
            lease,
            fence: self.request.start.fence.clone(),
        }))
        .map_err(|_| CaptureError::pending())?;
        Ok(None)
    }

    fn complete(
        mut self,
        audio: Result<RecordOutput, ()>,
        screen: Option<RawScreenWriterOutcome>,
        soniox_outcome: Option<SonioxCaptureOutcome>,
    ) -> Completion {
        self.take_completion(audio, screen, soniox_outcome)
    }

    fn take_completion(
        &mut self,
        audio: Result<RecordOutput, ()>,
        screen: Option<RawScreenWriterOutcome>,
        soniox_outcome: Option<SonioxCaptureOutcome>,
    ) -> Completion {
        let guard = self
            .guard
            .take()
            .expect("confirmed Begin owns the artifact lock");
        let provider = &self.request.start.provider;
        let manifest = terminal(&guard.lock(), audio, provider);
        let soniox_outcome = (provider.provider == CaptureProvider::Soniox).then(|| {
            soniox_outcome.unwrap_or(SonioxCaptureOutcome::Incomplete {
                reason: SonioxIncompleteReason::FinalizationInterrupted,
            })
        });
        Completion {
            request: FinalizeCaptureRequest {
                proof: self.request.proof.clone(),
                lease: self
                    .lease
                    .clone()
                    .expect("confirmed Begin carries the capture lease"),
                fence: self.request.start.fence.clone(),
            },
            guard: Some(guard),
            manifest,
            screen,
            soniox_outcome,
        }
    }
}

impl Completion {
    fn view(&self, state: CaptureState) -> FixtureCaptureView {
        FixtureCaptureView::new(
            state,
            Some(self.request.lease.meeting_id),
            self.manifest.duration_ms,
        )
        .with_screen(self.screen.map(ScreenState::from))
    }

    fn seal(&mut self) -> Result<(), CaptureError> {
        if let Some(guard) = self.guard.as_ref() {
            {
                let mut guard = guard.lock();
                if self.manifest.provenance.provider == SttProvider::Local
                    && self.manifest.last_confirmed_sequence > 0
                {
                    guard.publish_terminal_with_local_live(&self.manifest, self.screen)
                } else if self.screen.is_some() || self.soniox_outcome.is_some() {
                    guard.publish_terminal_with_evidence(
                        &self.manifest,
                        self.screen,
                        self.soniox_outcome,
                    )
                } else {
                    guard.publish_terminal(&self.manifest)
                }
                .map_err(|_| CaptureError::pending())?;
            }
            drop(self.guard.take());
        }
        Ok(())
    }

    fn commit(&mut self, owner: &ServiceCapture) -> Result<FixtureCaptureView, CaptureError> {
        self.seal()?;
        let (mut client, _) = owner.connect()?;
        // Reconnect does not adopt a new epoch into this request. A restarted
        // service consumes the seal and can replay the resulting receipt.
        let receipt = async_runtime::block_on(client.finalize_capture(self.request.clone()))
            .map_err(|_| CaptureError::pending())?;
        let mut view = self.view(CaptureState::Idle);
        view.job_id = receipt.job_id;
        Ok(view)
    }
}

fn fixture_audio_config(
    directory: &Path,
    settings: &Settings,
    use_configured_audio: bool,
) -> RecordConfig {
    if use_configured_audio {
        return crate::recorder::audio_writer_config(directory, settings);
    }
    RecordConfig {
        dir: directory.into(),
        mic_device: None,
        loopback_device: None,
        capture_system: false,
    }
}

fn native_meeting(meeting: DesktopMeeting) -> Result<Meeting, CaptureError> {
    let status = match meeting.status.as_str() {
        "recording" => MeetingStatus::Recording,
        "processing" => MeetingStatus::Processing,
        "ready" => MeetingStatus::Ready,
        "failed" => MeetingStatus::Failed,
        _ => return Err(CaptureError::unavailable()),
    };
    Ok(Meeting {
        id: meeting.id,
        title: meeting.title,
        app: meeting.app,
        started_at: meeting.started_at,
        ended_at: meeting.ended_at,
        duration_ms: meeting.duration_ms,
        has_video: meeting.has_video,
        status,
        folder_id: meeting.folder_id,
        created_at: meeting.created_at,
        error: meeting.error,
        title_auto: meeting.title_auto,
        participants: meeting.participants,
        calendar_uid: meeting.calendar_uid,
    })
}

fn fixture_provider() -> CaptureSelection {
    CaptureSelection {
        provider: CaptureProvider::Local,
        model: None,
        language: None,
    }
}

fn terminal(
    guard: &ArtifactGuard,
    audio: Result<RecordOutput, ()>,
    provider: &CaptureSelection,
) -> TerminalManifest {
    let wav_duration = mix_duration(&guard.directory().join("mix.wav"));
    let completed = audio.is_ok() && wav_duration.is_some();
    let media_duration = audio
        .map(|output| output.duration.as_millis() as u64)
        .ok()
        .filter(|_| completed)
        .or(wav_duration)
        .unwrap_or(0);
    // Keep confirmed timestamps unchanged when the WAV duration rounds down,
    // a provider clock trails the media clock, or a failed writer leaves partial media.
    let confirmed_end = guard
        .confirmed()
        .iter()
        .map(|caption| caption.end_ms)
        .max()
        .unwrap_or(0);
    let duration_ms = media_duration.max(confirmed_end);
    TerminalManifest {
        version: MANIFEST_VERSION,
        identity: guard.identity().clone(),
        provenance: ProviderProvenance {
            provider: match provider.provider {
                CaptureProvider::Local => SttProvider::Local,
                CaptureProvider::Soniox => SttProvider::Soniox,
            },
            model_id: provider.model.clone(),
        },
        duration_ms,
        ended_at: Utc::now(),
        status: if completed {
            CaptureTerminalStatus::Completed
        } else {
            CaptureTerminalStatus::Failed
        },
        error_code: (!completed).then(|| "MICROPHONE_CAPTURE_FAILED".into()),
        has_audio: wav_duration.is_some(),
        has_video: false,
        last_confirmed_sequence: guard.last_sequence(),
    }
}

fn mix_duration(path: &Path) -> Option<u64> {
    let metadata = path.symlink_metadata().ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return None;
        }
    }
    let reader = hound::WavReader::open(path).ok()?;
    Some(u64::from(reader.duration()) * 1_000 / u64::from(reader.spec().sample_rate.max(1)))
}

#[tauri::command]
pub(crate) async fn service_fixture_capture_start(
    state: State<'_, ServiceCapture>,
    runtime: State<'_, RuntimeState>,
    request: FixtureCaptureStartRequest,
) -> Result<FixtureCaptureView, CaptureError> {
    let state = state.inner().clone();
    let settings = runtime.settings.read().clone();
    let ffmpeg = runtime.ffmpeg.clone();
    async_runtime::spawn_blocking(move || state.start(request, &settings, ffmpeg.as_deref()))
        .await
        .map_err(|_| CaptureError::unavailable())?
}

#[tauri::command]
pub(crate) async fn service_fixture_capture_stop(
    state: State<'_, ServiceCapture>,
) -> Result<FixtureCaptureView, CaptureError> {
    let state = state.inner().clone();
    async_runtime::spawn_blocking(move || state.stop_matching(None))
        .await
        .map_err(|_| CaptureError::unavailable())?
}

#[tauri::command]
pub(crate) async fn service_fixture_capture_pause(
    state: State<'_, ServiceCapture>,
) -> Result<FixtureCaptureView, CaptureError> {
    let state = state.inner().clone();
    async_runtime::spawn_blocking(move || state.change_pause(true))
        .await
        .map_err(|_| CaptureError::unavailable())?
}

#[tauri::command]
pub(crate) async fn service_fixture_capture_resume(
    state: State<'_, ServiceCapture>,
) -> Result<FixtureCaptureView, CaptureError> {
    let state = state.inner().clone();
    async_runtime::spawn_blocking(move || state.change_pause(false))
        .await
        .map_err(|_| CaptureError::unavailable())?
}

#[tauri::command]
pub(crate) fn service_fixture_capture_status(
    state: State<'_, ServiceCapture>,
) -> FixtureCaptureView {
    state.view()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(root: &Path) -> (CaptureIdentity, ArtifactGuard) {
        let identity = CaptureIdentity {
            library_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            worker_session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        };
        let directory = root.join(identity.meeting_id.to_string());
        std::fs::create_dir(&directory).unwrap();
        let guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
        (identity, guard)
    }

    #[test]
    fn fixture_defaults_to_microphone_and_close_blocks_a_queued_start() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            capture_system: true,
            mic_device: Some("fixture microphone".into()),
            loopback_device: Some("fixture output".into()),
            ..Settings::default()
        };
        let config = fixture_audio_config(root.path(), &settings, false);
        assert!(!config.capture_system);
        assert!(config.mic_device.is_none());
        let request: FixtureCaptureStartRequest =
            serde_json::from_value(serde_json::json!({"title":null})).unwrap();
        assert!(!request.capture_screen);
        assert!(!request.use_configured_audio);
        let config = fixture_audio_config(root.path(), &settings, true);
        assert!(config.capture_system);
        assert_eq!(config.mic_device, settings.mic_device);
        assert_eq!(config.loopback_device, settings.loopback_device);
        let settings = Settings {
            capture_system: false,
            ..settings
        };
        assert!(!fixture_audio_config(root.path(), &settings, true).capture_system);
        let owner = ServiceCapture::new(root.path().into(), Uuid::new_v4());
        let operation = owner.0.operation.lock();
        assert!(!owner.may_close());
        drop(operation);
        assert!(owner.may_close());
        assert!(
            matches!(owner.start(request, &settings, None), Err(error) if error.code == "RESOURCE_BUSY")
        );
    }

    #[test]
    fn idle_pause_resume_is_inert_and_rejects_a_concurrent_operation() {
        let root = tempfile::tempdir().unwrap();
        let owner = ServiceCapture::new(root.path().into(), Uuid::new_v4());
        for paused in [true, false] {
            let view = owner.change_pause(paused).unwrap();
            assert!(matches!(view.state, CaptureState::Idle));
            assert_eq!(view.elapsed_ms, 0);
            assert!(view.meeting_id.is_none());
        }
        let operation = owner.0.operation.lock();
        assert!(matches!(
            owner.change_pause(true),
            Err(error) if error.code == "RESOURCE_BUSY"
        ));
        drop(operation);
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn desktop_plan_preserves_picker_options_and_selected_soniox() {
        let settings = Settings {
            stt_provider: bazza_asr::provider::SttProvider::Soniox,
            soniox_model: "stt-rt-v5".into(),
            language: Some("ru".into()),
            ..Settings::default()
        };
        let screen = MonitorInfo {
            index: 1,
            screen_id: "fixture-display".into(),
            name: "Display 2".into(),
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
            primary: false,
            device: 2,
        };
        let plan = CapturePlan::desktop(
            StartOptions {
                title: Some("Meeting".into()),
                app: Some("Call app".into()),
                video: Some(true),
                target: Some(bazza_capture::VideoTarget::Monitor { index: 1 }),
                screen_id: Some(screen.screen_id.clone()),
                discord: None,
                attendee: None,
                bot_connection_id: None,
            },
            &settings,
            None,
            Some(screen.clone()),
        );
        assert_eq!(plan.title, "Meeting");
        assert_eq!(plan.app.as_deref(), Some("Call app"));
        assert_eq!(plan.video_target, Some(screen));
        assert!(matches!(plan.audio, CaptureAudio::Desktop));
        assert_eq!(plan.provider.provider, CaptureProvider::Soniox);
        assert_eq!(plan.provider.model.as_deref(), Some("stt-rt-v5"));
        assert_eq!(plan.provider.language.as_deref(), Some("ru"));
    }

    #[test]
    fn managed_named_plan_persists_the_displayed_local_lane() {
        for selected in [bazza_asr::provider::SttProvider::Local, bazza_asr::provider::SttProvider::Soniox] {
            let settings = Settings { stt_provider: selected, ..Settings::default() };
            for (selection, expected) in [
                (serde_json::json!({"attendee": {"meetingUrl": "https://meet.google.com/abc-defg-hij"}}), "meet"),
                (serde_json::json!({"discord": {"guild_id": "1234", "channel_id": "5678"}}), "discord"),
            ] {
                let options = serde_json::from_value(selection).unwrap();
                let plan = CapturePlan::desktop(options, &settings, None, None);
                assert_eq!(plan.bot_provider, Some(expected));
                assert_eq!(plan.provider.provider, CaptureProvider::Local);
                assert_eq!(plan.provider.model, None);
            }
        }
    }

    #[test]
    fn local_live_journal_seals_exact_rows_and_preserves_the_last_caption_clock() {
        let root = tempfile::tempdir().unwrap();
        let (identity, mut guard) = artifact(root.path());
        let provider = fixture_provider();
        let provenance = ProviderProvenance {
            provider: SttProvider::Local,
            model_id: None,
        };
        let mut bytes = 0;
        let raw = "  Точная реплика 🦀 ";
        let caption = Caption {
            text: raw.into(),
            start_ms: 123,
            end_ms: 123,
            is_final: true,
        };
        append_live_caption(&mut guard, &provenance, &mut bytes, &caption, Some("Имя")).unwrap();
        let manifest = terminal(&guard, Err(()), &provider);
        assert_eq!(manifest.duration_ms, 123);
        guard
            .publish_terminal_with_local_live(&manifest, None)
            .unwrap();
        let dir = guard.directory().to_path_buf();
        drop(guard);
        let sealed = bazza_capture_handoff::SealedCaptureGuard::acquire(&dir, &identity).unwrap();
        assert!(sealed.retain_local_live());
        let mut rows = Vec::new();
        sealed
            .scan_confirmed::<HandoffError>(|row| {
                rows.push(row);
                Ok(())
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, raw);
        assert_eq!(rows[0].speaker.as_deref(), Some("Имя"));
        assert_eq!((rows[0].start_ms, rows[0].end_ms), (123, 123));
    }

    #[test]
    fn soniox_permit_rejects_stale_or_mismatched_capture() {
        let root = tempfile::tempdir().unwrap();
        let (identity, _guard) = artifact(root.path());
        let selection = CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("stt-rt-v5".into()),
            language: Some("ru".into()),
        };
        let permit = SonioxPermissionPermit {
            capture_id: Uuid::new_v4(),
            meeting_id: identity.meeting_id,
            worker_session_id: identity.worker_session_id,
            generation: identity.generation,
            library_epoch: Uuid::new_v4(),
            permission_epoch: Uuid::new_v4(),
            model: selection.model.clone().unwrap(),
            language: selection.language.clone(),
            valid_for_ms: 8_000,
        };
        let expires = Instant::now() + Duration::from_secs(5);
        let verified =
            native_soniox_permit(permit.clone(), expires, &identity, &selection).unwrap();
        assert_eq!(verified.binding.permission_epoch, permit.permission_epoch);
        assert_eq!(verified.binding.artifact, identity);
        assert!(
            native_soniox_permit(permit.clone(), Instant::now(), &identity, &selection).is_none()
        );
        let wrong_language = SonioxPermissionPermit {
            language: Some("en".into()),
            ..permit.clone()
        };
        assert!(native_soniox_permit(wrong_language, expires, &identity, &selection).is_none());
        let wrong_capture = SonioxPermissionPermit {
            generation: Uuid::new_v4(),
            ..permit
        };
        assert!(native_soniox_permit(wrong_capture, expires, &identity, &selection).is_none());
    }

    #[test]
    fn desktop_idle_status_hides_the_last_fixture_receipt() {
        let root = tempfile::tempdir().unwrap();
        let owner = ServiceCapture::new(root.path().into(), Uuid::new_v4());
        let meeting = Some(Uuid::new_v4());
        *owner.0.phase.lock() =
            Phase::Idle(FixtureCaptureView::new(CaptureState::Idle, meeting, 99));
        assert_eq!(owner.view().meeting_id, meeting);
        let status = owner.recording_state();
        assert_eq!(status.state, "idle");
        assert!(status.meeting_id.is_none());
        assert_eq!(status.elapsed_ms, 0);
    }

    fn test_wav(directory: &Path) -> PathBuf {
        let path = directory.join("mix.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 1_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for sample in [42_i16, 0, -42] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    fn completion(
        guard: ArtifactGuard,
        manifest: TerminalManifest,
        screen: Option<RawScreenWriterOutcome>,
    ) -> Completion {
        let library_epoch = Uuid::new_v4();
        let identity = guard.identity();
        Completion {
            request: FinalizeCaptureRequest {
                proof: WorkerProof {
                    session_id: identity.worker_session_id,
                    secret: WorkerSecret::random(),
                    library_epoch,
                },
                lease: CaptureLease {
                    capture_id: Uuid::new_v4(),
                    meeting_id: identity.meeting_id,
                    worker_session_id: identity.worker_session_id,
                    generation: identity.generation,
                    library_epoch,
                },
                fence: WorkerSecret::random(),
            },
            guard: Some(Arc::new(Mutex::new(guard))),
            manifest,
            screen,
            soniox_outcome: None,
        }
    }

    #[test]
    fn failed_writer_keeps_partial_wav_and_pending_stop_replays_the_same_seal() {
        let root = tempfile::tempdir().unwrap();
        let (identity, guard) = artifact(root.path());
        let directory = guard.directory().to_path_buf();
        let mix = test_wav(&directory);
        let bytes = std::fs::read(&mix).unwrap();
        let manifest = terminal(&guard, Err(()), &fixture_provider());
        assert_eq!(manifest.status, CaptureTerminalStatus::Failed);
        assert!(manifest.has_audio);
        assert_eq!(manifest.duration_ms, 3);
        let owner = ServiceCapture::new(root.path().into(), identity.library_id);
        let pending = PendingCapture::Finish(completion(guard, manifest.clone(), None));
        *owner.0.phase.lock() = Phase::Pending(Box::new(pending));
        assert!(!owner.may_close());
        // This root deliberately has no service marker or endpoint.
        for _ in 0..2 {
            assert!(
                matches!(owner.stop_matching(None), Err(error) if error.code == "CAPTURE_PENDING")
            );
            let view = owner.view();
            assert!(matches!(view.state, CaptureState::Pending));
            assert_eq!(view.meeting_id, Some(identity.meeting_id));
            assert!(view.job_id.is_none());
            assert_eq!(std::fs::read(&mix).unwrap(), bytes);
            assert_eq!(
                bazza_capture_handoff::read_terminal_snapshot(&directory, &identity).unwrap(),
                manifest
            );
        }
        assert!(
            owner.may_close(),
            "the persisted seal is available to recovery"
        );
    }

    #[test]
    fn optional_screen_start_failure_seals_successful_audio_and_keeps_timing() {
        let root = tempfile::tempdir().unwrap();
        let (identity, guard) = artifact(root.path());
        let directory = guard.directory().to_path_buf();
        let mix = test_wav(&directory);
        let audio = RecordOutput {
            mic_wav: directory.join("mic.wav"),
            sys_wav: None,
            mix_wav: mix,
            duration: Duration::from_millis(3),
        };
        let timeline = directory.join("screen.raw.timeline.json");
        let timing_bytes = b"partial timeline";
        std::fs::write(&timeline, timing_bytes).unwrap();
        let manifest = terminal(&guard, Ok(audio), &fixture_provider());
        let screen = ScreenCapture { recorder: None };
        assert!(matches!(screen.state(), ScreenState::Failed));
        let mut completion = completion(guard, manifest, Some(screen.stop()));
        assert_eq!(completion.manifest.status, CaptureTerminalStatus::Completed);
        assert!(!completion.manifest.has_video);
        completion.seal().unwrap();
        let snapshot =
            bazza_capture_handoff::read_terminal_snapshot_with_evidence(&directory, &identity)
                .unwrap();
        assert_eq!(snapshot.manifest, completion.manifest);
        let evidence = snapshot.raw_screen.unwrap();
        assert_eq!(evidence.writer, RawScreenWriterOutcome::Failed);
        assert_eq!(evidence.raw_mp4_bytes, None);
        assert_eq!(evidence.timeline_bytes, Some(timing_bytes.len() as u64));
        assert_eq!(std::fs::read(timeline).unwrap(), timing_bytes);
        assert!(!directory.join("screen.mp4").exists());
    }

    #[test]
    fn soniox_revocation_seals_audio_and_retains_the_last_caption_rounding_millisecond() {
        let root = tempfile::tempdir().unwrap();
        let (identity, mut guard) = artifact(root.path());
        let directory = guard.directory().to_path_buf();
        let mix_wav = test_wav(&directory);
        let bytes = std::fs::read(&mix_wav).unwrap();
        let provider = CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("stt-rt-v5".into()),
            language: None,
        };
        guard
            .append_confirmed(&bazza_capture_handoff::ConfirmedCaption {
                sequence: 1,
                start_ms: 0,
                end_ms: 4,
                speaker: None,
                text: "confirmed".into(),
                provenance: ProviderProvenance {
                    provider: SttProvider::Soniox,
                    model_id: provider.model.clone(),
                },
            })
            .unwrap();
        let manifest = terminal(
            &guard,
            Ok(RecordOutput {
                mic_wav: directory.join("mic.wav"),
                sys_wav: None,
                mix_wav: mix_wav.clone(),
                duration: Duration::from_millis(3),
            }),
            &provider,
        );
        let mut completion = completion(guard, manifest, None);
        completion.soniox_outcome =
            CaptureTranscription::Unavailable(SonioxIncompleteReason::PermissionRevoked).finish();
        completion.seal().unwrap();
        let snapshot =
            bazza_capture_handoff::read_terminal_snapshot_with_evidence(&directory, &identity)
                .unwrap();
        assert_eq!(snapshot.manifest.status, CaptureTerminalStatus::Completed);
        assert_eq!(snapshot.manifest.provenance.provider, SttProvider::Soniox);
        assert_eq!(snapshot.manifest.duration_ms, 4);
        assert_eq!(snapshot.manifest.last_confirmed_sequence, 1);
        assert!(snapshot.manifest.has_audio);
        assert!(snapshot.raw_screen.is_none());
        assert_eq!(
            snapshot.soniox_outcome,
            Some(SonioxCaptureOutcome::Incomplete {
                reason: SonioxIncompleteReason::PermissionRevoked,
            })
        );
        assert_eq!(std::fs::read(mix_wav).unwrap(), bytes);
    }
}

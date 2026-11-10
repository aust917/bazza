//! Per-participant Google Meet audio from an explicitly configured Attendee server.

mod api;
mod audit;
mod config;
mod ingress;
mod tracks;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch, Notify},
    task::{self, JoinHandle},
    time::{sleep, timeout},
};

use api::{Api, ApiError, Bot};
use ingress::{Clock, Ingress, Observation, Packet};
use tracks::{TrackError, Tracks};

pub use audit::{Audit, AuditLog};
pub use config::{normalize_meeting_url, Config, Error};
pub use tracks::{
    Manifest, SampleInterval, Timing, TrackCoverage, TrackInfo, MANIFEST_FILE, TRACK_DIRECTORY,
};

pub const SAMPLE_RATE: u32 = 16_000;
const QUEUE_CAPACITY: usize = 512;
const JOIN_TIMEOUT: Duration = Duration::from_secs(180);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(300);
const LEAVE_TIMEOUT: Duration = Duration::from_secs(12);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    WaitingAdmission,
    Connected,
    Waiting,
    Receiving,
    Failed,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub phase: Phase,
    pub message: Option<String>,
}

struct Reporter {
    current: Arc<Mutex<Status>>,
    on_status: Box<dyn Fn(Status) + Send + Sync>,
}

impl Reporter {
    fn set(&self, phase: Phase, message: Option<String>) {
        let next = Status { phase, message };
        {
            let mut status = self.current.lock();
            if *status == next || (status.phase == Phase::Receiving && phase == Phase::Waiting) {
                return;
            }
            *status = next.clone();
        }
        (self.on_status)(next);
    }
}

/// Shared with the caller so the timeline and pause accounting start before bot setup.
#[derive(Clone)]
pub struct RecordingClock(Arc<Mutex<Clock>>);

impl RecordingClock {
    pub fn new(started_at: Instant, wall_time: SystemTime) -> Self {
        let origin_unix_micros = wall_time
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_micros()).ok());
        Self::with_origin(started_at, origin_unix_micros)
    }

    pub fn from_origin(started_at: Instant, unix_micros: u64) -> Self {
        Self::with_origin(started_at, Some(unix_micros))
    }

    fn with_origin(started_at: Instant, unix_micros: Option<u64>) -> Self {
        Self(Arc::new(Mutex::new(Clock::new(started_at, unix_micros))))
    }

    pub fn set_paused(&self, paused: bool) {
        self.0.lock().set_paused(paused, Instant::now());
    }
}

#[derive(Clone)]
struct RecordingTarget {
    directory: PathBuf,
    clock: RecordingClock,
    attached_at: Instant,
}

impl RecordingTarget {
    fn create(directory: &Path, clock: RecordingClock) -> Result<Self, Error> {
        let directory = directory.join(TRACK_DIRECTORY);
        std::fs::create_dir(&directory).map_err(|_| Error::DirectoryUnavailable)?;
        Ok(Self {
            directory,
            clock,
            attached_at: Instant::now(),
        })
    }
}

#[derive(Default)]
pub(crate) struct CaptureState {
    admitted: bool,
    closed: bool,
    target: Option<RecordingTarget>,
}

impl CaptureState {
    fn adopt(&mut self, directory: &Path, clock: RecordingClock) -> Result<(), Error> {
        if self.closed || !self.admitted || self.target.is_some() {
            return Err(Error::NotReady);
        }
        self.target = Some(RecordingTarget::create(directory, clock)?);
        Ok(())
    }

    fn set_admitted(&mut self, admitted: bool) -> Option<Phase> {
        if self.closed {
            return None;
        }
        self.admitted = admitted;
        Some(match (&self.target, admitted) {
            (Some(_), _) => Phase::Waiting,
            (None, true) => Phase::Connected,
            (None, false) => Phase::WaitingAdmission,
        })
    }

    fn close(&mut self) {
        self.closed = true;
        if let Some(target) = &self.target {
            target.clock.0.lock().stop(Instant::now());
        }
    }

    fn observe(&self, received_at: Instant) -> Option<Observation> {
        let target = self.target.as_ref()?;
        if self.closed || received_at < target.attached_at {
            return None;
        }
        target.clock.0.lock().observe(received_at)
    }
}

pub struct Session {
    audit: Option<Audit>,
    current: Arc<Mutex<Status>>,
    capture: Arc<Mutex<CaptureState>>,
    adoption: Arc<Notify>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), Error>>>,
}

impl Session {
    /// Starts a recording and its bot together.
    pub fn start(
        config: Config,
        meeting_url: String,
        directory: PathBuf,
        on_status: impl Fn(Status) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let clock = RecordingClock::new(Instant::now(), SystemTime::now());
        Self::start_with_clock(config, meeting_url, directory, clock, on_status)
    }

    /// Starts a recording with an existing local timeline.
    pub fn start_with_clock(
        config: Config,
        meeting_url: String,
        directory: PathBuf,
        clock: RecordingClock,
        on_status: impl Fn(Status) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        Self::spawn(
            config,
            meeting_url,
            Some((directory, clock)),
            None,
            on_status,
        )
    }

    /// Starts joining without accepting PCM for storage. Wait for `Phase::Connected`
    /// before adopting the same session. Dropping the handle requests bot cleanup.
    pub async fn prepare(
        config: Config,
        meeting_url: String,
        on_status: impl Fn(Status) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        Self::spawn(config, meeting_url, None, None, on_status)
    }

    pub fn prepare_audited(
        config: Config,
        meeting_url: String,
        audit: Audit,
        on_status: impl Fn(Status) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        Self::spawn(config, meeting_url, None, Some(audit), on_status)
    }

    fn spawn(
        config: Config,
        meeting_url: String,
        target: Option<(PathBuf, RecordingClock)>,
        audit: Option<Audit>,
        on_status: impl Fn(Status) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| Error::RuntimeUnavailable)?;
        let meeting_url = normalize_meeting_url(&meeting_url)?;
        let mut settings = config.validated()?;
        let api = Api::new(&settings)?;
        let listener = std::net::TcpListener::bind(config.bind_address)
            .and_then(|listener| {
                listener.set_nonblocking(true)?;
                TcpListener::from_std(listener)
            })
            .map_err(|_| Error::ListenerUnavailable)?;
        if let Some(audit) = &audit {
            audit.mark("listener_bound");
        }
        let target = target
            .map(|(directory, clock)| RecordingTarget::create(&directory, clock))
            .transpose()?;
        let capture = Arc::new(Mutex::new(CaptureState {
            target,
            ..CaptureState::default()
        }));
        let adoption = Arc::new(Notify::new());
        let token = uuid::Uuid::new_v4().simple().to_string();
        settings
            .callback
            .query_pairs_mut()
            .append_pair("token", &token);
        let (send, receive) = mpsc::channel(QUEUE_CAPACITY);
        let (stop, stop_rx) = watch::channel(false);
        let initial = Status {
            phase: Phase::Connecting,
            message: None,
        };
        on_status(initial.clone());
        let current = Arc::new(Mutex::new(initial));
        let reporter = Reporter {
            current: current.clone(),
            on_status: Box::new(on_status),
        };
        let ingress = Ingress {
            audit: audit.clone(),
            acceptor: settings.acceptor,
            callback: settings.callback,
            capture: capture.clone(),
            packets: send,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        let adopted = adoption.clone();
        let task = handle.spawn(async move {
            let outcome = run(
                api,
                meeting_url,
                ingress,
                listener,
                receive,
                stop_rx,
                adopted,
                &reporter,
            )
            .await;
            match outcome {
                Ok(message) => {
                    reporter.set(Phase::Finished, message.map(str::to_owned));
                    Ok(())
                }
                Err(error) => {
                    reporter.set(Phase::Failed, Some(error.to_string()));
                    Err(error)
                }
            }
        });
        Ok(Self {
            audit,
            current,
            capture,
            adoption,
            stop,
            task: Some(task),
        })
    }

    pub fn status(&self) -> Status {
        self.current.lock().clone()
    }

    /// Begins accepting PCM for this recording without creating another bot.
    pub fn adopt(&self, directory: &Path, clock: RecordingClock) -> Result<(), Error> {
        if let Some(audit) = &self.audit {
            audit.mark("adopt_requested");
        }
        let mut capture = self.capture.lock();
        if let Err(error) = capture.adopt(directory, clock) {
            if let Some(audit) = &self.audit {
                audit.mark("adopt_rejected");
            }
            return Err(error);
        }
        if let Some((audit, target)) = self.audit.as_ref().zip(capture.target.as_ref()) {
            audit.adopted(
                target.attached_at,
                target.clock.0.lock().snapshot().origin_unix_micros,
            );
        }
        drop(capture);
        self.adoption.notify_one();
        Ok(())
    }

    pub fn set_paused(&self, paused: bool) {
        if let Some(target) = &self.capture.lock().target {
            target.clock.set_paused(paused);
        }
    }

    pub fn request_stop(&self) {
        let mut capture = self.capture.lock();
        if !capture.closed {
            if let Some(audit) = &self.audit {
                audit.mark("stop_requested");
            }
            capture.close();
        }
        drop(capture);
        self.stop.send_replace(true);
    }

    pub async fn finish(mut self) -> Result<(), Error> {
        self.request_stop();
        if let Some(task) = self.task.take() {
            return task.await.map_err(|_| Error::RuntimeUnavailable)?;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.request_stop();
    }
}

async fn create_bot(api: &Api, meeting_url: &str, callback: &reqwest::Url) -> Result<Bot, Error> {
    let deduplication_key = uuid::Uuid::new_v4().to_string();
    match api.create(meeting_url, callback, &deduplication_key).await {
        Ok(bot) => return Ok(bot),
        Err(ApiError::Unauthorized) => return Err(Error::Unauthorized),
        Err(ApiError::Rejected) => return Err(Error::JoinRejected),
        Err(ApiError::Unavailable | ApiError::InvalidResponse) => {}
    }
    // Do not repeat POST. It could have created a bot before its response was lost.
    for _ in 0..2 {
        sleep(Duration::from_millis(250)).await;
        if let Ok(Some(bot)) = api.find_created(&deduplication_key).await {
            return Ok(bot);
        }
    }
    Err(Error::JoinUnconfirmed)
}

async fn confirm_left(api: &Api, bot_id: &str) -> Result<(), Error> {
    timeout(LEAVE_TIMEOUT, async {
        let mut accepted = false;
        loop {
            if !accepted {
                accepted = api.leave(bot_id).await.is_ok();
            }
            if api.get(bot_id).await.is_ok_and(|bot| bot.has_left()) {
                break;
            }
            sleep(Duration::from_millis(750)).await;
        }
    })
    .await
    .map_err(|_| Error::LeaveUnconfirmed)
}

enum End {
    Stopped,
    Left { admitted: bool },
    Failed(Error),
}

async fn monitor(api: &Api, bot: &Bot, capture: &Mutex<CaptureState>, reporter: &Reporter) -> End {
    let mut current = bot.state.clone();
    let mut errors = 0;
    let started = Instant::now();
    let mut admitted = false;
    loop {
        match current.as_str() {
            "fatal_error" | "joined_recording_permission_denied" => {
                return End::Failed(Error::JoinFailed)
            }
            "post_processing" | "ended" | "data_deleted" => return End::Left { admitted },
            "joined_not_recording" | "joined_recording" | "joined_recording_paused" => {
                admitted = true;
                let phase = capture.lock().set_admitted(true);
                if let Some(phase) = phase {
                    reporter.set(phase, None);
                }
            }
            "waiting_room" => {
                let phase = capture.lock().set_admitted(false);
                if let Some(phase) = phase {
                    reporter.set(phase, None);
                }
            }
            _ => {}
        }
        if !admitted
            && started.elapsed() >= JOIN_TIMEOUT
            && reporter.current.lock().phase != Phase::Receiving
        {
            return End::Failed(Error::JoinFailed);
        }
        sleep(POLL_INTERVAL).await;
        match api.get(&bot.id).await {
            Ok(bot) => {
                current = bot.state;
                errors = 0;
            }
            Err(ApiError::Unauthorized) => return End::Failed(Error::Unauthorized),
            Err(_) => {
                errors += 1;
                if errors >= 3 {
                    return End::Failed(Error::ApiUnavailable);
                }
            }
        }
    }
}

struct TrackWriter {
    packets: mpsc::Sender<Packet>,
    task: JoinHandle<(Tracks, Option<TrackError>)>,
}

impl TrackWriter {
    fn start(target: &RecordingTarget, audit: Option<Audit>) -> Self {
        let directory = target.directory.clone();
        let (packets, mut pending) = mpsc::channel::<Packet>(QUEUE_CAPACITY);
        let task = task::spawn_blocking(move || {
            let mut tracks = Tracks::new(directory);
            if let Some(audit) = &audit {
                audit.mark("writer_ready");
            }
            while let Some(packet) = pending.blocking_recv() {
                let metadata = packet
                    .audit_id
                    .map(|id| (id, packet.observation.elapsed_ms));
                let written = tracks.write(packet);
                if let Some((audit, (id, elapsed_ms))) = audit.as_ref().zip(metadata) {
                    audit.packet(
                        if written.is_ok() {
                            "writer_consumed"
                        } else {
                            "writer_error"
                        },
                        id,
                        None,
                        None,
                        Some(elapsed_ms),
                    );
                }
                if let Err(error) = written {
                    return (tracks, Some(error));
                }
            }
            (tracks, None)
        });
        Self { packets, task }
    }
}

async fn writer_closed(writer: Option<&TrackWriter>) {
    match writer {
        Some(writer) => writer.packets.closed().await,
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    api: Api,
    meeting_url: String,
    ingress: Ingress,
    listener: TcpListener,
    mut incoming: mpsc::Receiver<Packet>,
    mut stop: watch::Receiver<bool>,
    adoption: Arc<Notify>,
    reporter: &Reporter,
) -> Result<Option<&'static str>, Error> {
    if *stop.borrow() {
        return Ok(None);
    }
    let mut listener_task = task::spawn(ingress.clone().serve(listener));
    if let Some(audit) = &ingress.audit {
        audit.mark("bot_create_begin");
    }
    // A stop while POST is in flight must still let us recover its ID and request leave.
    let bot = match create_bot(&api, &meeting_url, &ingress.callback).await {
        Ok(bot) => {
            if let Some(audit) = &ingress.audit {
                audit.mark("bot_create_confirmed");
            }
            bot
        }
        Err(error) => {
            listener_task.abort();
            let _ = listener_task.await;
            return Err(error);
        }
    };
    let mut writer = ingress
        .capture
        .lock()
        .target
        .as_ref()
        .map(|target| TrackWriter::start(target, ingress.audit.clone()));
    let prepare_timeout = sleep(PREPARE_TIMEOUT);
    tokio::pin!(prepare_timeout);
    let dropped = &ingress.dropped;
    let mut has_audio = false;
    let mut listener_ended = false;
    let end = {
        let monitor = monitor(&api, &bot, &ingress.capture, reporter);
        tokio::pin!(monitor);
        loop {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stop| *stop) => break End::Stopped,
                _ = writer_closed(writer.as_ref()) => break End::Failed(Error::SaveFailed),
                _ = &mut listener_task => {
                    listener_ended = true;
                    break End::Failed(Error::ListenerUnavailable);
                }
                end = &mut monitor => break end,
                _ = adoption.notified(), if writer.is_none() => {
                    writer = ingress.capture.lock().target.as_ref().map(|target| TrackWriter::start(target, ingress.audit.clone()));
                    if writer.is_some() {
                        reporter.set(Phase::Waiting, None);
                    }
                }
                _ = &mut prepare_timeout, if writer.is_none() => {
                    let mut capture = ingress.capture.lock();
                    let Some(target) = &capture.target else {
                        capture.close();
                        break End::Failed(Error::PrepareExpired);
                    };
                    writer = Some(TrackWriter::start(target, ingress.audit.clone()));
                }
                Some(packet) = incoming.recv() => {
                    if writer.is_none() {
                        writer = ingress.capture.lock().target.as_ref().map(|target| TrackWriter::start(target, ingress.audit.clone()));
                    }
                    if let Some(writer) = &writer {
                        if queue_packet(packet, &bot.id, &writer.packets, dropped, ingress.audit.as_ref()) && !has_audio {
                            has_audio = true;
                            reporter.set(Phase::Receiving, None);
                        }
                    }
                }
            }
        }
    };
    let target = {
        let mut capture = ingress.capture.lock();
        capture.close();
        capture.target.clone()
    };
    if !listener_ended {
        listener_task.abort();
        let _ = listener_task.await;
    }
    let leave = async {
        if matches!(end, End::Left { .. }) || bot.has_left() {
            Ok(())
        } else {
            confirm_left(&api, &bot.id).await
        }
    };
    let Some(target) = target else {
        leave.await?;
        return match end {
            End::Stopped | End::Left { admitted: true } => Ok(None),
            End::Left { admitted: false } => Err(Error::JoinFailed),
            End::Failed(error) => Err(error),
        };
    };
    let TrackWriter { packets, task } =
        writer.unwrap_or_else(|| TrackWriter::start(&target, ingress.audit.clone()));
    while let Ok(packet) = incoming.try_recv() {
        queue_packet(packet, &bot.id, &packets, dropped, ingress.audit.as_ref());
    }
    drop(packets);
    let cleanup = async {
        let left = leave.await;
        let names = timeout(api::REQUEST_TIMEOUT, api.participants(&bot.id)).await;
        (left, names.ok().and_then(Result::ok))
    };
    let (written, (left, names)) = tokio::join!(task, cleanup);
    let (tracks, write_error) = written.map_err(|_| Error::SaveFailed)?;
    let clock = target.clock.0.lock().snapshot();
    let queue_dropped_packets = dropped.load(Ordering::Relaxed);
    let names_missing = names.is_none();
    let saved = task::spawn_blocking(move || {
        tracks.finish(&names.unwrap_or_default(), clock, queue_dropped_packets)
    })
    .await
    .map_err(|_| Error::SaveFailed)?;

    left?;
    if let Some(error) = write_error {
        return Err(track_error(error));
    }
    let saved = saved.map_err(track_error)?;
    if let End::Failed(error) = end {
        return Err(error);
    }
    Ok(if saved == 0 {
        Some("Attendee did not send any participant audio.")
    } else if names_missing {
        Some("Bazza saved the audio but could not load participant names.")
    } else if queue_dropped_packets > 0 {
        Some("Bazza saved the tracks with gaps after dropping some audio packets.")
    } else {
        None
    })
}

fn queue_packet(
    packet: Packet,
    bot_id: &str,
    writer: &mpsc::Sender<Packet>,
    dropped: &AtomicU64,
    audit: Option<&Audit>,
) -> bool {
    let audit_id = packet.audit_id;
    let trace = |event| {
        if let Some((audit, id)) = audit.zip(audit_id) {
            audit.packet(event, id, None, None, None);
        }
    };
    if packet.bot_id != bot_id {
        trace("rejected_bot");
        return false;
    }
    match writer.try_send(packet) {
        Ok(()) => {
            trace("queued_writer");
            true
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            trace("queue_writer_full");
            dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            trace("queue_writer_closed");
            false
        }
    }
}

fn track_error(error: TrackError) -> Error {
    match error {
        TrackError::Disk => Error::SaveFailed,
        TrackError::Limit => Error::TrackLimit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_capture_adopts_only_once_after_admission() {
        let directory = tempfile::tempdir().unwrap();
        let clock = RecordingClock::from_origin(Instant::now(), 1_793_750_400_000_000);
        let track_directory = directory.path().join(TRACK_DIRECTORY);
        let mut capture = CaptureState::default();

        assert_eq!(capture.set_admitted(false), Some(Phase::WaitingAdmission));
        assert_eq!(
            capture.adopt(directory.path(), clock.clone()),
            Err(Error::NotReady)
        );
        assert!(!track_directory.exists());
        assert_eq!(capture.set_admitted(true), Some(Phase::Connected));
        let before_adoption = Instant::now();
        assert!(capture.observe(before_adoption).is_none());
        assert!(!track_directory.exists());

        assert_eq!(capture.adopt(directory.path(), clock.clone()), Ok(()));
        assert!(capture.observe(Instant::now()).is_some());
        let attached_at = capture.target.as_ref().unwrap().attached_at;
        assert!(capture
            .observe(attached_at - Duration::from_millis(1))
            .is_none());
        assert_eq!(
            capture.adopt(directory.path(), clock.clone()),
            Err(Error::NotReady)
        );
        assert_eq!(capture.set_admitted(true), Some(Phase::Waiting));

        capture.close();
        assert!(capture.observe(Instant::now()).is_none());
        assert_eq!(capture.adopt(directory.path(), clock), Err(Error::NotReady));
    }

    #[test]
    fn cancelled_preparation_never_creates_track_directory() {
        let directory = tempfile::tempdir().unwrap();
        let clock = RecordingClock::from_origin(Instant::now(), 1_793_750_400_000_000);
        let mut capture = CaptureState::default();

        assert_eq!(capture.set_admitted(true), Some(Phase::Connected));
        capture.close();
        assert_eq!(capture.set_admitted(true), None);
        assert_eq!(capture.adopt(directory.path(), clock), Err(Error::NotReady));
        assert!(!directory.path().join(TRACK_DIRECTORY).exists());
    }
}

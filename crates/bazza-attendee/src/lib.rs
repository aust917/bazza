//! Per-participant Google Meet audio from an explicitly configured Attendee server.

mod api;
mod config;
mod ingress;
mod live;
mod tracks;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{stream::FuturesUnordered, StreamExt};
use parking_lot::Mutex;
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch, Notify},
    task::{self, JoinHandle},
    time::{sleep, timeout},
};

use api::{Api, ApiError, Bot};
use ingress::{Clock, Ingress, Observation, Packet};
use live::LiveAudio;
use tracks::{TrackError, Tracks};

pub use config::{normalize_meeting_url, Config, Error};
pub use live::LiveAudioSink;
pub use tracks::{
    Manifest, SampleInterval, Timing, TrackCoverage, TrackInfo, MANIFEST_FILE, TRACK_DIRECTORY,
};

pub const SAMPLE_RATE: u32 = 16_000;
const QUEUE_CAPACITY: usize = 512;
const JOIN_TIMEOUT: Duration = Duration::from_secs(180);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(300);
const LEAVE_TIMEOUT: Duration = Duration::from_secs(12);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const ROSTER_INTERVAL: Duration = Duration::from_secs(1);

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
    live: Option<LiveAudioSink>,
}

impl RecordingTarget {
    fn create(
        directory: &Path,
        clock: RecordingClock,
        live: Option<LiveAudioSink>,
    ) -> Result<Self, Error> {
        let directory = directory.join(TRACK_DIRECTORY);
        std::fs::create_dir(&directory).map_err(|_| Error::DirectoryUnavailable)?;
        Ok(Self {
            directory,
            clock,
            attached_at: Instant::now(),
            live,
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
    fn adopt(
        &mut self,
        directory: &Path,
        clock: RecordingClock,
        live: Option<LiveAudioSink>,
    ) -> Result<(), Error> {
        if self.closed || !self.admitted || self.target.is_some() {
            return Err(Error::NotReady);
        }
        self.target = Some(RecordingTarget::create(directory, clock, live)?);
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
        Self::start_with_live_audio(config, meeting_url, directory, clock, on_status, None)
    }

    /// Starts a recording with an existing timeline and an optional live PCM sink.
    pub fn start_with_live_audio(
        config: Config,
        meeting_url: String,
        directory: PathBuf,
        clock: RecordingClock,
        on_status: impl Fn(Status) + Send + Sync + 'static,
        live: Option<LiveAudioSink>,
    ) -> Result<Self, Error> {
        Self::spawn(
            config,
            meeting_url,
            Some((directory, clock, live)),
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
        Self::spawn(config, meeting_url, None, on_status)
    }

    fn spawn(
        config: Config,
        meeting_url: String,
        target: Option<(PathBuf, RecordingClock, Option<LiveAudioSink>)>,
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
        let target = target
            .map(|(directory, clock, live)| RecordingTarget::create(&directory, clock, live))
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
        self.adopt_with_live_audio(directory, clock, None)
    }

    /// Adopts a prepared session and attaches its live PCM sink together.
    pub fn adopt_with_live_audio(
        &self,
        directory: &Path,
        clock: RecordingClock,
        live: Option<LiveAudioSink>,
    ) -> Result<(), Error> {
        self.capture.lock().adopt(directory, clock, live)?;
        self.adoption.notify_one();
        Ok(())
    }

    pub fn set_paused(&self, paused: bool) {
        if let Some(target) = &self.capture.lock().target {
            target.clock.set_paused(paused);
        }
    }

    pub fn request_stop(&self) {
        self.capture.lock().close();
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
    live: Option<LiveAudio>,
}

impl TrackWriter {
    fn start(target: &RecordingTarget) -> Self {
        let directory = target.directory.clone();
        let (packets, mut pending) = mpsc::channel(QUEUE_CAPACITY);
        let task = task::spawn_blocking(move || {
            let mut tracks = Tracks::new(directory);
            while let Some(packet) = pending.blocking_recv() {
                if let Err(error) = tracks.write(packet) {
                    return (tracks, Some(error));
                }
            }
            (tracks, None)
        });
        let live = target
            .live
            .as_ref()
            .map(|sink| LiveAudio::new(sink.clone(), target.clock.clone()));
        Self {
            packets,
            task,
            live,
        }
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
    // A stop while POST is in flight must still let us recover its ID and request leave.
    let bot = match create_bot(&api, &meeting_url, &ingress.callback).await {
        Ok(bot) => bot,
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
        .map(TrackWriter::start);
    let prepare_timeout = sleep(PREPARE_TIMEOUT);
    tokio::pin!(prepare_timeout);
    let dropped = &ingress.dropped;
    let mut has_audio = false;
    let mut listener_ended = false;
    let end = {
        let mut roster_tick = tokio::time::interval(ROSTER_INTERVAL);
        roster_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut roster = FuturesUnordered::new();
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
                    writer = ingress.capture.lock().target.as_ref().map(TrackWriter::start);
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
                    writer = Some(TrackWriter::start(target));
                }
                _ = roster_tick.tick(), if roster.is_empty() && writer.as_ref()
                    .is_some_and(|writer| writer.live.is_some()) => {
                    if writer.as_ref().and_then(|writer| writer.live.as_ref())
                        .is_some_and(LiveAudio::needs_names) {
                        roster.push(timeout(api::REQUEST_TIMEOUT, api.participants(&bot.id)));
                    }
                }
                Some(result) = roster.next(), if !roster.is_empty() => {
                    if let (Ok(Ok(names)), Some(live)) =
                        (result, writer.as_mut().and_then(|writer| writer.live.as_mut())) {
                        live.update_names(names);
                    }
                }
                Some(packet) = incoming.recv() => {
                    if writer.is_none() {
                        writer = ingress.capture.lock().target.as_ref().map(TrackWriter::start);
                    }
                    if let Some(writer) = &mut writer {
                        if packet.bot_id == bot.id {
                            if let Some(live) = &mut writer.live {
                                live.packet(&packet);
                            }
                        }
                        if queue_packet(packet, &bot.id, &writer.packets, dropped) && !has_audio {
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
    let TrackWriter { packets, task, .. } = writer.unwrap_or_else(|| TrackWriter::start(&target));
    while let Ok(packet) = incoming.try_recv() {
        queue_packet(packet, &bot.id, &packets, dropped);
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
) -> bool {
    if packet.bot_id != bot_id {
        return false;
    }
    match writer.try_send(packet) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
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
            capture.adopt(directory.path(), clock.clone(), None),
            Err(Error::NotReady)
        );
        assert!(!track_directory.exists());
        assert_eq!(capture.set_admitted(true), Some(Phase::Connected));
        let before_adoption = Instant::now();
        assert!(capture.observe(before_adoption).is_none());
        assert!(!track_directory.exists());

        let live: LiveAudioSink = Arc::new(|_, _, _, _, _| {});
        assert_eq!(
            capture.adopt(directory.path(), clock.clone(), Some(live)),
            Ok(())
        );
        assert!(capture.observe(Instant::now()).is_some());
        let attached_at = capture.target.as_ref().unwrap().attached_at;
        assert!(capture
            .observe(attached_at - Duration::from_millis(1))
            .is_none());
        assert_eq!(
            capture.adopt(directory.path(), clock.clone(), None),
            Err(Error::NotReady)
        );
        assert_eq!(capture.set_admitted(true), Some(Phase::Waiting));

        capture.close();
        assert!(capture.observe(Instant::now()).is_none());
        assert_eq!(
            capture.adopt(directory.path(), clock, None),
            Err(Error::NotReady)
        );
    }

    #[test]
    fn cancelled_preparation_never_creates_track_directory() {
        let directory = tempfile::tempdir().unwrap();
        let clock = RecordingClock::from_origin(Instant::now(), 1_793_750_400_000_000);
        let mut capture = CaptureState::default();

        assert_eq!(capture.set_admitted(true), Some(Phase::Connected));
        capture.close();
        assert_eq!(capture.set_admitted(true), None);
        assert_eq!(
            capture.adopt(directory.path(), clock, None),
            Err(Error::NotReady)
        );
        assert!(!directory.path().join(TRACK_DIRECTORY).exists());
    }
}

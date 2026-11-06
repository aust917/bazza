use std::{
    collections::{HashMap, HashSet},
    fs,
    future::{pending, IntoFuture},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, ensure, Context as _, Result};
use serenity::{
    all::{
        Channel, ChannelId, ChannelType, Client, Context, EventHandler, GatewayIntents, Guild,
        GuildId, Http, HttpError, Member, Ready, UserId, VoiceState,
    },
    async_trait,
};
use songbird::{
    driver::{DecodeConfig, DecodeMode},
    Config, CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit,
};
use tokio::{
    sync::{mpsc, oneshot, watch, Notify},
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::tracks::{
    save_manifest, CaptureSpan, CaptureTimeline, Manifest, Tick, TickRange, Tracks, MAX_TRACKS,
    SAMPLES_PER_TICK, SAMPLE_RATE, TICK_MS,
};

const SETUP_TIMEOUT: Duration = Duration::from_secs(20);
const LEAVE_TIMEOUT: Duration = Duration::from_secs(5);
const MEMBER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
const MEMBER_LOOKUP_COOLDOWN: Duration = Duration::from_secs(3);
const MAX_CACHED_VOICE_NAMES: usize = MAX_TRACKS * 8;
const MAX_WAV_TICKS: u64 = (u32::MAX as u64 - 36) / (SAMPLES_PER_TICK as u64 * 2);
const MAX_TIMELINE_RANGES: usize = 256;
const CALLBACK_GAP_US: u64 = 3 * TICK_MS * 1_000;
type Stop = watch::Sender<Option<&'static str>>;
type StopRx = watch::Receiver<Option<&'static str>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceStatus {
    Connecting,
    Connected,
    Receiving,
    Stopped,
    Interrupted,
    Failed(String),
}

pub struct VoiceOutcome {
    pub manifest: Option<Manifest>,
    /// Discord has confirmed that the bot is out of voice, or no join was attempted.
    pub disconnected: bool,
    pub capture_started_at: Option<Instant>,
}

/// A provider-attributed 20 ms mono PCM frame at [`SAMPLE_RATE`]. Empty samples
/// carry a name update for a user whose voice was already observed.
pub struct LiveVoiceFrame {
    pub user_id: u64,
    pub display_name: Option<Arc<str>>,
    pub start_ms: u64,
    pub samples: Vec<i16>,
}

/// A live consumer must enqueue or drop the frame without waiting.
pub type LiveAudioSink = Arc<dyn Fn(LiveVoiceFrame) + Send + Sync>;

struct LiveAudio {
    origin: Instant,
    sink: LiveAudioSink,
}

/// An owned recording attempt. Dropping the handle requests the same graceful stop
/// as an explicit stop, provided the runtime is still running.
pub struct VoiceSession {
    stop: Stop,
    status: watch::Receiver<VoiceStatus>,
    state: Arc<Mutex<Capture>>,
    worker: JoinHandle<Result<VoiceOutcome>>,
    activation: Option<oneshot::Sender<RecordingTarget>>,
}

struct RecordingTarget {
    dir: PathBuf,
    local_epoch: Option<(Instant, u64)>,
    live_audio: Option<LiveAudio>,
}

enum RecordingStart {
    Immediate(RecordingTarget),
    Prepared(oneshot::Receiver<RecordingTarget>),
}

impl VoiceSession {
    /// Starts setup on the current Tokio runtime without blocking local capture.
    pub fn start(guild_id: u64, channel_id: u64, dir: PathBuf, token: String) -> Self {
        Self::start_recording(guild_id, channel_id, dir, token, None)
    }

    pub fn start_with_live_audio(
        guild_id: u64,
        channel_id: u64,
        dir: PathBuf,
        token: String,
        origin: Instant,
        sink: LiveAudioSink,
    ) -> Self {
        Self::spawn(
            guild_id,
            channel_id,
            token,
            None,
            RecordingStart::Immediate(RecordingTarget {
                dir,
                local_epoch: None,
                live_audio: Some(LiveAudio { origin, sink }),
            }),
        )
    }

    /// Starts a session with a time limit, for an interactive probe.
    pub fn start_with_limit(
        guild_id: u64,
        channel_id: u64,
        dir: PathBuf,
        token: String,
        duration: Duration,
    ) -> Self {
        Self::start_recording(guild_id, channel_id, dir, token, Some(duration))
    }

    /// Joins voice without creating any files or accepting PCM into a recording.
    pub fn prepare(guild_id: u64, channel_id: u64, token: String) -> Self {
        let (activation, receiver) = oneshot::channel();
        let mut session = Self::spawn(
            guild_id,
            channel_id,
            token,
            None,
            RecordingStart::Prepared(receiver),
        );
        session.activation = Some(activation);
        session
    }

    /// Hands the prepared connection to a recording. Acceptance queues the target;
    /// the status stream still reports subsequent connection or writer failures.
    pub fn adopt(&mut self, dir: PathBuf, local_epoch: (Instant, u64)) -> Result<()> {
        self.adopt_with_live_audio(dir, local_epoch, None)
    }

    pub fn adopt_with_live_audio(
        &mut self,
        dir: PathBuf,
        local_epoch: (Instant, u64),
        sink: Option<LiveAudioSink>,
    ) -> Result<()> {
        ensure!(
            self.stop.borrow().is_none(),
            "Discord preparation is no longer active"
        );
        let activation = self
            .activation
            .take()
            .context("Discord preparation already used")?;
        activation
            .send(RecordingTarget {
                dir,
                local_epoch: Some(local_epoch),
                live_audio: sink.map(|sink| LiveAudio {
                    origin: local_epoch.0,
                    sink,
                }),
            })
            .map_err(|_| anyhow!("Discord preparation is no longer active"))
    }

    fn start_recording(
        guild_id: u64,
        channel_id: u64,
        dir: PathBuf,
        token: String,
        duration: Option<Duration>,
    ) -> Self {
        Self::spawn(
            guild_id,
            channel_id,
            token,
            duration,
            RecordingStart::Immediate(RecordingTarget {
                dir,
                local_epoch: None,
                live_audio: None,
            }),
        )
    }

    fn spawn(
        guild_id: u64,
        channel_id: u64,
        token: String,
        duration: Option<Duration>,
        start: RecordingStart,
    ) -> Self {
        let (stop, stop_rx) = watch::channel(None);
        let (status_tx, status) = watch::channel(VoiceStatus::Connecting);
        let state = Arc::new(Mutex::new(Capture::new(duration)));
        let capture = Receiver {
            state: state.clone(),
            stop: stop.clone(),
            status: status_tx,
            names: Arc::new(Mutex::new(SpeakerNames::default())),
            names_needed: Arc::new(Notify::new()),
        };
        let worker = tokio::spawn(async move {
            let result = run(
                guild_id, channel_id, start, token, duration, &capture, stop_rx,
            )
            .await;
            let status = match &result {
                Ok(outcome)
                    if outcome.disconnected
                        && outcome.manifest.as_ref().is_none_or(|m| {
                            matches!(
                                m.finish_reason.as_str(),
                                "stopped_by_operator" | "duration_elapsed"
                            )
                        }) =>
                {
                    VoiceStatus::Stopped
                }
                Ok(_) => VoiceStatus::Interrupted,
                Err(error) => VoiceStatus::Failed(error.to_string()),
            };
            capture.status.send_replace(status);
            result
        });
        Self {
            stop,
            status,
            state,
            worker,
            activation: None,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<VoiceStatus> {
        self.status.clone()
    }

    pub fn set_paused(&self, paused: bool) {
        self.state
            .lock()
            .unwrap()
            .set_paused(paused, Instant::now());
    }

    pub fn request_stop(&self) {
        stop_once(&self.stop, "stopped_by_operator");
    }

    pub async fn stop(mut self) -> Result<VoiceOutcome> {
        self.request_stop();
        (&mut self.worker)
            .await
            .context("Discord recording task failed")?
    }
}

impl Drop for VoiceSession {
    fn drop(&mut self) {
        self.request_stop();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Unknown,
    Away,
    In(ChannelId),
}

struct Gateway {
    ready: Mutex<Option<oneshot::Sender<Context>>>,
    bot: UserId,
    guild: GuildId,
    channel: ChannelId,
    position: watch::Sender<Position>,
    stop: Stop,
    names: Arc<Mutex<SpeakerNames>>,
}

#[derive(Default)]
struct SpeakerNames {
    by_user: HashMap<UserId, Arc<str>>,
}

impl SpeakerNames {
    fn member_name(user: UserId, member: &Member) -> Option<&str> {
        let name = member.display_name().trim();
        (member.user.id == user && !name.is_empty()).then_some(name)
    }

    fn remember_gateway(&mut self, user: UserId, member: &Member) {
        if self.by_user.len() >= MAX_CACHED_VOICE_NAMES && !self.by_user.contains_key(&user) {
            return;
        }
        if let Some(name) = Self::member_name(user, member) {
            self.by_user.insert(user, Arc::from(name));
        }
    }

    fn remember_rest(&mut self, user: UserId, member: &Member) {
        if let Some(name) = Self::member_name(user, member) {
            // A voice-state update may have supplied a more recent name while REST was in flight.
            self.by_user.entry(user).or_insert_with(|| Arc::from(name));
        }
    }
}

enum LookupState {
    CoolingDown(Instant),
    Done,
}

#[derive(Default)]
struct MemberLookups {
    attempts: HashMap<UserId, LookupState>,
}

impl MemberLookups {
    fn next(&self, users: &[UserId], names: &SpeakerNames, now: Instant) -> Option<(UserId, bool)> {
        let mut retry = None;
        for &user in users {
            if names.by_user.contains_key(&user) {
                continue;
            }
            match self.attempts.get(&user) {
                None => return Some((user, false)),
                Some(LookupState::CoolingDown(at)) if *at <= now => retry = Some((user, true)),
                _ => {}
            }
        }
        retry
    }

    fn finish(&mut self, user: UserId, retry: bool, transient: bool, now: Instant) {
        let state = if transient && !retry {
            LookupState::CoolingDown(now + MEMBER_LOOKUP_COOLDOWN)
        } else {
            LookupState::Done
        };
        self.attempts.insert(user, state);
    }
}

#[async_trait]
impl EventHandler for Gateway {
    async fn ready(&self, ctx: Context, event: Ready) {
        if event.user.id == self.bot {
            if let Some(ready) = self.ready.lock().unwrap().take() {
                let _ = ready.send(ctx);
            }
        }
    }

    async fn guild_create(&self, _: Context, guild: Guild, _: Option<bool>) {
        if guild.id == self.guild {
            {
                let mut names = self.names.lock().unwrap();
                for (&user, state) in &guild.voice_states {
                    if user == self.bot || state.channel_id != Some(self.channel) {
                        continue;
                    }
                    let member = state
                        .member
                        .as_ref()
                        .filter(|member| member.user.id == user)
                        .or_else(|| guild.members.get(&user));
                    if let Some(member) = member {
                        names.remember_gateway(user, member);
                    }
                }
            }
            let initial = guild
                .voice_states
                .get(&self.bot)
                .and_then(|state| state.channel_id)
                .map_or(Position::Away, Position::In);
            self.position.send_if_modified(|position| {
                if *position != Position::Unknown {
                    return false;
                }
                *position = initial;
                true
            });
        }
    }

    async fn voice_state_update(&self, _: Context, _: Option<VoiceState>, new: VoiceState) {
        if new.guild_id != Some(self.guild) {
            return;
        }
        if new.user_id == self.bot {
            let position = new.channel_id.map_or(Position::Away, Position::In);
            let previous = self.position.send_replace(position);
            if previous == Position::In(self.channel) && position != previous {
                stop_once(&self.stop, "bot_left_or_moved");
            }
        } else if new.channel_id == Some(self.channel) {
            if let Some(member) = new.member.as_ref() {
                self.names
                    .lock()
                    .unwrap()
                    .remember_gateway(new.user_id, member);
            }
        }
    }
}

struct Capture {
    by_ssrc: HashMap<u32, u64>,
    users: HashSet<u64>,
    writer: Option<mpsc::Sender<Tick>>,
    live_audio: Option<LiveAudio>,
    paused_since: Option<Instant>,
    paused_total: Duration,
    last_resume: Option<Instant>,
    ticks: u64,
    limit: u64,
    limit_reason: &'static str,
    dropped_ticks: u64,
    unmapped_frames: u64,
    invalid_frames: u64,
    clock: CaptureClock,
}

struct CaptureClock {
    origin: Instant,
    split_next: bool,
    timeline: CaptureTimeline,
}

impl CaptureClock {
    fn new() -> Self {
        let origin = Instant::now();
        let attempt_started_unix_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_micros()).ok());
        Self {
            origin,
            split_next: false,
            timeline: CaptureTimeline {
                attempt_started_unix_us,
                local_epoch_unix_us: None,
                local_epoch_elapsed_us: None,
                capture_closed_elapsed_us: 0,
                pause_seen: false,
                spans: Vec::new(),
                writer_drops: Vec::new(),
                truncated: false,
            },
        }
    }

    fn elapsed_us(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.origin)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    }

    fn pause(&mut self) {
        self.timeline.pause_seen = true;
        self.split_next = true;
    }

    fn set_local_epoch(&mut self, (epoch, unix_us): (Instant, u64)) {
        let offset = if let Some(elapsed) = epoch.checked_duration_since(self.origin) {
            i64::try_from(elapsed.as_micros()).ok()
        } else {
            i64::try_from(self.origin.duration_since(epoch).as_micros())
                .ok()
                .map(|elapsed| -elapsed)
        };
        self.timeline.local_epoch_unix_us = Some(unix_us);
        self.timeline.local_epoch_elapsed_us = offset;
    }

    fn tick(&mut self, tick: u64, now: Instant) {
        let elapsed_us = self.elapsed_us(now);
        let split = std::mem::take(&mut self.split_next);
        if let Some(span) = self.timeline.spans.last_mut().filter(|span| {
            !split
                && span.end_tick == tick
                && elapsed_us.saturating_sub(span.last_elapsed_us) <= CALLBACK_GAP_US
        }) {
            span.end_tick = tick + 1;
            span.last_elapsed_us = elapsed_us;
        } else if self.timeline.spans.len() < MAX_TIMELINE_RANGES {
            self.timeline.spans.push(CaptureSpan {
                start_tick: tick,
                end_tick: tick + 1,
                first_elapsed_us: elapsed_us,
                last_elapsed_us: elapsed_us,
            });
        } else {
            self.timeline.truncated = true;
        }
    }

    fn writer_drop(&mut self, tick: u64) {
        if let Some(last) = self
            .timeline
            .writer_drops
            .last_mut()
            .filter(|last| last.end_tick == tick)
        {
            last.end_tick = tick + 1;
        } else if self.timeline.writer_drops.len() < MAX_TIMELINE_RANGES {
            self.timeline.writer_drops.push(TickRange {
                start_tick: tick,
                end_tick: tick + 1,
            });
        } else {
            self.timeline.truncated = true;
        }
    }

    fn close(mut self, now: Instant) -> CaptureTimeline {
        self.timeline.capture_closed_elapsed_us = self.elapsed_us(now);
        self.timeline
    }
}

impl Capture {
    fn new(duration: Option<Duration>) -> Self {
        let requested_ticks = duration.map_or(u128::MAX, |d| d.as_millis() / u128::from(TICK_MS));
        let limit = requested_ticks.min(u128::from(MAX_WAV_TICKS)) as u64;
        Self {
            by_ssrc: HashMap::new(),
            users: HashSet::new(),
            writer: None,
            live_audio: None,
            paused_since: None,
            paused_total: Duration::ZERO,
            last_resume: None,
            ticks: 0,
            limit,
            limit_reason: if requested_ticks == u128::from(limit) {
                "duration_elapsed"
            } else {
                "wav_size_limit"
            },
            dropped_ticks: 0,
            unmapped_frames: 0,
            invalid_frames: 0,
            clock: CaptureClock::new(),
        }
    }

    fn set_paused(&mut self, paused: bool, now: Instant) {
        if paused {
            if self.paused_since.is_none() {
                self.paused_since = Some(now);
                self.clock.pause();
            }
        } else if let Some(since) = self.paused_since.take() {
            self.paused_total += now.saturating_duration_since(since);
            self.last_resume = Some(now);
        }
    }

    fn live_at(&self, end: Instant) -> Option<(&LiveAudio, u64)> {
        let live = self.live_audio.as_ref()?;
        let start = end.checked_sub(Duration::from_millis(TICK_MS))?;
        if self.paused_since.is_some() || self.last_resume.is_some_and(|resume| start < resume) {
            return None;
        }
        let elapsed = start
            .checked_duration_since(live.origin)?
            .saturating_sub(self.paused_total);
        Some((live, elapsed.as_millis().min(u128::from(u64::MAX)) as u64))
    }
}

#[derive(Clone)]
struct Receiver {
    state: Arc<Mutex<Capture>>,
    stop: Stop,
    status: watch::Sender<VoiceStatus>,
    names: Arc<Mutex<SpeakerNames>>,
    names_needed: Arc<Notify>,
}

fn stop_once(stop: &Stop, reason: &'static str) {
    stop.send_if_modified(|current| {
        if current.is_some() {
            return false;
        }
        *current = Some(reason);
        true
    });
}

#[async_trait]
impl VoiceEventHandler for Receiver {
    async fn act(&self, event: &EventContext<'_>) -> Option<Event> {
        let mut capture = self.state.lock().unwrap();
        match event {
            EventContext::SpeakingStateUpdate(update) => {
                if let Some(user) = update.user_id.filter(|user| user.0 != 0) {
                    capture
                        .by_ssrc
                        .retain(|ssrc, id| *id != user.0 || *ssrc == update.ssrc);
                    capture.by_ssrc.insert(update.ssrc, user.0);
                }
            }
            EventContext::ClientDisconnect(update) => {
                capture.by_ssrc.retain(|_, id| *id != update.user_id.0);
            }
            EventContext::DriverDisconnect(_) | EventContext::DriverReconnect(_) => {
                capture.writer.take();
                capture.by_ssrc.clear();
                stop_once(&self.stop, "voice_connection_interrupted");
            }
            EventContext::VoiceTick(tick) => {
                if capture.writer.is_none()
                    || capture.paused_since.is_some()
                    || self.stop.borrow().is_some()
                {
                    return None;
                }
                let index = capture.ticks;
                capture.ticks += 1;
                let now = Instant::now();
                capture.clock.tick(index, now);
                let mut frames = Vec::new();
                for (ssrc, data) in &tick.speaking {
                    // Packetless decoder output may be synthetic loss concealment.
                    if data.packet.is_none() {
                        continue;
                    }
                    let Some(&user) = capture.by_ssrc.get(ssrc) else {
                        capture.unmapped_frames += 1;
                        continue;
                    };
                    let Some(stereo) = data
                        .decoded_voice
                        .as_deref()
                        .filter(|pcm| pcm.len() == SAMPLES_PER_TICK * 2)
                    else {
                        capture.invalid_frames += 1;
                        continue;
                    };
                    if !capture.users.contains(&user) && capture.users.len() == MAX_TRACKS {
                        stop_once(&self.stop, "speaker_limit");
                        return None;
                    }
                    if capture.users.insert(user) && capture.live_audio.is_some() {
                        self.names_needed.notify_one();
                    }
                    let mono = stereo
                        .chunks_exact(2)
                        .map(|pair| ((i32::from(pair[0]) + i32::from(pair[1])) / 2) as i16)
                        .collect::<Vec<_>>();
                    frames.push((user, mono));
                }
                let live_batch =
                    capture
                        .live_at(now)
                        .filter(|_| !frames.is_empty())
                        .map(|(live, start_ms)| {
                            let names = self.names.try_lock().ok();
                            let batch = frames
                                .iter()
                                .map(|(user_id, samples)| LiveVoiceFrame {
                                    user_id: *user_id,
                                    display_name: names
                                        .as_ref()
                                        .and_then(|names| names.by_user.get(&UserId::new(*user_id)))
                                        .cloned(),
                                    start_ms,
                                    samples: samples.clone(),
                                })
                                .collect::<Vec<_>>();
                            (live.sink.clone(), batch)
                        });
                let queued = if frames.is_empty() {
                    false
                } else {
                    match capture
                        .writer
                        .as_ref()
                        .unwrap()
                        .try_send(Tick { index, frames })
                    {
                        Ok(()) => true,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            capture.dropped_ticks += 1;
                            capture.clock.writer_drop(index);
                            false
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            stop_once(&self.stop, "writer_stopped");
                            false
                        }
                    }
                };
                if capture.ticks == capture.limit {
                    stop_once(&self.stop, capture.limit_reason);
                }
                drop(capture);
                if queued {
                    if let Some((sink, batch)) = live_batch {
                        for frame in batch {
                            if self.stop.borrow().is_some() {
                                break;
                            }
                            sink(frame);
                        }
                    }
                    self.status.send_if_modified(|status| {
                        if *status != VoiceStatus::Connected {
                            return false;
                        }
                        *status = VoiceStatus::Receiving;
                        true
                    });
                }
            }
            _ => {}
        }
        None
    }
}

async fn run(
    guild_id: u64,
    channel_id: u64,
    start: RecordingStart,
    token: String,
    duration: Option<Duration>,
    capture: &Receiver,
    mut stop: StopRx,
) -> Result<VoiceOutcome> {
    ensure!(
        guild_id != 0 && channel_id != 0,
        "Invalid Discord server or channel"
    );
    ensure!(
        capture.state.lock().unwrap().limit != 0,
        "Invalid Discord recording duration"
    );
    let guild = GuildId::new(guild_id);
    let channel = ChannelId::new(channel_id);
    let (ready, ready_rx) = oneshot::channel();
    let (position_tx, mut position) = watch::channel(Position::Unknown);
    let mut client = tokio::select! {
        biased;
        _ = stop.changed() => return Ok(not_joined()),
        result = timeout(SETUP_TIMEOUT, async {
            let bot = Http::new(&token).get_current_user().await
                .map_err(|_| anyhow!("Could not authenticate the Discord bot"))?;
            ensure!(bot.bot, "Discord credential does not identify a bot");
            let config = Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));
            Client::builder(&token, GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES)
                .event_handler(Gateway {
                    ready: Mutex::new(Some(ready)), bot: bot.id, guild, channel,
                    position: position_tx, stop: capture.stop.clone(),
                    names: capture.names.clone(),
                })
                .register_songbird_from_config(config)
                .into_future().await.map_err(|_| anyhow!("Could not initialize the Discord bot"))
        }) => result.context("Discord setup timed out")??,
    };
    drop(token);
    let shards = client.shard_manager.clone();
    let gateway_stop = capture.stop.clone();
    let gateway = tokio::spawn(async move {
        let _ = client.start().await;
        stop_once(&gateway_stop, "gateway_stopped");
    });
    let result = async {
        let ctx = tokio::select! {
            biased;
            _ = stop.changed() => return stop_before_join(&stop),
            ready = timeout(SETUP_TIMEOUT, ready_rx) => {
                ready.context("Discord login timed out")?.context("Discord did not acknowledge the login")?
            }
        };
        tokio::select! {
            biased;
            _ = stop.changed() => return stop_before_join(&stop),
            result = timeout(SETUP_TIMEOUT, async {
                let info = channel.to_channel(&ctx.http).await
                    .map_err(|_| anyhow!("The bot cannot access the Discord channel"))?;
                ensure!(matches!(info, Channel::Guild(info) if info.guild_id == guild && info.kind == ChannelType::Voice),
                    "Select a regular Discord voice channel on the configured server");
                let current = position.wait_for(|value| *value != Position::Unknown).await
                    .map_err(|_| anyhow!("The Discord server is unavailable"))?;
                ensure!(*current == Position::Away, "The bot is already in a voice channel on this server");
                Ok::<_, anyhow::Error>(())
            }) => result.context("Discord server did not report the bot voice state")??,
        }
        let manager = songbird::get(&ctx).await.context("Discord voice client is unavailable")?.clone();
        capture_channel(&manager, &ctx, guild, channel, start, duration, capture, &mut stop, &mut position).await
    }.await;
    shards.shutdown_all().await;
    gateway.abort();
    result
}

fn not_joined() -> VoiceOutcome {
    VoiceOutcome {
        manifest: None,
        disconnected: true,
        capture_started_at: None,
    }
}

fn stop_before_join(stop: &StopRx) -> Result<VoiceOutcome> {
    ensure!(
        *stop.borrow() == Some("stopped_by_operator"),
        "Discord connection closed before the bot joined"
    );
    Ok(not_joined())
}

async fn resolve_speaker_names(guild: GuildId, http: &Http, capture: &Receiver) {
    let mut lookups = MemberLookups::default();
    let mut published = HashMap::new();
    loop {
        let now = Instant::now();
        let (users, live) = {
            let state = capture.state.lock().unwrap();
            let users = state
                .users
                .iter()
                .copied()
                .map(UserId::new)
                .collect::<Vec<_>>();
            let live = state
                .live_at(now)
                .map(|(audio, ms)| (audio.sink.clone(), ms));
            (users, live)
        };
        let (next, updates) = {
            let names = capture.names.lock().unwrap();
            let next = lookups.next(&users, &names, now);
            let updates = live.as_ref().map(|_| {
                users
                    .iter()
                    .filter_map(|user| {
                        let name = names.by_user.get(user)?;
                        (published.get(user) != Some(name)).then(|| (*user, name.clone()))
                    })
                    .collect::<Vec<_>>()
            });
            (next, updates)
        };
        if let (Some((sink, start_ms)), Some(updates)) = (live, updates) {
            for (user, name) in updates {
                if capture.stop.borrow().is_some() {
                    break;
                }
                published.insert(user, name.clone());
                sink(LiveVoiceFrame {
                    user_id: user.get(),
                    display_name: Some(name),
                    start_ms,
                    samples: Vec::new(),
                });
            }
        }
        let Some((user, retry)) = next else {
            tokio::select! {
                _ = sleep(Duration::from_secs(1)) => {},
                _ = capture.names_needed.notified() => {},
            }
            continue;
        };
        // Same-guild member requests share a REST bucket. Let Serenity pace one at a time.
        let transient = match timeout(MEMBER_LOOKUP_TIMEOUT, guild.member(http, user)).await {
            Ok(Ok(member)) => {
                capture.names.lock().unwrap().remember_rest(user, &member);
                false
            }
            Ok(Err(serenity::Error::Http(HttpError::Request(error)))) => {
                error.is_connect() || error.is_timeout()
            }
            Ok(Err(serenity::Error::Http(error))) => error
                .status_code()
                .is_some_and(|status| status.as_u16() == 429 || status.is_server_error()),
            Err(_) => true,
            _ => false,
        };
        lookups.finish(user, retry, transient, Instant::now());
    }
}

async fn capture_channel(
    manager: &songbird::Songbird,
    ctx: &Context,
    guild: GuildId,
    channel: ChannelId,
    start: RecordingStart,
    duration: Option<Duration>,
    capture: &Receiver,
    stop: &mut StopRx,
    position: &mut watch::Receiver<Position>,
) -> Result<VoiceOutcome> {
    let call = manager.get_or_insert(guild);
    {
        let mut call = call.lock().await;
        call.mute(true)
            .await
            .map_err(|_| anyhow!("Could not mute the Discord bot"))?;
        for event in [
            CoreEvent::SpeakingStateUpdate,
            CoreEvent::ClientDisconnect,
            CoreEvent::DriverDisconnect,
            CoreEvent::DriverReconnect,
            CoreEvent::VoiceTick,
        ] {
            call.add_global_event(event.into(), capture.clone());
        }
    }
    ensure!(
        *position.borrow() == Position::Away,
        "The bot is already in a voice channel on this server"
    );
    if stop.borrow().is_some() {
        return stop_before_join(stop);
    }
    let prepared = matches!(&start, RecordingStart::Prepared(_));
    let joined = tokio::select! {
        result = timeout(SETUP_TIMEOUT, async {
            if prepared {
                // No media socket during preparation, so no audio can be buffered before Record.
                manager.join_gateway(guild, channel).await
                    .map_err(|_| anyhow!("The bot could not join the Discord voice channel"))?;
            } else {
                manager.join(guild, channel).await
                    .map_err(|_| anyhow!("The bot could not join the Discord voice channel"))?;
            }
            position.wait_for(|state| *state == Position::In(channel)).await
                .map_err(|_| anyhow!("Discord did not confirm that the bot joined"))?;
            Ok::<_, anyhow::Error>(())
        }) => result.context("The Discord voice channel did not answer in time").and_then(|result| result),
        _ = stop.changed() => Err(anyhow!("Discord stopped while the bot was joining")),
    };
    if let Err(error) = joined {
        let disconnected = leave(manager, guild, channel, position, false).await;
        return end_before_capture(disconnected, stop, error);
    }

    let target = match start {
        RecordingStart::Immediate(target) => Some(target),
        RecordingStart::Prepared(receiver) => {
            capture.status.send_replace(VoiceStatus::Connected);
            tokio::select! {
                biased;
                _ = stop.changed() => None,
                target = receiver => target.ok(),
            }
        }
    };
    let Some(RecordingTarget {
        dir,
        local_epoch,
        live_audio,
    }) = target.filter(|_| stop.borrow().is_none())
    else {
        let disconnected = leave(manager, guild, channel, position, true).await;
        return end_before_capture(
            disconnected,
            stop,
            anyhow!("The Discord connection ended before recording started"),
        );
    };

    if prepared {
        let connected = tokio::select! {
            biased;
            _ = stop.changed() => Err(anyhow!("Discord stopped before recording started")),
            result = timeout(SETUP_TIMEOUT, async {
                let connect = {
                    let mut call = call.lock().await;
                    ensure!(call.current_channel() == Some(channel.into()), "The prepared Discord channel changed");
                    let connection = call.current_connection().cloned()
                        .context("The prepared Discord connection expired")?;
                    call.connect(connection)
                };
                connect.await.map_err(|_| anyhow!("Could not open the prepared Discord audio connection"))
            }) => result.context("The prepared Discord audio connection timed out").and_then(|result| result),
        };
        if let Err(error) = connected {
            let disconnected = leave(manager, guild, channel, position, true).await;
            return end_before_capture(disconnected, stop, error);
        }
    }

    let output_dir = dir.clone();
    let created = tokio::task::spawn_blocking(move || fs::create_dir(output_dir)).await;
    if !matches!(created, Ok(Ok(()))) {
        let disconnected = leave(manager, guild, channel, position, true).await;
        ensure!(disconnected, "Could not create the Discord recording folder, and Discord did not confirm that the bot left");
        return Err(anyhow!("Could not create the Discord recording folder"));
    }
    if stop.borrow().is_some() || *position.borrow() != Position::In(channel) {
        let output_dir = dir.clone();
        let _ = tokio::task::spawn_blocking(move || fs::remove_dir(output_dir)).await;
        let disconnected = leave(manager, guild, channel, position, true).await;
        return end_before_capture(
            disconnected,
            stop,
            anyhow!("The Discord connection ended before recording started"),
        );
    }
    let (tx, rx) = mpsc::channel(128);
    let output_dir = dir.clone();
    let mut writer = tokio::task::spawn_blocking(move || Tracks::collect(output_dir, rx));
    let capture_started_at = Instant::now();
    capture.status.send_replace(VoiceStatus::Connected);
    {
        let mut state = capture.state.lock().unwrap();
        if let Some(epoch) = local_epoch {
            state.clock.set_local_epoch(epoch);
        }
        state.live_audio = live_audio;
        state.writer = Some(tx);
    }
    let mut writer_result = None;
    let resolve_names = resolve_speaker_names(guild, &ctx.http, capture);
    tokio::pin!(resolve_names);
    let finish_reason = tokio::select! {
        _ = stop.changed() => (*stop.borrow()).unwrap_or("gateway_stopped"),
        result = &mut writer => {
            writer_result = Some(result);
            (*stop.borrow()).unwrap_or("writer_stopped")
        }
        _ = async {
            match duration {
                Some(duration) => sleep(duration.saturating_add(Duration::from_secs(3))).await,
                None => pending::<()>().await,
            }
        } => "voice_tick_timeout",
        _ = &mut resolve_names => unreachable!("speaker name worker runs until recording ends"),
    };
    let state = {
        let mut state = capture.state.lock().unwrap();
        let mut final_state = std::mem::replace(&mut *state, Capture::new(duration));
        final_state.writer.take();
        final_state.live_audio.take();
        final_state
    };
    let timeline = state.clock.close(Instant::now());
    let end_tick = state.ticks;
    // Allow in-flight lookups to complete during normal shutdown, without extending it.
    let (disconnected, mut tracks) = tokio::select! {
        result = async {
            let disconnected = leave(manager, guild, channel, position, true).await;
            let tracks = match writer_result {
                Some(result) => result,
                None => writer.await,
            }.context("Discord track writer stopped")??;
            let tracks = tokio::task::spawn_blocking(move || tracks.finish(end_tick)).await??;
            Ok::<_, anyhow::Error>((disconnected, tracks))
        } => result?,
        _ = &mut resolve_names => unreachable!("speaker name worker runs until recording ends"),
    };
    {
        let names = capture.names.lock().unwrap();
        for track in &mut tracks {
            if let Some(name) = names.by_user.get(&UserId::new(track.user_id.parse()?)) {
                track.display_name = name.to_string();
            }
        }
    }
    let manifest = Manifest {
        schema_version: 1,
        sample_rate: SAMPLE_RATE,
        duration_ms: end_tick * TICK_MS,
        finish_reason: finish_reason.to_string(),
        dropped_ticks: state.dropped_ticks,
        unmapped_frames: state.unmapped_frames,
        invalid_frames: state.invalid_frames,
        tracks,
        timeline: Some(timeline),
    };
    let manifest = tokio::task::spawn_blocking(move || {
        save_manifest(&dir, &manifest)?;
        Ok::<_, anyhow::Error>(manifest)
    })
    .await??;
    Ok(VoiceOutcome {
        manifest: Some(manifest),
        disconnected,
        capture_started_at: Some(capture_started_at),
    })
}

fn end_before_capture(
    disconnected: bool,
    stop: &StopRx,
    error: anyhow::Error,
) -> Result<VoiceOutcome> {
    if *stop.borrow() == Some("stopped_by_operator") {
        return Ok(VoiceOutcome {
            disconnected,
            ..not_joined()
        });
    }
    ensure!(
        disconnected,
        "{error}. Discord did not confirm that the bot left"
    );
    Err(error)
}

async fn leave(
    manager: &songbird::Songbird,
    guild: GuildId,
    channel: ChannelId,
    position: &mut watch::Receiver<Position>,
    joined: bool,
) -> bool {
    let previous = *position.borrow_and_update();
    // If Discord moved the bot elsewhere, do not send a leave into that channel.
    if matches!(previous, Position::In(other) if other != channel) {
        return false;
    }
    if joined && previous == Position::Away {
        return true;
    }
    matches!(
        timeout(LEAVE_TIMEOUT, async {
            manager.remove(guild).await.ok()?;
            // An initial Away snapshot may predate an in-flight join request.
            position.changed().await.ok()?;
            let confirmed = position
                .wait_for(|state| *state != Position::In(channel))
                .await
                .ok()?;
            Some(*confirmed == Position::Away)
        })
        .await,
        Ok(Some(true))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u64, nick: &str) -> Member {
        let mut member = Member::default();
        member.user.id = UserId::new(id);
        member.nick = Some(nick.to_owned());
        member
    }

    #[test]
    fn gateway_member_name_overrides_rest_and_stays_keyed_by_user() {
        let user = UserId::new(10);
        let other = UserId::new(20);
        let mut names = SpeakerNames::default();

        names.remember_rest(user, &member(20, "Wrong member"));
        names.remember_gateway(user, &member(10, " "));
        assert!(!names.by_user.contains_key(&user));

        names.remember_rest(user, &member(10, "Old nickname"));
        names.remember_gateway(user, &member(10, "Shared nickname"));
        names.remember_rest(user, &member(10, "Stale REST response"));
        names.remember_gateway(other, &member(20, "Shared nickname"));
        assert_eq!(names.by_user[&user].as_ref(), "Shared nickname");
        assert_eq!(names.by_user[&other].as_ref(), "Shared nickname");
        assert_eq!(names.by_user.len(), 2);
    }

    #[test]
    fn member_lookup_prioritizes_late_speaker_and_retries_transient_failure_once() {
        let first = UserId::new(10);
        let late = UserId::new(20);
        let now = Instant::now();
        let cooled = now + MEMBER_LOOKUP_COOLDOWN;
        let names = SpeakerNames::default();
        let mut lookups = MemberLookups::default();

        assert_eq!(lookups.next(&[first], &names, now), Some((first, false)));
        lookups.finish(first, false, true, now);
        assert_eq!(lookups.next(&[first], &names, now), None);
        assert_eq!(
            lookups.next(&[first, late], &names, cooled),
            Some((late, false))
        );
        lookups.finish(late, false, false, cooled);
        assert_eq!(
            lookups.next(&[first, late], &names, cooled),
            Some((first, true))
        );
        lookups.finish(first, true, true, cooled);
        assert_eq!(
            lookups.next(&[first, late], &names, cooled + MEMBER_LOOKUP_COOLDOWN),
            None
        );
    }

    #[test]
    fn clock_preserves_late_first_tick_pause_and_callback_gaps() {
        let mut clock = CaptureClock::new();
        let origin = clock.origin;
        clock.tick(0, origin + Duration::from_millis(230));
        clock.tick(1, origin + Duration::from_millis(250));
        clock.pause();
        clock.tick(2, origin + Duration::from_millis(290));
        clock.tick(3, origin + Duration::from_millis(410));
        clock.writer_drop(1);
        clock.writer_drop(2);
        let timeline = clock.close(origin + Duration::from_millis(424));

        assert!(timeline.pause_seen);
        assert_eq!(timeline.capture_closed_elapsed_us, 424_000);
        assert_eq!(
            timeline
                .spans
                .iter()
                .map(|span| (span.start_tick, span.end_tick, span.first_elapsed_us))
                .collect::<Vec<_>>(),
            [(0, 2, 230_000), (2, 3, 290_000), (3, 4, 410_000)]
        );
        assert_eq!(
            timeline.writer_drops,
            [TickRange {
                start_tick: 1,
                end_tick: 3
            }]
        );
        assert!(!timeline.truncated);
    }

    #[test]
    fn live_clock_anchors_late_ticks_and_excludes_paused_samples() {
        let origin = Instant::now();
        let mut capture = Capture::new(None);
        assert!(capture
            .live_at(origin + Duration::from_millis(100))
            .is_none());
        capture.live_audio = Some(LiveAudio {
            origin,
            sink: Arc::new(|_| {}),
        });
        let at = |ms| origin + Duration::from_millis(ms);
        assert!(capture.live_at(at(19)).is_none());
        assert_eq!(capture.live_at(at(2_120)).map(|(_, ms)| ms), Some(2_100));
        capture.set_paused(true, at(2_200));
        capture.set_paused(true, at(2_300));
        assert!(capture.live_at(at(2_400)).is_none());
        capture.set_paused(false, at(3_200));
        assert!(capture.live_at(at(3_219)).is_none());
        assert_eq!(capture.live_at(at(3_220)).map(|(_, ms)| ms), Some(2_200));
        capture.set_paused(false, at(3_230));
        assert_eq!(capture.live_at(at(3_240)).map(|(_, ms)| ms), Some(2_220));
    }

    #[tokio::test]
    async fn prepared_session_can_be_adopted_once_and_cancelled_without_a_folder() -> Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("discord");
        let mut session = VoiceSession::prepare(1, 2, String::new());
        session.adopt_with_live_audio(
            dir.clone(),
            (Instant::now(), 1),
            Some(Arc::new(|_| {
                panic!("cancelled preparation forwarded audio")
            })),
        )?;
        assert!(session.adopt(dir.clone(), (Instant::now(), 1)).is_err());
        let outcome = session.stop().await?;

        assert!(outcome.disconnected);
        assert!(outcome.manifest.is_none());
        assert!(!dir.exists());
        Ok(())
    }

    #[tokio::test]
    async fn stopping_before_the_worker_starts_does_not_open_an_output_folder() -> Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("discord");
        // A current-thread runtime has not polled the worker before this stop.
        let session = VoiceSession::start(1, 2, dir.clone(), String::new());
        let status = session.subscribe();
        let outcome = session.stop().await?;

        assert_eq!(*status.borrow(), VoiceStatus::Stopped);
        assert!(outcome.disconnected);
        assert!(outcome.manifest.is_none());
        assert!(!dir.exists());
        Ok(())
    }
}

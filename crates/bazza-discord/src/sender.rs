use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure, Context as _, Result};
use serenity::{
    all::{
        Channel, ChannelId, ChannelType, Client, Context, EventHandler, GatewayIntents, Guild,
        GuildId, Http, Ready, ShardManager, UserId, VoiceState,
    },
    async_trait,
};
use songbird::{
    driver::{opus, DecodeMode},
    events::EventData,
    input::Input,
    tracks::{PlayMode, Track, TrackHandle},
    Config, Event, EventContext, EventHandler as TrackEventHandler, SerenityInit, Songbird,
    TrackEvent,
};
use tokio::{
    sync::{oneshot, watch, Mutex as AsyncMutex},
    task::{spawn_blocking, JoinHandle},
    time::timeout,
};

const SETUP_TIMEOUT: Duration = Duration::from_secs(20);
const LEAVE_TIMEOUT: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(20);
const OPUS_CLOCK: u32 = 48_000;
const MAX_WAV_SECONDS: u32 = 120;

pub struct DiscordSenderConfig {
    pub token: String,
    pub guild_id: String,
    pub channel_id: String,
}

/// Sender-side callbacks from the first 20 ms of mixer progression and natural track end.
/// These observations are not UDP send timestamps or acknowledgements from a receiver.
pub struct DiscordPlayback {
    pub started_at: Instant,
    pub ended_at: Instant,
    pub played_for: Duration,
}

/// One synthetic-audio test account with a warmed Gateway connection.
pub struct DiscordSender {
    user_id: String,
    guild: GuildId,
    channel: ChannelId,
    manager: Arc<Songbird>,
    position: watch::Receiver<Position>,
    attempted_join: Arc<AsyncMutex<bool>>,
    playback: AsyncMutex<()>,
    gateway: Option<GatewayTask>,
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
    position: watch::Sender<Position>,
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
        if new.guild_id == Some(self.guild) && new.user_id == self.bot {
            self.position
                .send_replace(new.channel_id.map_or(Position::Away, Position::In));
        }
    }
}

struct GatewayTask {
    shards: Arc<ShardManager>,
    task: JoinHandle<()>,
    shutdown_complete: bool,
}

impl Drop for GatewayTask {
    fn drop(&mut self) {
        self.task.abort();
        if !self.shutdown_complete {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let shards = self.shards.clone();
                runtime.spawn(async move {
                    let _ = timeout(LEAVE_TIMEOUT, shards.shutdown_all()).await;
                });
            }
        }
    }
}

impl DiscordSender {
    /// Authenticates the bot and validates the selected channel without joining voice.
    pub async fn connect(config: DiscordSenderConfig) -> Result<Self> {
        let guild_id: u64 = config
            .guild_id
            .parse()
            .context("Invalid Discord server ID")?;
        let channel_id: u64 = config
            .channel_id
            .parse()
            .context("Invalid Discord channel ID")?;
        ensure!(
            guild_id != 0 && channel_id != 0,
            "Invalid Discord server or channel ID"
        );
        let guild = GuildId::new(guild_id);
        let channel = ChannelId::new(channel_id);
        let (ready, ready_rx) = oneshot::channel();
        let (position_tx, mut position) = watch::channel(Position::Unknown);
        let (bot, mut client) = timeout(SETUP_TIMEOUT, async {
            let bot = Http::new(&config.token)
                .get_current_user()
                .await
                .map_err(|_| anyhow!("Could not authenticate the Discord sender bot"))?;
            ensure!(bot.bot, "Discord credential does not identify a bot");
            let client = Client::builder(
                &config.token,
                GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES,
            )
            .event_handler(Gateway {
                ready: Mutex::new(Some(ready)),
                bot: bot.id,
                guild,
                position: position_tx.clone(),
            })
            .register_songbird_from_config(Config::default().decode_mode(DecodeMode::Pass))
            .await
            .map_err(|_| anyhow!("Could not initialize the Discord sender bot"))?;
            Ok::<_, anyhow::Error>((bot.id, client))
        })
        .await
        .context("Discord sender setup timed out")??;
        drop(config.token);

        let shards = client.shard_manager.clone();
        let task = tokio::spawn(async move {
            let _ = client.start().await;
            position_tx.send_replace(Position::Unknown);
        });
        let gateway = GatewayTask {
            shards,
            task,
            shutdown_complete: false,
        };
        let manager = timeout(SETUP_TIMEOUT, async {
            let ctx = ready_rx
                .await
                .context("Discord did not acknowledge sender login")?;
            let info = channel
                .to_channel(&ctx.http)
                .await
                .map_err(|_| anyhow!("The sender bot cannot access the Discord channel"))?;
            ensure!(
                matches!(info, Channel::Guild(info) if info.guild_id == guild && info.kind == ChannelType::Voice),
                "Select a regular Discord voice channel on the configured server"
            );
            let current = *position
                .wait_for(|state| *state != Position::Unknown)
                .await
                .map_err(|_| anyhow!("The Discord server is unavailable"))?;
            ensure!(
                current == Position::Away,
                "The sender bot is already in voice on this server"
            );
            songbird::get(&ctx)
                .await
                .context("Discord sender voice client is unavailable")
        })
        .await
        .context("Discord sender login or channel validation timed out")??;
        Ok(Self {
            user_id: bot.to_string(),
            guild,
            channel,
            manager,
            position,
            attempted_join: Arc::new(AsyncMutex::new(false)),
            playback: AsyncMutex::new(()),
            gateway: Some(gateway),
        })
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub async fn join(&self) -> Result<()> {
        let mut attempted = self.attempted_join.lock().await;
        let mut position = self.position.clone();
        ensure!(
            !*attempted,
            "The sender bot has an outstanding voice session"
        );
        ensure!(
            *position.borrow() == Position::Away,
            "Discord sender bot is already in voice or its state is unavailable"
        );
        *attempted = true;
        let joined = timeout(SETUP_TIMEOUT, async {
            self.manager
                .join(self.guild, self.channel)
                .await
                .map_err(|_| anyhow!("The sender bot could not join Discord voice"))?;
            position
                .wait_for(|state| *state == Position::In(self.channel))
                .await
                .map_err(|_| anyhow!("Discord did not confirm the sender joined"))?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("Discord sender voice join timed out")
        .and_then(|result| result);
        if let Err(error) = joined {
            leave_voice(
                &self.manager,
                self.guild,
                self.channel,
                position,
                &mut attempted,
            )
            .await
            .with_context(|| error.to_string())?;
            return Err(error);
        }
        Ok(())
    }

    pub async fn leave(&self) -> Result<()> {
        let mut attempted = self.attempted_join.lock().await;
        leave_voice(
            &self.manager,
            self.guild,
            self.channel,
            self.position.clone(),
            &mut attempted,
        )
        .await
    }

    pub async fn close(mut self) -> Result<()> {
        self.leave().await?;
        if let Some(mut gateway) = self.gateway.take() {
            timeout(LEAVE_TIMEOUT, gateway.shards.shutdown_all())
                .await
                .context("Discord sender Gateway shutdown timed out")?;
            gateway.shutdown_complete = true;
        }
        Ok(())
    }

    pub async fn play_wav(&self, path: &Path) -> Result<DiscordPlayback> {
        let _playback = self.playback.lock().await;
        let path = path.to_owned();
        let (input, duration) = spawn_blocking(move || encode_wav(&path))
            .await
            .context("Discord WAV preparation stopped")??;
        let (started, started_rx) = oneshot::channel();
        let (ended, ended_rx) = oneshot::channel();
        let ended = Arc::new(Mutex::new(Some(ended)));
        let mut track = Track::new(input);
        track.events.add_event(
            EventData::new(
                Event::Delayed(TICK),
                PlaybackEvent::First(Mutex::new(Some(started))),
            ),
            Duration::ZERO,
        );
        for event in [TrackEvent::End, TrackEvent::Error] {
            track.events.add_event(
                EventData::new(event.into(), PlaybackEvent::Terminal(ended.clone())),
                Duration::ZERO,
            );
        }
        let mut position = self.position.clone();
        let playback = {
            let attempted = self.attempted_join.lock().await;
            ensure!(
                *attempted && *position.borrow() == Position::In(self.channel),
                "The sender bot is outside the test channel"
            );
            let call = self
                .manager
                .get(self.guild)
                .context("Discord sender has no voice driver")?;
            let mut call = call.lock().await;
            ensure!(
                *position.borrow() == Position::In(self.channel),
                "The sender bot left the test channel"
            );
            ActiveTrack(call.play(track))
        };
        let result = tokio::select! {
            biased;
            _ = position.wait_for(|state| *state != Position::In(self.channel)) => {
                Err(anyhow!("The sender bot left the test channel during playback"))
            }
            result = timeout(duration + LEAVE_TIMEOUT, async {
                let (ended_at, played_for) = ended_rx.await.context("Discord sender playback stopped")??;
                let started_at = started_rx.await.context("Discord sender did not start playback")?;
                Ok(DiscordPlayback { started_at, ended_at, played_for })
            }) => result.context("Discord sender playback timed out")?,
        };
        drop(playback);
        result
    }
}

impl Drop for DiscordSender {
    fn drop(&mut self) {
        if let (Some(gateway), Ok(runtime)) =
            (self.gateway.take(), tokio::runtime::Handle::try_current())
        {
            let manager = self.manager.clone();
            let guild = self.guild;
            let channel = self.channel;
            let position = self.position.clone();
            let attempted_join = self.attempted_join.clone();
            runtime.spawn(async move {
                let mut attempted = attempted_join.lock().await;
                let _ = leave_voice(&manager, guild, channel, position, &mut attempted).await;
                drop(gateway);
            });
        }
    }
}

async fn leave_voice(
    manager: &Songbird,
    guild: GuildId,
    channel: ChannelId,
    mut position: watch::Receiver<Position>,
    attempted: &mut bool,
) -> Result<()> {
    if !*attempted {
        return Ok(());
    }
    let previous = *position.borrow_and_update();
    ensure!(
        !matches!(previous, Position::In(other) if other != channel),
        "Discord moved the sender bot outside the test channel"
    );
    timeout(LEAVE_TIMEOUT, async {
        manager
            .remove(guild)
            .await
            .map_err(|_| anyhow!("Could not leave Discord sender voice"))?;
        position
            .changed()
            .await
            .context("Discord sender Gateway closed during leave")?;
        let current = position
            .wait_for(|state| *state != Position::In(channel))
            .await
            .context("Discord sender Gateway closed during leave")?;
        ensure!(
            *current == Position::Away,
            "Discord did not confirm the sender left voice"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Discord did not confirm the sender left voice in time")??;
    *attempted = false;
    Ok(())
}

struct ActiveTrack(TrackHandle);

impl Drop for ActiveTrack {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

type TerminalPlayback = Result<(Instant, Duration)>;

enum PlaybackEvent {
    First(Mutex<Option<oneshot::Sender<Instant>>>),
    Terminal(Arc<Mutex<Option<oneshot::Sender<TerminalPlayback>>>>),
}

#[async_trait]
impl TrackEventHandler for PlaybackEvent {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let observed = Instant::now();
        match self {
            Self::First(sender) => {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let _ = sender.send(observed);
                }
            }
            Self::Terminal(sender) => {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let result = match ctx {
                        EventContext::Track([(state, _)])
                            if matches!(state.playing, PlayMode::End) =>
                        {
                            Ok((observed, state.play_time))
                        }
                        _ => Err(anyhow!("Discord sender audio did not finish normally")),
                    };
                    let _ = sender.send(result);
                }
            }
        }
        Some(Event::Cancel)
    }
}

pub fn validate_sender_wav(path: &Path) -> Result<Duration> {
    let (mut reader, duration) = open_wav(path)?;
    for sample in reader.samples::<i16>() {
        sample.context("Could not read a sample from the Discord sender WAV")?;
    }
    Ok(duration)
}

fn open_wav(
    path: &Path,
) -> Result<(
    hound::WavReader<std::io::BufReader<std::fs::File>>,
    Duration,
)> {
    ensure!(
        path.metadata()?.is_file(),
        "Discord sender WAV must be a regular file"
    );
    let reader = hound::WavReader::open(path).context("Could not open the Discord sender WAV")?;
    let spec = reader.spec();
    ensure!(
        matches!(spec.sample_rate, 24_000 | OPUS_CLOCK)
            && matches!(spec.channels, 1 | 2)
            && spec.bits_per_sample == 16
            && spec.sample_format == hound::SampleFormat::Int,
        "Discord sender WAV must be 24 or 48 kHz mono or stereo signed 16-bit PCM"
    );
    let frames = reader.duration();
    ensure!(
        (spec.sample_rate / 50..=MAX_WAV_SECONDS * spec.sample_rate).contains(&frames),
        "Discord sender WAV must be between 20 milliseconds and 120 seconds long"
    );
    let duration = Duration::from_secs_f64(f64::from(frames) / f64::from(spec.sample_rate));
    Ok((reader, duration))
}

fn encode_wav(path: &Path) -> Result<(Input, Duration)> {
    let (mut wav, duration) = open_wav(path)?;
    let spec = wav.spec();
    let samples_per_frame = spec.sample_rate as usize / 50;
    let metadata = serde_json::to_vec(&serde_json::json!({
        "dca": { "version": 1, "tool": { "name": "Bazza Discord probe", "version": "1" } },
        "opus": { "mode": "audio", "sample_rate": OPUS_CLOCK, "frame_size": OPUS_CLOCK / 50,
                  "vbr": true, "channels": 2 }
    }))?;
    let mut dca = b"DCA1".to_vec();
    dca.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    dca.extend_from_slice(&metadata);

    // Feed Songbird's built-in DCA/Opus path. The pinned workspace does not enable
    // Symphonia's PCM decoder, which Songbird's RawAdapter would require.
    let mut encoder = opus::Encoder::new(
        spec.sample_rate,
        opus::Channels::Stereo,
        opus::Application::Audio,
    )
    .context("Could not initialize the Discord sender Opus encoder")?;
    let frame_count = (wav.duration() as usize).div_ceil(samples_per_frame);
    let mut samples = wav.samples::<i16>();
    let mut pcm = vec![0_i16; samples_per_frame * 2];
    let mut packet = [0_u8; 1275];
    for _ in 0..frame_count {
        for stereo in pcm.chunks_exact_mut(2) {
            stereo[0] = samples.next().transpose()?.unwrap_or_default();
            stereo[1] = if spec.channels == 2 {
                samples.next().transpose()?.unwrap_or_default()
            } else {
                stereo[0]
            };
        }
        let len = encoder
            .encode(&pcm, &mut packet)
            .context("Discord sender Opus encoding failed")?;
        dca.extend_from_slice(&(len as u16).to_le_bytes());
        dca.extend_from_slice(&packet[..len]);
    }
    Ok((Input::from(dca), duration))
}

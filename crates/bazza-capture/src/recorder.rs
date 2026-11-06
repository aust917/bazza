//! Mic + system-audio recorder producing archive WAVs and a live 16 kHz feed.

use crate::devices::{find_device, find_selected_device};
use crate::dsp::{rms_db, Resampler};
use crate::stream::{open_microphone_timed, open_monitored_loopback_timed, unix_now_us, AudioCaptureTime, InputStream, StreamError};
use crate::wav::WavWriter;
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[path = "buffered_mixer.rs"]
mod buffered_mixer;
use buffered_mixer::{AlignedBlock, BufferedMixer};

pub const ARCHIVE_HZ: u32 = 48_000;
pub const ASR_HZ: u32 = 16_000;
const TICK: Duration = Duration::from_millis(100);
/// Timeout for a microphone callback or a successful WASAPI output poll.
const STALL_AFTER: Duration = Duration::from_secs(3);
const MIC: usize = 0;
const SYS: usize = 1;

#[derive(Debug, Clone)]
pub struct RecordConfig {
    pub dir: PathBuf,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub capture_system: bool,
}

#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// Every ~100 ms.
    Levels { mic_db: f32, sys_db: f32 },
    /// Mixed mono 16 kHz audio, ~100 ms per message. Paused intervals are excluded.
    Pcm16k(Vec<f32>),
    /// System audio on the same sample clock as the mix, for bot captions.
    SystemPcm16k(Vec<f32>),
    /// Non-fatal: e.g. system audio unavailable, recording continues mic-only.
    Warning(String),
    /// A requested source could not be opened. Recovery retries while recording.
    AudioUnavailable { system: bool, detail: String },
    /// A source was reopened. Earlier samples from that source may be missing.
    AudioReconnected { system: bool },
    /// Fatal stream error; the recorder keeps running but data may be missing.
    Error(String),
    /// The archive writer stopped. Unlike device errors, reopening a stream
    /// cannot recover this worker; the owner must finish the recording.
    WriterError(String),
    /// A stream stopped delivering audio (device switched or unplugged). The
    /// owner should call [`Recorder::reopen_stalled`]; the recorder keeps going.
    Stalled { mic: bool, sys: bool },
}

#[derive(Debug, Clone)]
pub struct RecordOutput {
    pub mic_wav: PathBuf,
    pub sys_wav: Option<PathBuf>,
    pub mix_wav: PathBuf,
    pub duration: Duration,
}

/// Frame zero of mix.wav on the same Unix clock used by video capture. Source
/// timestamps are measured independently; no device-specific A/V offset is used.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioTiming {
    pub version: u32,
    pub start_unix_us: i64,
    #[serde(default)]
    pub first_mic_unix_us: Option<i64>,
    #[serde(default)]
    pub first_sys_unix_us: Option<i64>,
    #[serde(default)]
    pub estimated_timestamps: bool,
}

impl AudioTiming {
    fn save(&self, wav: &Path) -> std::io::Result<()> {
        let path = wav.with_extension("clock.json");
        let temp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temp, path)
    }
}

struct CapturedPacket {
    generation: u64,
    source_token: u64,
    active_start_frame: u64,
    time: AudioCaptureTime,
    samples: Vec<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("no microphone available")]
    NoMic,
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error("wav: {0}")]
    Wav(#[from] hound::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

struct Shared {
    emit_system_pcm: bool,
    mic_buf: Mutex<Vec<CapturedPacket>>,
    sys_buf: Mutex<Vec<CapturedPacket>>,
    paused: AtomicBool,
    stop: AtomicBool,
    /// Pause/resume closes the pending sample-alignment interval.
    boundary: AtomicU64,
    intent_unix_us: i64,
    paused_us: AtomicU64,
    interval_start_us: AtomicU64,
    sources: [SourceHealth; 2],
    epoch: Instant,
}

#[derive(Default)]
struct SourceHealth {
    token: AtomicU64,
    seen_ms: AtomicU64,
    failed: AtomicBool,
}

impl SourceHealth {
    fn needs_reopen(&self, now_ms: u64, monitor_silence: bool) -> bool {
        self.failed.load(Ordering::SeqCst) || (monitor_silence
            && now_ms.saturating_sub(self.seen_ms.load(Ordering::Relaxed)) > STALL_AFTER.as_millis() as u64)
    }
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn buffer(&self, source: usize) -> &Mutex<Vec<CapturedPacket>> {
        if source == MIC { &self.mic_buf } else { &self.sys_buf }
    }

    fn stalled_sources(&self, now_ms: u64, capture_system: bool) -> [bool; 2] {
        [self.sources[MIC].needs_reopen(now_ms, true),
            capture_system && self.sources[SYS].needs_reopen(now_ms, cfg!(windows))]
    }
}

pub struct Recorder {
    shared: Arc<Shared>,
    cfg: RecordConfig,
    tx: Sender<CaptureEvent>,
    unavailable_reported: [bool; 2],
    mic: Option<InputStream>,
    sys: Option<InputStream>,
    worker: Option<JoinHandle<Result<Duration, RecordError>>>,
    started: Instant,
    paused_total: Arc<Mutex<Duration>>,
    paused_since: Mutex<Option<Instant>>,
    out: RecordOutput,
}

impl Recorder {
    pub fn start(cfg: RecordConfig, tx: Sender<CaptureEvent>) -> Result<Recorder, RecordError> {
        Self::start_inner(cfg, tx, false)
    }

    pub fn start_with_system_pcm(cfg: RecordConfig, tx: Sender<CaptureEvent>) -> Result<Recorder, RecordError> {
        Self::start_inner(cfg, tx, true)
    }

    fn start_inner(cfg: RecordConfig, tx: Sender<CaptureEvent>, emit_system_pcm: bool) -> Result<Recorder, RecordError> {
        std::fs::create_dir_all(&cfg.dir)?;
        let shared = Arc::new(Shared {
            emit_system_pcm,
            mic_buf: Mutex::new(Vec::new()),
            sys_buf: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            boundary: AtomicU64::new(0),
            intent_unix_us: unix_now_us(),
            paused_us: AtomicU64::new(0),
            interval_start_us: AtomicU64::new(0),
            sources: Default::default(),
            epoch: Instant::now(),
        });

        // Microphone (required).
        let mic = open_mic(&cfg, &shared, &tx)?;
        // System audio (optional).
        let mut unavailable_reported = [false; 2];
        let sys = if cfg.capture_system {
            match open_sys(&cfg, &shared, &tx) {
                Ok(stream) => Some(stream),
                Err(error) => {
                    shared.sources[SYS].failed.store(true, Ordering::SeqCst);
                    unavailable_reported[SYS] = true;
                    let _ = tx.try_send(CaptureEvent::AudioUnavailable { system: true, detail: error.to_string() });
                    None
                }
            }
        } else { None };

        let mic_wav = cfg.dir.join("mic.wav");
        let sys_wav = cfg.capture_system.then(|| cfg.dir.join("sys.wav"));
        let mix_wav = cfg.dir.join("mix.wav");
        let out = RecordOutput { mic_wav: mic_wav.clone(), sys_wav: sys_wav.clone(), mix_wav: mix_wav.clone(), duration: Duration::ZERO };

        let worker = {
            let shared = shared.clone();
            let has_sys = cfg.capture_system;
            let tx = tx.clone();
            std::thread::Builder::new().name("bazza-capture".into()).spawn(move || {
                run_worker_reporting(shared, tx, has_sys, mic_wav, sys_wav, mix_wav)
            })?
        };

        let started = shared.epoch;
        Ok(Recorder {
            shared,
            cfg,
            tx,
            unavailable_reported,
            mic: Some(mic),
            sys,
            worker: Some(worker),
            started,
            paused_total: Arc::new(Mutex::new(Duration::ZERO)),
            paused_since: Mutex::new(None),
            out,
        })
    }

    pub fn pause(&self) {
        let _mic = self.shared.mic_buf.lock();
        let _sys = self.shared.sys_buf.lock();
        if !self.shared.paused.swap(true, Ordering::SeqCst) {
            *self.paused_since.lock() = Some(Instant::now());
            self.shared.boundary.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn resume(&self) {
        let _mic = self.shared.mic_buf.lock();
        let _sys = self.shared.sys_buf.lock();
        if self.shared.paused.swap(false, Ordering::SeqCst) {
            if let Some(t) = self.paused_since.lock().take() {
                *self.paused_total.lock() += t.elapsed();
            }
            self.shared.paused_us.store(self.paused_total.lock().as_micros().min(u64::MAX as u128) as u64, Ordering::SeqCst);
            self.shared.interval_start_us.store(self.shared.epoch.elapsed().as_micros().min(u64::MAX as u128) as u64, Ordering::SeqCst);
            self.shared.boundary.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::SeqCst)
    }

    /// Monotonic origin of the recording and its Unix timestamp in microseconds.
    pub fn time_origin(&self) -> (Instant, u64) {
        (self.started, u64::try_from(self.shared.intent_unix_us).unwrap_or_default())
    }

    /// Wall time recorded, excluding paused time.
    pub fn elapsed(&self) -> Duration {
        let paused_total = *self.paused_total.lock();
        let paused = paused_total + self.paused_since.lock().map(|t| t.elapsed()).unwrap_or_default();
        self.started.elapsed().saturating_sub(paused)
    }

    /// Rebuild the requested audio streams while keeping the archive writers.
    pub fn reopen(&mut self) -> Result<(), RecordError> {
        let mic = self.restart_source(MIC);
        if self.cfg.capture_system {
            let _ = self.restart_source(SYS);
        }
        mic
    }

    /// Recheck a queued stall event and reopen only the affected source.
    pub fn reopen_stalled(&mut self, mic: bool, sys: bool) {
        if self.is_paused() || self.shared.stop.load(Ordering::SeqCst) { return; }
        let sources = self.shared.stalled_sources(self.shared.now_ms(), self.cfg.capture_system);
        for (index, (requested, stalled)) in [mic, sys].into_iter().zip(sources).enumerate() {
            if requested && stalled {
                let _ = self.restart_source(index);
            }
        }
    }

    fn restart_source(&mut self, source: usize) -> Result<(), RecordError> {
        let health = &self.shared.sources[source];
        {
            let _gate = self.shared.buffer(source).lock();
            // Keep accepted packets and invalidate only this source's callbacks.
            health.token.fetch_add(1, Ordering::SeqCst);
            health.failed.store(false, Ordering::SeqCst);
            health.seen_ms.store(self.shared.now_ms(), Ordering::Relaxed);
        }
        let system = source == SYS;
        let stream = if system { &mut self.sys } else { &mut self.mic };
        if let Some(old) = stream.take() { old.dispose(); }
        let result = if system { open_sys(&self.cfg, &self.shared, &self.tx) }
            else { open_mic(&self.cfg, &self.shared, &self.tx) };
        match result {
            Ok(opened) => {
                *stream = Some(opened);
                health.seen_ms.fetch_max(self.shared.now_ms(), Ordering::Relaxed);
                if !health.failed.load(Ordering::SeqCst) {
                    self.unavailable_reported[source] = false;
                    let _ = self.tx.try_send(CaptureEvent::AudioReconnected { system });
                }
                Ok(())
            }
            Err(error) => {
                health.failed.store(true, Ordering::SeqCst);
                if !self.unavailable_reported[source] {
                    self.unavailable_reported[source] = true;
                    let _ = self.tx.try_send(CaptureEvent::AudioUnavailable { system, detail: error.to_string() });
                }
                Err(error)
            }
        }
    }

    /// True when an enabled source failed or could not be opened.
    pub fn stream_failed(&self) -> bool {
        self.shared.sources.iter().any(|source| source.failed.load(Ordering::SeqCst))
    }

    pub fn stop(mut self) -> Result<RecordOutput, RecordError> {
        self.shared.stop.store(true, Ordering::SeqCst);
        let duration = match self.worker.take() {
            Some(h) => h.join().map_err(|_| RecordError::Io(std::io::Error::other("capture thread panicked")))??,
            None => Duration::ZERO,
        };
        // Streams are disposed off-thread (see `InputStream::dispose`) so a dead
        // device can never hang the stop.
        if let Some(m) = self.mic.take() {
            m.dispose();
        }
        if let Some(s) = self.sys.take() {
            s.dispose();
        }
        let mut out = self.out.clone();
        out.duration = duration;
        if out.sys_wav.as_ref().is_some_and(|path| !path.is_file()) {
            out.sys_wav = None;
        }
        Ok(out)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // A later application startup/store error can discard the owner before
        // stop() is called. Let the worker finalize asynchronously; stream fields
        // already perform their platform-specific teardown when dropped.
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

/// cpal funnels transient glitches through the same callback as device loss.
pub fn is_fatal_stream_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    !(m.contains("underrun") || m.contains("overrun"))
}

fn open_mic(cfg: &RecordConfig, shared: &Arc<Shared>, tx: &Sender<CaptureEvent>) -> Result<InputStream, RecordError> {
    let mic_dev = find_device(cfg.mic_device.as_deref(), false).ok_or(RecordError::NoMic)?;
    let mic_shared = shared.clone();
    let token = shared.sources[MIC].token.load(Ordering::SeqCst);
    Ok(open_microphone_timed(
        &mic_dev,
        cfg.mic_device.is_none(),
        ARCHIVE_HZ,
        Arc::new(move |pcm, timing| {
            accept_packet(&mic_shared, MIC, pcm, timing, token);
        }),
        source_error(shared, MIC, token, tx),
    )?)
}

fn open_sys(cfg: &RecordConfig, shared: &Arc<Shared>, tx: &Sender<CaptureEvent>) -> Result<InputStream, RecordError> {
    let dev = find_selected_device(cfg.loopback_device.as_deref(), true).ok_or(StreamError::NoDevice)?;
    let sys_shared = shared.clone();
    let health_shared = shared.clone();
    let token = shared.sources[SYS].token.load(Ordering::SeqCst);
    Ok(open_monitored_loopback_timed(
        &dev,
        cfg.loopback_device.is_none(),
        ARCHIVE_HZ,
        Arc::new(move |pcm, timing| {
            accept_packet(&sys_shared, SYS, pcm, timing, token);
        }),
        source_error(shared, SYS, token, tx),
        Some(Arc::new(move || {
            let _gate = health_shared.sys_buf.lock();
            let health = &health_shared.sources[SYS];
            if health.token.load(Ordering::SeqCst) == token {
                health.seen_ms.store(health_shared.now_ms(), Ordering::Relaxed);
            }
        })),
    )?)
}

fn source_error(shared: &Arc<Shared>, source: usize, token: u64, tx: &Sender<CaptureEvent>) -> crate::stream::ErrorSink {
    let shared = shared.clone();
    let tx = tx.clone();
    Arc::new(move |error| {
        let _gate = shared.buffer(source).lock();
        let health = &shared.sources[source];
        if !shared.stop.load(Ordering::SeqCst) && health.token.load(Ordering::SeqCst) == token {
            // Buffer over/underruns are dropped packets, not a dead stream.
            let event = if is_fatal_stream_error(&error) {
                health.failed.store(true, Ordering::SeqCst);
                CaptureEvent::Error(error)
            } else { CaptureEvent::Warning(error) };
            let _ = tx.try_send(event);
        }
    })
}

fn accept_packet(shared: &Shared, source: usize, pcm: &[f32], time: AudioCaptureTime, source_token: u64) {
    let mut buffer = shared.buffer(source).lock();
    let health = &shared.sources[source];
    if shared.stop.load(Ordering::SeqCst)
        || health.token.load(Ordering::SeqCst) != source_token { return; }
    health.seen_ms.store(shared.now_ms(), Ordering::Relaxed);
    if shared.paused.load(Ordering::SeqCst) { return; }
    if let Some(mut packet) = packet_on_active_clock(pcm, time, shared.boundary.load(Ordering::SeqCst),
        shared.intent_unix_us, shared.paused_us.load(Ordering::SeqCst), shared.interval_start_us.load(Ordering::SeqCst)) {
        packet.source_token = source_token;
        buffer.push(packet);
    }
}

fn packet_on_active_clock(pcm: &[f32], time: AudioCaptureTime, generation: u64, origin_unix_us: i64,
    paused_us: u64, interval_start_us: u64) -> Option<CapturedPacket> {
    let capture_us = time.unix_us as i128 - origin_unix_us as i128;
    let trim_us = (interval_start_us as i128 - capture_us).max(0) as u128;
    let trim = (trim_us * ARCHIVE_HZ as u128).div_ceil(1_000_000).min(pcm.len() as u128) as usize;
    if trim == pcm.len() { return None; }
    let start_frame = ((capture_us - paused_us as i128) * ARCHIVE_HZ as i128 + 500_000).div_euclid(1_000_000)
        + trim as i128;
    Some(CapturedPacket {
        generation, source_token: 0, active_start_frame: start_frame.max(0).min(u64::MAX as i128) as u64,
        time: AudioCaptureTime { unix_us: time.unix_us.saturating_add((trim as u128 * 1_000_000 / ARCHIVE_HZ as u128) as i64), ..time },
        samples: pcm[trim..].to_vec(),
    })
}

fn write_silence(writer: &mut WavWriter, mut frames: u64) -> Result<(), RecordError> {
    let zeros = [0.0; ARCHIVE_HZ as usize / 10];
    while frames > 0 {
        let count = frames.min(zeros.len() as u64) as usize;
        writer.write(&zeros[..count])?;
        frames -= count as u64;
    }
    Ok(())
}

/// Trust measured discontinuities, not callback arrival jitter. One millisecond
/// tolerates timestamp rounding/resampling residuals without padding every packet.
fn placement(desired: u64, written: u64, samples: usize, measured: bool, first: bool) -> (u64, usize) {
    let tolerance = ARCHIVE_HZ as u64 / 1_000;
    if !measured || (!first && desired.abs_diff(written) <= tolerance) { return (0, 0); }
    if desired >= written { (desired - written, 0) }
    else { (0, written.saturating_sub(desired).min(samples as u64) as usize) }
}

struct WorkerExit(Arc<Shared>);
impl Drop for WorkerExit {
    fn drop(&mut self) { self.0.stop.store(true, Ordering::SeqCst); }
}

fn run_worker_reporting(
    shared: Arc<Shared>,
    tx: Sender<CaptureEvent>,
    has_sys: bool,
    mic_wav: PathBuf,
    sys_wav: Option<PathBuf>,
    mix_wav: PathBuf,
) -> Result<Duration, RecordError> {
    let result = run_worker(shared, tx.clone(), has_sys, mic_wav, sys_wav, mix_wav);
    if let Err(error) = &result {
        let _ = tx.send(CaptureEvent::WriterError(format!("Audio writer stopped: {error}")));
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    shared: Arc<Shared>,
    tx: Sender<CaptureEvent>,
    has_sys: bool,
    mic_wav: PathBuf,
    sys_wav: Option<PathBuf>,
    mix_wav: PathBuf,
) -> Result<Duration, RecordError> {
    let _close_callbacks_on_exit = WorkerExit(shared.clone());
    let mut mic_w = WavWriter::create(&mic_wav, ARCHIVE_HZ, 1)?;
    let mut sys_w = None;
    let mut mix_w = WavWriter::create(&mix_wav, ARCHIVE_HZ, 1)?;
    // Streams already deliver ARCHIVE_HZ; only the live feed needs another rate.
    let mut to16 = Resampler::new(ARCHIVE_HZ, ASR_HZ);
    let mut system_to16 = (has_sys && shared.emit_system_pcm).then(|| Resampler::new(ARCHIVE_HZ, ASR_HZ));

    let written_frames = Cell::new(0u64);
    let mut mixer = BufferedMixer::new(has_sys, ARCHIVE_HZ as usize * 3 / 10);
    let mut interval = None;
    let mut source_tokens = [None; 2];
    let mut interval_start_frame = 0u64;
    let mut mix_source_frames = [0u64; 2];
    let mut raw_frames = [0u64; 2];
    let mut timing = AudioTiming { version: 1, start_unix_us: shared.intent_unix_us,
        first_mic_unix_us: None, first_sys_unix_us: None, estimated_timestamps: false };
    let mut timing_saved = false;
    let mut timing_warning_sent = false;
    let mut alignment_warning_sent = false;
    let mut write_mix = |AlignedBlock { mixed, mut system }: AlignedBlock| -> Result<(), RecordError> {
        written_frames.set(written_frames.get() + mixed.len() as u64);
        mix_w.write(&mixed)?;
        let mixed16 = to16.process(&mixed);
        if !mixed16.is_empty() { let _ = tx.send(CaptureEvent::Pcm16k(mixed16)); }
        if let Some(resampler) = &mut system_to16 {
            system.resize(mixed.len(), 0.0);
            let pcm = resampler.process(&system);
            if !pcm.is_empty() { let _ = tx.send(CaptureEvent::SystemPcm16k(pcm)); }
        }
        Ok(())
    };
    let mut last_stall_report: [Option<Instant>; 2] = [None; 2];
    loop {
        std::thread::sleep(TICK);
        let (mic_packets, sys_packets, current_boundary, paused, stopping) = {
            let mut mic = shared.mic_buf.lock();
            let mut sys = shared.sys_buf.lock();
            (std::mem::take(&mut *mic), std::mem::take(&mut *sys),
                shared.boundary.load(Ordering::SeqCst), shared.paused.load(Ordering::SeqCst), shared.stop.load(Ordering::SeqCst))
        };
        let mut groups: BTreeMap<u64, [Vec<CapturedPacket>; 2]> = BTreeMap::new();
        for packet in mic_packets { groups.entry(packet.generation).or_default()[0].push(packet); }
        for packet in sys_packets { groups.entry(packet.generation).or_default()[1].push(packet); }
        let mut levels = [-100.0; 2];
        for (generation, sources) in groups {
            if interval != Some(generation) {
                loop { let tail = mixer.finish_block(ARCHIVE_HZ as usize / 10); if tail.mixed.is_empty() { break; } write_mix(tail)?; }
                interval = Some(generation); source_tokens = [None; 2];
                interval_start_frame = written_frames.get(); mix_source_frames = [0; 2];
            }
            let mut changed = false;
            for (index, packets) in sources.into_iter().enumerate() {
                let mut level_samples = Vec::new();
                for packet in packets {
                    let first = source_tokens[index] != Some(packet.source_token);
                    let (raw_gap, raw_trim) = placement(packet.active_start_frame, raw_frames[index], packet.samples.len(), first || packet.time.device_timestamp, first);
                    let samples = &packet.samples[raw_trim..];
                    let desired = packet.active_start_frame.saturating_add(raw_trim as u64);
                    let (mix_gap, mix_trim) = placement(desired, interval_start_frame + mix_source_frames[index], samples.len(), first || packet.time.device_timestamp, first);
                    if index == MIC { write_silence(&mut mic_w, raw_gap)?; mic_w.write(samples)?; }
                    else if let Some(path) = &sys_wav {
                        if sys_w.is_none() { sys_w = Some(WavWriter::create(path, ARCHIVE_HZ, 1)?); }
                        let writer = sys_w.as_mut().expect("system writer is open");
                        write_silence(writer, raw_gap)?;
                        writer.write(samples)?;
                    }
                    raw_frames[index] += raw_gap + samples.len() as u64;
                    mix_source_frames[index] += mix_gap + (samples.len() - mix_trim) as u64;
                    mixer.enqueue_source(index, &samples[mix_trim..], mix_gap as usize);
                    level_samples.extend_from_slice(samples);
                    source_tokens[index] = Some(packet.source_token);
                    let first_epoch = if index == 0 { &mut timing.first_mic_unix_us } else { &mut timing.first_sys_unix_us };
                    if first_epoch.is_none() { *first_epoch = Some(packet.time.unix_us); changed = true; }
                    if !packet.time.device_timestamp && !timing.estimated_timestamps {
                        timing.estimated_timestamps = true; changed = true;
                        let _ = tx.send(CaptureEvent::Warning("Device capture timestamps are unavailable; recording timing is estimated.".into()));
                    }
                }
                levels[index] = rms_db(&level_samples);
            }
            if changed {
                match timing.save(&mix_wav) {
                    Ok(()) => timing_saved = true,
                    Err(e) => { timing_saved = false; if !timing_warning_sent {
                        let _ = tx.send(CaptureEvent::Warning(format!("Could not save audio timing ({e}); original audio is still being recorded.")));
                        timing_warning_sent = true;
                    } }
                }
            }
            mixer.finish_enqueue();
            if mixer.is_degraded() && !alignment_warning_sent && source_tokens.iter().all(Option::is_some)
                && !shared.sources.iter().any(|source| source.failed.load(Ordering::SeqCst)) {
                let _ = tx.send(CaptureEvent::Warning("Audio sources fell out of sync. Late packets may be omitted from the mix; separate audio tracks are preserved.".into()));
                alignment_warning_sent = true;
            }
            loop { let block = mixer.drain_ready(ARCHIVE_HZ as usize / 10); if block.mixed.is_empty() { break; } write_mix(block)?; }
        }
        if paused || stopping || interval.is_some_and(|value| value != current_boundary) {
            loop { let tail = mixer.finish_block(ARCHIVE_HZ as usize / 10); if tail.mixed.is_empty() { break; } write_mix(tail)?; }
            interval = Some(current_boundary); source_tokens = [None; 2];
            interval_start_frame = written_frames.get(); mix_source_frames = [0; 2];
        }
        let _ = tx.send(CaptureEvent::Levels { mic_db: if paused { -100.0 } else { levels[0] }, sys_db: if paused || !has_sys { -100.0 } else { levels[1] } });
        if stopping { break; }
        if paused { continue; }
        let stalled = shared.stalled_sources(shared.now_ms(), has_sys);
        let mut retry = [false; 2];
        for (index, stalled) in stalled.into_iter().enumerate() {
            let last_report = &mut last_stall_report[index];
            if !stalled {
                *last_report = None;
            } else if last_report.is_none_or(|time| time.elapsed() >= Duration::from_secs(5)) {
                *last_report = Some(Instant::now());
                retry[index] = true;
            }
        }
        let [mic, sys] = retry;
        if mic || sys { let _ = tx.send(CaptureEvent::Stalled { mic, sys }); }
    }
    if !timing_saved && (timing.first_mic_unix_us.is_some() || timing.first_sys_unix_us.is_some()) {
        if let Err(e) = timing.save(&mix_wav) { let _ = tx.send(CaptureEvent::Warning(format!("Audio timing could not be saved for video processing: {e}"))); }
    }
    mic_w.finish()?;
    if let Some(w) = sys_w {
        w.finish()?;
    }
    mix_w.finish()?;
    Ok(Duration::from_secs_f64(written_frames.get() as f64 / ARCHIVE_HZ as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_for_test(origin: i64) -> Arc<Shared> {
        Arc::new(Shared {
            emit_system_pcm: false,
            mic_buf: Mutex::new(Vec::new()), sys_buf: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false), stop: AtomicBool::new(false),
            boundary: AtomicU64::new(0),
            intent_unix_us: origin, paused_us: AtomicU64::new(0), interval_start_us: AtomicU64::new(0),
            sources: Default::default(), epoch: Instant::now(),
        })
    }

    fn packet(generation: u64, start: u64, samples: Vec<f32>) -> CapturedPacket {
        CapturedPacket { generation, source_token: 0, active_start_frame: start,
            time: AudioCaptureTime { unix_us: 1_000_000 + (start * 1_000_000 / ARCHIVE_HZ as u64) as i64, device_timestamp: true }, samples }
    }

    fn read_wav(path: &Path) -> Vec<i16> {
        hound::WavReader::open(path).unwrap().into_samples().map(Result::unwrap).collect()
    }

    fn pcm(value: f32) -> i16 { (value * i16::MAX as f32) as i16 }

    fn fidelity_signal(len: usize) -> Vec<f32> {
        let mut random = 0x1234_5678u32;
        (0..len).map(|i| {
            random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (random as f64 / u32::MAX as f64 - 0.5) * 0.04;
            let tones: f64 = [997.0, 1733.0, 4523.0].into_iter().map(|hz| {
                0.17 * (std::f64::consts::TAU * hz * i as f64 / ARCHIVE_HZ as f64).sin()
            }).sum();
            (tones + noise) as f32
        }).collect()
    }

    fn render_fidelity_packets(packets: Vec<CapturedPacket>, frames: usize) -> (Vec<i16>, Vec<i16>) {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared_for_test(1_000_000);
        *shared.mic_buf.lock() = vec![packet(0, 0, vec![0.0; frames])];
        *shared.sys_buf.lock() = packets;
        shared.stop.store(true, Ordering::SeqCst);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let sys = dir.path().join("sys.wav"); let mix = dir.path().join("mix.wav");
        run_worker(shared, tx, true, dir.path().join("mic.wav"), Some(sys.clone()), mix.clone()).unwrap();
        (read_wav(&sys), read_wav(&mix))
    }

    #[test]
    fn native_microphone_frame_positions_preserve_archive_and_mix_under_qpc_jitter() {
        use crate::stream::NativePacketClock;
        let signal = fidelity_signal(95_777);
        let shared = shared_for_test(1_000_000);
        let mut clock = NativePacketClock::new(ARCHIVE_HZ);
        let sizes = [480, 257, 769, 511, 443, 3, 997];
        let jitter = [0i128, 50, -50, 73, -73, 96, -96];
        let mut start = 0usize;
        let mut packets = Vec::new();
        while start < signal.len() {
            let i = packets.len();
            let count = sizes[i % sizes.len()].min(signal.len() - start);
            let observed = AudioCaptureTime {
                unix_us: 1_010_000 + ((start as i128 + jitter[i % jitter.len()]) * 1_000_000 / ARCHIVE_HZ as i128) as i64,
                device_timestamp: true,
            };
            let time = clock.timestamp(observed, Some(91_000 + start as u64), count, i == 0);
            packets.push(packet_on_active_clock(&signal[start..start + count], time, 0, 1_000_000, 0, 0).unwrap());
            start += count;
        }
        *shared.mic_buf.lock() = packets;
        shared.stop.store(true, Ordering::SeqCst);
        let dir = tempfile::tempdir().unwrap();
        let mic = dir.path().join("mic.wav");
        let mix = dir.path().join("mix.wav");
        let (tx, _rx) = crossbeam_channel::unbounded();
        run_worker(shared, tx, false, mic.clone(), None, mix.clone()).unwrap();
        let mut expected = vec![0i16; 480];
        expected.extend(signal.into_iter().map(pcm));
        assert_eq!(read_wav(&mic), expected, "QPC jitter must not insert zeros or trim microphone samples");
        assert_eq!(read_wav(&mix), expected, "microphone-only mix must retain identical PCM");
    }

    #[test]
    fn native_frame_clock_preserves_raw_and_mix_pcm_under_qpc_jitter_and_changing_packet_sizes() {
        use crate::stream::NativePacketClock;
        let signal = fidelity_signal(95_777);
        let mut clock = NativePacketClock::new(ARCHIVE_HZ);
        let mut packets = Vec::new();
        let mut start = 0usize;
        let sizes = [480, 257, 769, 511, 443, 3, 997];
        let jitter = [0i128, 73, -73, 49, -49, 96, -96];
        while start < signal.len() {
            let i = packets.len();
            let count = sizes[i % sizes.len()].min(signal.len() - start);
            let observed = AudioCaptureTime {
                unix_us: 1_010_000 + ((start as i128 + jitter[i % jitter.len()]) * 1_000_000 / ARCHIVE_HZ as i128) as i64,
                device_timestamp: true,
            };
            let timing = clock.timestamp(observed, Some(7_123 + start as u64), count, i == 0);
            packets.push(packet_on_active_clock(&signal[start..start + count], timing, 0, 1_000_000, 0, 0).unwrap());
            start += count;
        }
        let mut expected = vec![0i16; 480];
        expected.extend(signal.into_iter().map(pcm));
        let (system, mixed) = render_fidelity_packets(packets, expected.len());
        assert_eq!(system, expected, "native QPC jitter must not insert zeros or delete original system PCM");
        assert_eq!(mixed, expected, "mixed PCM must preserve the same waveform after one measured source prefix");
    }

    #[test]
    fn native_frame_gaps_and_duplicate_packets_preserve_exact_waveform_and_real_silence() {
        use crate::stream::NativePacketClock;
        let signal = fidelity_signal(1_920);
        let mut clock = NativePacketClock::new(ARCHIVE_HZ);
        let mut packets = Vec::new();
        for (i, (native, pcm_start)) in [(10_000, 0usize), (10_000, 0), (11_440, 960)].into_iter().enumerate() {
            let observed = AudioCaptureTime { unix_us: 1_000_000 + i as i64 * 21_521, device_timestamp: true };
            let time = clock.timestamp(observed, Some(native), 960, i == 0 || i == 2);
            packets.push(packet_on_active_clock(&signal[pcm_start..pcm_start + 960], time, 0, 1_000_000, 0, 0).unwrap());
        }
        let mut expected: Vec<_> = signal[..960].iter().copied().map(pcm).collect();
        expected.extend([0i16; 480]);
        expected.extend(signal[960..].iter().copied().map(pcm));
        let (system, mixed) = render_fidelity_packets(packets, expected.len());
        assert_eq!(system, expected);
        assert_eq!(mixed, expected);
    }

    #[test]
    fn native_counter_reset_after_an_uncertain_packet_keeps_all_accepted_pcm() {
        use crate::stream::NativePacketClock;
        let signal = fidelity_signal(1_920);
        let mut clock = NativePacketClock::new(ARCHIVE_HZ);
        let mut packets = Vec::new();
        for (i, (position, discontinuity)) in [(Some(0), true), (None, true), (Some(480), false), (Some(960), false)].into_iter().enumerate() {
            let observed = AudioCaptureTime { unix_us: 1_010_000 + i as i64 * 11_521, device_timestamp: position.is_some() };
            let time = clock.timestamp(observed, position, 480, discontinuity);
            packets.push(packet_on_active_clock(&signal[i * 480..(i + 1) * 480], time, 0, 1_000_000, 0, 0).unwrap());
        }
        let mut expected = vec![0i16; 480];
        expected.extend(signal.into_iter().map(pcm));
        let (system, mixed) = render_fidelity_packets(packets, expected.len());
        assert_eq!(system, expected, "a counter epoch change must not trim valid PCM accepted after the uncertain packet");
        assert_eq!(mixed, expected);
    }

    #[test]
    fn estimated_process_frame_order_survives_pause_and_fresh_clock_reopen() {
        use crate::stream::NativePacketClock;
        let origin = 1_000_000;
        let dir = tempfile::tempdir().unwrap();
        let shared = shared_for_test(origin);
        let signal = fidelity_signal(13 * 480);
        let mut clock = NativePacketClock::new(ARCHIVE_HZ);
        for index in 0..11usize {
            if index == 4 {
                // The native source keeps consuming packets during the pause.
                shared.paused.store(true, Ordering::SeqCst);
                shared.boundary.store(1, Ordering::SeqCst);
            } else if index == 7 {
                shared.paused_us.store(30_000, Ordering::SeqCst);
                shared.interval_start_us.store(80_000, Ordering::SeqCst);
                shared.boundary.store(2, Ordering::SeqCst);
                shared.paused.store(false, Ordering::SeqCst);
            }
            let observed = AudioCaptureTime {
                unix_us: origin + 10_000 + index as i64 * 10_000 + [0, 1700, -900][index % 3],
                device_timestamp: true,
            };
            let timing = clock.timestamp(observed, None, 480, index == 0);
            assert!(!timing.device_timestamp);
            accept_packet(&shared, SYS,
                &signal[index * 480..(index + 1) * 480], timing, 0);
        }
        // Reopen gets a fresh native clock and a new callback token. Its first
        // packet starts 10 ms late; this one source prefix must be kept once.
        shared.sources[SYS].token.store(1, Ordering::SeqCst);
        let mut reopened = NativePacketClock::new(ARCHIVE_HZ);
        for index in 0..2usize {
            let observed = AudioCaptureTime { unix_us: origin + 130_000 + index as i64 * 10_000, device_timestamp: true };
            let timing = reopened.timestamp(observed, None, 480, index == 0);
            accept_packet(&shared, SYS,
                &signal[(index + 11) * 480..(index + 12) * 480], timing, 1);
        }
        let sys_packets = shared.sys_buf.lock();
        assert_eq!(sys_packets.len(), 10, "paused source packets must not enter the archive");
        assert!(sys_packets.iter().all(|packet| !packet.time.device_timestamp));
        drop(sys_packets);
        *shared.mic_buf.lock() = [(0, 0, 0, 0, 2400), (2, 80_000, 30_000, 80_000, 1920), (2, 120_000, 30_000, 80_000, 1440)]
            .into_iter().map(|(generation, wall, paused, interval, count)| {
                packet_on_active_clock(&vec![0.0; count], AudioCaptureTime { unix_us: origin + wall, device_timestamp: true },
                    generation, origin, paused, interval).unwrap()
            }).collect();
        shared.stop.store(true, Ordering::SeqCst);
        let (tx, rx) = crossbeam_channel::unbounded();
        let mic = dir.path().join("mic.wav"); let sys = dir.path().join("sys.wav"); let mix = dir.path().join("mix.wav");
        let duration = run_worker(shared, tx, true, mic.clone(), Some(sys.clone()), mix.clone()).unwrap();
        let mut expected = vec![0i16; 480];
        expected.extend(signal[..4 * 480].iter().copied().map(pcm));
        expected.extend(signal[7 * 480..11 * 480].iter().copied().map(pcm));
        expected.extend([0i16; 480]);
        expected.extend(signal[11 * 480..].iter().copied().map(pcm));
        assert_eq!(duration, Duration::from_millis(120));
        assert_eq!(read_wav(&sys), expected, "preserve all delivered active PCM and each source prefix once");
        assert_eq!(read_wav(&mix), expected, "mix must retain the same waveform across both boundaries");
        assert_eq!(read_wav(&mic), vec![0i16; expected.len()]);
        let timing: AudioTiming = serde_json::from_slice(&std::fs::read(dir.path().join("mix.clock.json")).unwrap()).unwrap();
        assert!(timing.estimated_timestamps);
        assert_eq!(timing.first_mic_unix_us, Some(origin));
        assert_eq!(timing.first_sys_unix_us, Some(origin + 10_000));
        assert_eq!(rx.try_iter().filter(|event| matches!(event, CaptureEvent::Warning(message) if message.contains("recording timing is estimated"))).count(), 1);
    }

    #[test]
    fn first_system_packet_after_pause_does_not_repeat_the_previous_interval() {
        let dir = tempfile::tempdir().unwrap();
        let mut shared = shared_for_test(1_000_000);
        Arc::get_mut(&mut shared).unwrap().emit_system_pcm = true;
        *shared.mic_buf.lock() = vec![packet(0, 0, vec![0.125; 4_800]), packet(2, 4_800, vec![0.25; 4_800])];
        *shared.sys_buf.lock() = vec![packet(2, 4_800, vec![0.5; 4_800])];
        shared.boundary.store(2, Ordering::SeqCst); shared.stop.store(true, Ordering::SeqCst);
        let (tx, rx) = crossbeam_channel::unbounded();
        let mic = dir.path().join("mic.wav"); let sys = dir.path().join("sys.wav"); let mix = dir.path().join("mix.wav");
        let duration = run_worker(shared, tx, true, mic, Some(sys.clone()), mix.clone()).unwrap();
        assert_eq!(duration, Duration::from_millis(200));
        let mixed = read_wav(&mix); let system = read_wav(&sys);
        assert_eq!(mixed.len(), 9_600); assert_eq!(system.len(), 9_600);
        assert!(mixed[..4_800].iter().all(|x| *x == pcm(0.125)));
        assert!(mixed[4_800..].iter().all(|x| *x == pcm(0.75)));
        assert!(system[..4_800].iter().all(|x| *x == 0));
        assert!(system[4_800..].iter().all(|x| *x == pcm(0.5)));
        let mut live_mix = Vec::new();
        let mut live_system = Vec::new();
        for event in rx.try_iter() {
            match event {
                CaptureEvent::Pcm16k(samples) => live_mix.extend(samples),
                CaptureEvent::SystemPcm16k(samples) => live_system.extend(samples),
                _ => {}
            }
        }
        assert_eq!(live_mix.len(), live_system.len());
        let input = [vec![0.0; 4_800], vec![0.5; 4_800]].concat();
        assert_eq!(live_system, Resampler::new(ARCHIVE_HZ, ASR_HZ).process(&input));
    }

    #[test]
    fn measured_mid_interval_gap_preserves_position_and_duplicate_packets_are_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared_for_test(1_000_000);
        *shared.mic_buf.lock() = vec![packet(0, 0, vec![0.125; 14_400])];
        *shared.sys_buf.lock() = vec![packet(0, 0, vec![0.25; 4_800]),
            packet(0, 0, vec![0.25; 4_800]), packet(0, 9_600, vec![0.5; 4_800])];
        shared.stop.store(true, Ordering::SeqCst);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mic = dir.path().join("mic.wav"); let sys = dir.path().join("sys.wav"); let mix = dir.path().join("mix.wav");
        run_worker(shared, tx, true, mic, Some(sys.clone()), mix.clone()).unwrap();
        let mixed = read_wav(&mix); let system = read_wav(&sys);
        assert_eq!(mixed.len(), 14_400); assert_eq!(system.len(), 14_400);
        assert!(mixed[..4_800].iter().all(|x| *x == pcm(0.375)));
        assert!(mixed[4_800..9_600].iter().all(|x| *x == pcm(0.125)));
        assert!(mixed[9_600..].iter().all(|x| *x == pcm(0.625)));
        assert!(system[4_800..9_600].iter().all(|x| *x == 0));
    }

    #[test]
    fn timestamp_rounding_and_estimated_callback_jitter_do_not_pad_each_packet() {
        assert_eq!(placement(4_824, 4_800, 4_800, true, false), (0, 0));
        assert_eq!(placement(4_776, 4_800, 4_800, true, false), (0, 0));
        assert_eq!(placement(9_600, 4_800, 4_800, false, false), (0, 0));
        assert_eq!(placement(9_600, 4_800, 4_800, true, false), (4_800, 0));
        assert_eq!(placement(0, 4_800, 4_800, true, false), (0, 4_800));
    }

    #[test]
    fn disposed_stream_callbacks_cannot_enter_a_reopened_recording() {
        let shared = shared_for_test(1_000_000);
        let (tx, rx) = crossbeam_channel::unbounded();
        let stale_error = source_error(&shared, SYS, 0, &tx);
        shared.sources[SYS].token.store(1, Ordering::SeqCst);
        let time = AudioCaptureTime { unix_us: 1_000_000, device_timestamp: true };
        shared.sources[SYS].seen_ms.store(u64::MAX, Ordering::Relaxed);
        accept_packet(&shared, SYS, &[0.25], time, 0);
        stale_error("old stream failed".into());
        assert!(shared.sys_buf.lock().is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(shared.sources[SYS].seen_ms.load(Ordering::Relaxed), u64::MAX);
        accept_packet(&shared, SYS, &[0.5], time, 1);
        accept_packet(&shared, MIC, &[0.125], time, 0);
        source_error(&shared, SYS, 1, &tx)("new stream failed".into());
        assert_eq!(shared.sys_buf.lock()[0].samples, vec![0.5]);
        assert_eq!(shared.mic_buf.lock()[0].samples, vec![0.125]);
        assert!(matches!(rx.try_recv(), Ok(CaptureEvent::Error(_))));
        assert_eq!(shared.stalled_sources(0, true), [false, true]);
        assert_eq!(shared.stalled_sources(0, false), [false, false]);
    }

    #[test]
    fn system_poll_distinguishes_silent_output_from_a_stalled_stream() {
        let health = SourceHealth::default();
        let now = 10_000;
        assert!(health.needs_reopen(now, true));
        health.seen_ms.store(now, Ordering::Relaxed);
        assert!(!health.needs_reopen(now, true));
        assert!(!health.needs_reopen(now + 5_000, false));
        health.failed.store(true, Ordering::SeqCst);
        assert!(health.needs_reopen(now, false));
    }

    #[test]
    fn system_audio_can_join_after_start_and_recover_without_restarting_the_mic() {
        let dir = tempfile::tempdir().unwrap();
        let origin = 1_000_000;
        let shared = shared_for_test(origin);
        let mic = dir.path().join("mic.wav");
        let sys = dir.path().join("sys.wav");
        let mix = dir.path().join("mix.wav");
        let (tx, rx) = crossbeam_channel::unbounded();
        shared.mic_buf.lock().push(packet(0, 0, vec![0.125; 19_200]));
        shared.sources[SYS].failed.store(true, Ordering::SeqCst);
        let worker = {
            let (shared, mic, sys, mix) = (shared.clone(), mic.clone(), sys.clone(), mix.clone());
            std::thread::spawn(move || run_worker(shared, tx, true, mic, Some(sys), mix))
        };
        assert!((0..20).any(|_| matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), CaptureEvent::Pcm16k(_))));
        assert!((0..20).any(|_| matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), CaptureEvent::Stalled { mic: false, sys: true })));
        assert!(!sys.exists(), "initially missing system audio must not create an empty track");

        // Feed current audio from two successive replacement system streams.
        // The microphone keeps its original token across both reopens.
        {
            let mut mic = shared.mic_buf.lock();
            let mut sys = shared.sys_buf.lock();
            mic.push(packet(0, 19_200, vec![0.125; 14_400]));
            for (token, start, value) in [(1, 19_200, 0.25), (2, 28_800, 0.5)] {
                let mut recovered = packet(0, start, vec![value; 4_800]);
                recovered.source_token = token;
                recovered.time.device_timestamp = false;
                sys.push(recovered);
            }
            shared.stop.store(true, Ordering::SeqCst);
        }
        assert_eq!(worker.join().unwrap().unwrap(), Duration::from_millis(700));
        let mixed = read_wav(&mix);
        let system = read_wav(&sys);
        let pcm = |value: f32| (value * i16::MAX as f32) as i16;
        assert_eq!(read_wav(&mic), vec![pcm(0.125); 33_600]);
        assert_eq!(mixed.len(), 33_600);
        assert_eq!(system.len(), mixed.len());
        for (range, sys_value) in [(0..19_200, 0.0), (19_200..24_000, 0.25), (24_000..28_800, 0.0), (28_800..33_600, 0.5)] {
            assert!(mixed[range.clone()].iter().all(|sample| *sample == pcm(0.125 + sys_value)));
            assert!(system[range].iter().all(|sample| *sample == pcm(sys_value)));
        }
    }

    #[test]
    fn writer_failure_stops_accepting_pcm() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared_for_test(1_000_000);
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(run_worker_reporting(shared.clone(), tx, false, dir.path().to_owned(), None, dir.path().join("mix.wav")).is_err());
        assert!(shared.stop.load(Ordering::SeqCst));
        assert!(matches!(rx.recv().unwrap(), CaptureEvent::WriterError(message) if message.contains("Audio writer stopped:")));
        assert!(rx.try_recv().is_err());
        accept_packet(&shared, MIC, &[0.25], AudioCaptureTime { unix_us: 1_000_000, device_timestamp: true }, 0);
        assert!(shared.mic_buf.lock().is_empty());
    }

    #[test]
    fn dropping_recorder_signals_worker_to_finalize_archives() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared_for_test(1_000_000);
        shared.mic_buf.lock().push(packet(0, 0, vec![0.25; 4_800]));
        let mic = dir.path().join("mic.wav");
        let mix = dir.path().join("mix.wav");
        let (tx, _rx) = crossbeam_channel::unbounded();
        let (finished, completion) = crossbeam_channel::bounded(1);
        let worker = {
            let shared = shared.clone();
            let mic = mic.clone();
            let mix = mix.clone();
            std::thread::spawn(move || {
                let result = run_worker_reporting(shared, tx, false, mic, None, mix);
                let _ = finished.send(result.as_ref().copied().map_err(ToString::to_string));
                result
            })
        };
        let recorder = Recorder {
            shared: shared.clone(),
            cfg: RecordConfig { dir: dir.path().to_owned(), mic_device: None, loopback_device: None, capture_system: false },
            tx: crossbeam_channel::unbounded().0, unavailable_reported: [false; 2],
            mic: None, sys: None, worker: Some(worker),
            started: Instant::now(), paused_total: Arc::new(Mutex::new(Duration::ZERO)), paused_since: Mutex::new(None),
            out: RecordOutput { mic_wav: mic.clone(), sys_wav: None, mix_wav: mix.clone(), duration: Duration::ZERO },
        };
        drop(recorder);
        let duration = completion.recv_timeout(Duration::from_secs(2)).expect("dropped owner must stop its worker").unwrap();
        assert_eq!(duration, Duration::from_millis(100));
        assert!(shared.stop.load(Ordering::SeqCst));
        assert_eq!(hound::WavReader::open(mic).unwrap().len(), 4_800);
        assert_eq!(hound::WavReader::open(mix).unwrap().len(), 4_800);
    }

    #[test]
    fn stopping_while_paused_preserves_accepted_tail_without_resuming() {
        let dir = tempfile::tempdir().unwrap();
        let mic_wav = dir.path().join("mic.wav");
        let sys_wav = dir.path().join("sys.wav");
        let mix_wav = dir.path().join("mix.wav");
        let origin = unix_now_us();
        let shared = Arc::new(Shared {
            emit_system_pcm: false,
            // These samples were accepted before the pause transition.
            mic_buf: Mutex::new(vec![CapturedPacket { generation: 0, source_token: 0, active_start_frame: 0, time: AudioCaptureTime { unix_us: origin, device_timestamp: true }, samples: vec![0.125, 0.25] }]),
            sys_buf: Mutex::new(vec![CapturedPacket { generation: 0, source_token: 0, active_start_frame: 0, time: AudioCaptureTime { unix_us: origin, device_timestamp: true }, samples: vec![0.5] }]),
            paused: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            boundary: AtomicU64::new(1),
            intent_unix_us: origin,
            paused_us: AtomicU64::new(0),
            interval_start_us: AtomicU64::new(0),
            sources: Default::default(),
            epoch: Instant::now(),
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let worker = {
            let shared = shared.clone();
            let mic = mic_wav.clone();
            let sys = sys_wav.clone();
            let mixed = mix_wav.clone();
            std::thread::spawn(move || run_worker(shared, tx, true, mic, Some(sys), mixed))
        };
        let recorder = Recorder {
            shared: shared.clone(),
            cfg: RecordConfig { dir: dir.path().to_owned(), mic_device: None, loopback_device: None, capture_system: true },
            tx: crossbeam_channel::unbounded().0,
            unavailable_reported: [false; 2],
            mic: None,
            sys: None,
            worker: Some(worker),
            started: Instant::now(),
            paused_total: Arc::new(Mutex::new(Duration::ZERO)),
            paused_since: Mutex::new(Some(Instant::now())),
            out: RecordOutput { mic_wav: mic_wav.clone(), sys_wav: Some(sys_wav.clone()), mix_wav: mix_wav.clone(), duration: Duration::ZERO },
        };
        let out = recorder.stop().unwrap();
        assert!(shared.paused.load(Ordering::SeqCst));
        assert_eq!(out.duration, Duration::from_secs_f64(2.0 / ARCHIVE_HZ as f64));
        assert_eq!(hound::WavReader::open(mic_wav).unwrap().len(), 2);
        assert_eq!(hound::WavReader::open(sys_wav).unwrap().len(), 1);
        let mixed: Vec<i16> = hound::WavReader::open(mix_wav).unwrap().into_samples().map(Result::unwrap).collect();
        assert_eq!(mixed, vec![(0.625 * i16::MAX as f32) as i16, (0.25 * i16::MAX as f32) as i16]);
        let timing: AudioTiming = serde_json::from_slice(&std::fs::read(dir.path().join("mix.clock.json")).unwrap()).unwrap();
        assert_eq!(timing.start_unix_us, origin);
        assert_eq!(timing.first_mic_unix_us, Some(origin));
    }

    #[test]
    fn capture_before_record_intent_is_clipped_without_shifting_the_remainder() {
        let packet = packet_on_active_clock(&[0.25; 96], AudioCaptureTime { unix_us: 999_000, device_timestamp: true }, 0, 1_000_000, 0, 0).unwrap();
        assert_eq!(packet.samples.len(), 48);
        assert_eq!(packet.active_start_frame, 0);
        assert_eq!(packet.time.unix_us, 1_000_000);
    }

    #[test]
    fn resumed_packet_uses_active_elapsed_and_excludes_paused_tail() {
        let packet = packet_on_active_clock(&[0.25; 96], AudioCaptureTime { unix_us: 2_999_000, device_timestamp: true }, 2, 1_000_000, 1_000_000, 2_000_000).unwrap();
        assert_eq!(packet.samples.len(), 48);
        assert_eq!(packet.active_start_frame, 48_000);
        assert_eq!(packet.generation, 2);
    }

    #[test]
    fn worker_persists_intent_origin_and_measured_initial_silence() {
        let dir = tempfile::tempdir().unwrap();
        let origin = unix_now_us();
        let shared = Arc::new(Shared {
            emit_system_pcm: false,
            mic_buf: Mutex::new(vec![CapturedPacket { generation: 0, source_token: 0, active_start_frame: 4_800, time: AudioCaptureTime { unix_us: origin + 100_000, device_timestamp: true }, samples: vec![0.25] }]),
            sys_buf: Mutex::new(vec![CapturedPacket { generation: 0, source_token: 0, active_start_frame: 9_600, time: AudioCaptureTime { unix_us: origin + 200_000, device_timestamp: true }, samples: vec![0.5] }]),
            paused: AtomicBool::new(false), stop: AtomicBool::new(true), boundary: AtomicU64::new(0),
            intent_unix_us: origin, paused_us: AtomicU64::new(0), interval_start_us: AtomicU64::new(0),
            sources: Default::default(), epoch: Instant::now(),
        });
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mic = dir.path().join("mic.wav"); let sys = dir.path().join("sys.wav"); let mix = dir.path().join("mix.wav");
        run_worker(shared, tx, true, mic.clone(), Some(sys.clone()), mix.clone()).unwrap();
        let read = |path| hound::WavReader::open(path).unwrap().into_samples::<i16>().map(Result::unwrap).collect::<Vec<_>>();
        let mic = read(mic); let sys = read(sys); let mix = read(mix);
        assert_eq!(mic.len(), 4_801); assert_eq!(sys.len(), 9_601); assert_eq!(mix.len(), 9_601);
        assert!(mic[..4_800].iter().all(|x| *x == 0));
        assert_eq!(mix[4_800], (0.25 * i16::MAX as f32) as i16);
        assert_eq!(mix[9_600], (0.5 * i16::MAX as f32) as i16);
        let timing: AudioTiming = serde_json::from_slice(&std::fs::read(dir.path().join("mix.clock.json")).unwrap()).unwrap();
        assert_eq!(timing.start_unix_us, origin);
        assert!(!timing.estimated_timestamps);
    }
}

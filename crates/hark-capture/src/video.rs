//! Screen video via an ffmpeg sidecar. Argument builders are pure and tested;
//! `VideoRecorder` just runs the process and stops it cleanly.

use crate::capture_environment::{check_desktop_available, CaptureEnvironmentGuard};
use crate::monitors::MonitorInfo;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CLOCK_FILE_LIMIT: u64 = 4096;
const CAPTURE_LOG_LIMIT: usize = 64 * 1024;

/// First-frame metadata is separate from the zero-based encoded video timestamps.
pub fn capture_clock_path(video: &Path) -> PathBuf {
    video.with_extension("clock.txt")
}

fn capture_clock_filter(video: &Path) -> String {
    // Capture uses a fixed internal filename in a recording directory. A relative
    // filename avoids the Windows drive-colon/filter escaping ambiguity.
    let name = capture_clock_path(video).file_name().unwrap().to_string_lossy().into_owned();
    let escaped = name.replace('\\', "\\\\").replace('\'', "\\'").replace(':', "\\:");
    format!("settb=AVTB,metadata=mode=add:key=hark.clock:value=1:enable='eq(n,0)',metadata=mode=print:key=hark.clock:file='{escaped}':direct=1:enable='eq(n,0)',metadata=mode=delete:key=hark.clock,setpts=PTS-STARTPTS")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum VideoTarget {
    /// Whole monitor by index (0 = primary).
    Monitor { index: u32 },
    /// A single window by exact title (Windows only; macOS falls back to monitor 0).
    Window { title: String },
}

impl Default for VideoTarget {
    fn default() -> Self {
        VideoTarget::Monitor { index: 0 }
    }
}

fn common_encode_args(out: &Path, max_height: u32) -> Vec<String> {
    vec![
        "-vf".into(),
        format!("scale=-2:'min({max_height},ih)'"),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "28".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-an".into(),
        "-movflags".into(),
        "+frag_keyframe+empty_moov+default_base_moof".into(),
        "-f".into(),
        "mp4".into(),
        "-y".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// ffmpeg arguments to capture `target` to a fragmented MP4 at `fps`.
/// `monitor` is the resolved display for `VideoTarget::Monitor` (geometry on
/// Windows, avfoundation device on macOS); `None` captures the whole desktop.
pub fn ffmpeg_capture_args(target: &VideoTarget, monitor: Option<&MonitorInfo>, out: &Path, fps: u32, max_height: u32) -> Vec<String> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    if cfg!(windows) {
        // gdigrab timestamps are Unix microseconds. Preserve the source value for
        // the first-frame clock marker, then normalize before encoding.
        args.push("-copyts".into());
        args.extend(["-f".into(), "gdigrab".into(), "-framerate".into(), fps.to_string(), "-draw_mouse".into(), "1".into()]);
        match target {
            VideoTarget::Monitor { .. } => {
                if let Some(m) = monitor.filter(|m| m.width > 0 && m.height > 0) {
                    args.extend([
                        "-offset_x".into(),
                        m.x.to_string(),
                        "-offset_y".into(),
                        m.y.to_string(),
                        "-video_size".into(),
                        format!("{}x{}", m.width, m.height),
                    ]);
                }
                args.extend(["-i".into(), "desktop".into()]);
            }
            VideoTarget::Window { title } => args.extend(["-i".into(), format!("title={title}")]),
        }
    } else if cfg!(target_os = "macos") {
        let index = match (target, monitor) {
            (VideoTarget::Monitor { .. }, Some(m)) => m.device,
            (VideoTarget::Monitor { index }, None) => *index,
            (VideoTarget::Window { .. }, Some(m)) => m.device,
            (VideoTarget::Window { .. }, None) => 0,
        };
        args.extend([
            "-f".into(),
            "avfoundation".into(),
            "-framerate".into(),
            fps.to_string(),
            "-capture_cursor".into(),
            "1".into(),
            "-i".into(),
            format!("{index}:none"),
        ]);
    } else {
        args.extend(["-f".into(), "x11grab".into(), "-framerate".into(), fps.to_string(), "-i".into(), ":0.0".into()]);
    }
    args.extend(common_encode_args(out, max_height));
    if cfg!(windows) {
        let filter = args.iter().position(|arg| arg == "-vf").unwrap() + 1;
        args[filter] = format!("{},{}", capture_clock_filter(out), args[filter]);
    }
    args
}

/// Mux the finished video with the mixed audio WAV into a normal MP4.
pub fn ffmpeg_mux_args(video: &Path, audio_wav: &Path, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        video.to_string_lossy().into_owned(),
        "-i".into(),
        audio_wav.to_string_lossy().into_owned(),
        "-c:v".into(),
        "copy".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "96k".into(),
        "-shortest".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Pause boundaries survive an interrupted capture/pipeline alongside the raw MP4.
pub fn timeline_path(video: &Path) -> PathBuf {
    video.with_extension("timeline.json")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct PauseInterval {
    start_s: f64,
    /// An unfinished pause excludes everything after its start, including on recovery.
    end_s: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VideoTimeline {
    version: u32,
    fps: u32,
    pauses: Vec<PauseInterval>,
    /// A crash between the two durable writes must not silently publish guessed timing.
    #[serde(default)]
    transition_pending: bool,
    /// Only gdigrab uses a Unix input clock; AVFoundation keeps legacy timing
    /// until its native CMSampleBuffer clock is explicitly bridged.
    #[serde(default)]
    clock: Option<VideoClock>,
    #[serde(default)]
    capture_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VideoClock {
    domain: String,
    /// Unix time paired with VideoRecorder.started, the origin of pause intervals.
    epoch_unix_us: i64,
}

#[derive(Deserialize)]
struct AudioClockOrigin {
    version: u32,
    start_unix_us: i64,
}

impl VideoTimeline {
    fn new(fps: u32) -> Self {
        Self { version: 1, fps, pauses: Vec::new(), transition_pending: false, clock: None, capture_error: None }
    }

    fn pause_at(&mut self, seconds: f64) -> bool {
        if self.pauses.last().is_some_and(|p| p.end_s.is_none()) {
            return false;
        }
        self.pauses.push(PauseInterval { start_s: seconds, end_s: None });
        true
    }

    fn resume_at(&mut self, seconds: f64) -> bool {
        if let Some(pause) = self.pauses.last_mut().filter(|p| p.end_s.is_none()) {
            pause.end_s = Some(seconds.max(pause.start_s));
            return true;
        }
        false
    }

    fn transition_with(
        &mut self,
        pausing: bool,
        mut persist: impl FnMut(&Self) -> std::io::Result<()>,
        transition: impl FnOnce() -> f64,
    ) -> std::io::Result<()> {
        let paused = self.pauses.last().is_some_and(|pause| pause.end_s.is_none());
        if paused == pausing {
            transition();
            // Also repair a previous failed final write from known in-memory state.
            return persist(self);
        }
        let mut pending = self.clone();
        pending.transition_pending = true;
        persist(&pending)?;
        // The audio state changes only after the write-ahead marker is durable.
        // Timestamp the actual transition, excluding both filesystem sync delays.
        let seconds = transition();
        if pausing { self.pause_at(seconds); } else { self.resume_at(seconds); }
        self.transition_pending = false;
        // Retain the actual interval in memory even if this write fails. A later
        // transition can repair it; recovery rejects the pending on-disk marker.
        persist(self)
    }

    fn normalized_pauses(&self) -> std::io::Result<Vec<PauseInterval>> {
        if self.version != 1 || self.fps == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "unsupported video timeline version or frame rate"));
        }
        if self.clock.as_ref().is_some_and(|clock| clock.domain != "unix_us" || clock.epoch_unix_us <= 0) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "unsupported video capture clock"));
        }
        let mut pauses = self.pauses.clone();
        for pause in &pauses {
            if !pause.start_s.is_finite() || pause.start_s < 0.0
                || pause.end_s.is_some_and(|end| !end.is_finite() || end < pause.start_s)
            {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid video pause interval"));
            }
        }
        pauses.retain(|pause| pause.end_s != Some(pause.start_s));
        pauses.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        let mut merged: Vec<PauseInterval> = Vec::new();
        for pause in pauses {
            if let Some(last) = merged.last_mut() {
                if last.end_s.is_none() {
                    break;
                }
                if pause.start_s <= last.end_s.unwrap() {
                    last.end_s = match (last.end_s, pause.end_s) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        _ => None,
                    };
                    continue;
                }
            }
            merged.push(pause);
        }
        Ok(merged)
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        self.normalized_pauses()?;
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        let mut file = fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp, path)
    }

    fn load(path: &Path) -> std::io::Result<Self> {
        let timeline: Self = serde_json::from_slice(&fs::read(path)?).map_err(std::io::Error::other)?;
        timeline.normalized_pauses()?;
        Ok(timeline)
    }
}

#[derive(Debug)]
struct MuxTiming {
    pauses: Vec<PauseInterval>,
    /// First video frame relative to the first sample in mix.wav.
    video_offset_s: f64,
    /// (video-relative end, duration overlapping the audio timeline).
    removed: Vec<(f64, f64)>,
}

impl MuxTiming {
    fn legacy(pauses: Vec<PauseInterval>) -> Self {
        let removed = pauses.iter().filter_map(|pause| pause.end_s.map(|end| (end, end - pause.start_s))).collect();
        Self { pauses, video_offset_s: 0.0, removed }
    }

    fn from_clocks(timeline: &VideoTimeline, video_start_us: i64, audio_start_us: i64) -> Result<Self, String> {
        let clock = timeline.clock.as_ref().ok_or("video capture clock is missing")?;
        if video_start_us <= 0 || audio_start_us <= 0 {
            return Err("invalid capture clock origin".into());
        }
        // Domain mistakes or wall-clock jumps must not produce giant guessed
        // offsets. This is a validity bound, not a measured-latency correction.
        let delta_us = video_start_us.checked_sub(audio_start_us).ok_or("capture clock offset overflow")?;
        let startup_us = video_start_us.checked_sub(clock.epoch_unix_us).ok_or("video startup clock overflow")?;
        if delta_us.unsigned_abs() > 300_000_000 || !(-5_000_000..=120_000_000).contains(&startup_us) {
            return Err("capture clocks do not describe the same recording; original media preserved".into());
        }
        let spawn_to_video = (clock.epoch_unix_us - video_start_us) as f64 / 1_000_000.0;
        let spawn_to_audio = (clock.epoch_unix_us - audio_start_us) as f64 / 1_000_000.0;
        let mut pauses = Vec::new();
        let mut removed = Vec::new();
        for pause in timeline.normalized_pauses().map_err(|e| e.to_string())? {
            let start_s = pause.start_s + spawn_to_video;
            let end_s = pause.end_s.map(|end| end + spawn_to_video);
            if let Some(end) = pause.end_s {
                // A pause before the first audio sample is not removed twice.
                let duration = (end + spawn_to_audio).max(0.0) - (pause.start_s + spawn_to_audio).max(0.0);
                if duration > 0.0 { removed.push((end + spawn_to_video, duration)); }
            }
            pauses.push(PauseInterval { start_s, end_s });
        }
        Ok(Self { pauses, video_offset_s: delta_us as f64 / 1_000_000.0, removed })
    }
}

fn read_video_start(path: &Path) -> Result<i64, String> {
    let mut bytes = Vec::new();
    fs::File::open(path).and_then(|file| file.take(CLOCK_FILE_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("read first video frame clock: {e}"))?;
    if bytes.len() as u64 > CLOCK_FILE_LIMIT {
        return Err("first video frame clock exceeds its size limit".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "first video frame clock is not UTF-8")?;
    let mut lines = text.lines();
    let frame = lines.next().ok_or("first video frame clock is empty")?;
    if !frame.starts_with("frame:0 ") || lines.next() != Some("hark.clock=1") || lines.next().is_some() {
        return Err("first video frame clock is incomplete or invalid".into());
    }
    let pts = frame.split_whitespace().find_map(|field| field.strip_prefix("pts:"))
        .ok_or("first video frame timestamp is missing")?
        .parse::<i64>().map_err(|_| "first video frame timestamp is invalid")?;
    if pts <= 0 { return Err("first video frame timestamp is not a Unix timestamp".into()); }
    Ok(pts)
}

fn pause_filter(timing: &MuxTiming) -> String {
    let mut excludes = Vec::new();
    for pause in &timing.pauses {
        match pause.end_s {
            Some(end) => {
                excludes.push(format!("gte(t,{:.9})*lt(t,{end:.9})", pause.start_s));
            }
            None => excludes.push(format!("gte(t,{:.9})", pause.start_s)),
        }
    }
    // Keep the source's timing rather than rebuilding it from frame count: rounding
    // every pause to N/fps would accumulate another error across repeated pauses.
    let removed: Vec<_> = timing.removed.iter().map(|(end, duration)| format!("gte(T,{end:.9})*{duration:.9}")).collect();
    let shift = if removed.is_empty() { "0".to_string() } else { removed.join("+") };
    let selected = if excludes.is_empty() { "1".to_string() } else { format!("not({})", excludes.join("+")) };
    format!("setpts=PTS-STARTPTS,select='{selected}',setpts='PTS+({:.9}-({shift}))/TB'", timing.video_offset_s)
}

fn mux_args_with_timeline(video: &Path, audio: &Path, out: &Path, timing: &MuxTiming, duration_s: f64, filter_path: &Path) -> Vec<String> {
    let mut args = ffmpeg_mux_args(video, audio, out);
    // A pause may cover every captured frame (e.g. immediate pause + crash).
    // Do not publish an audio-only MP4 as a successfully recovered screen video.
    args.splice(0..0, ["-abort_on".into(), "empty_output_stream".into()]);
    let codec = args.iter().position(|arg| arg == "-c:v").unwrap();
    if !timing.pauses.is_empty() {
        args.splice(codec..codec + 2, [
            "-filter_script:v".into(), filter_path.to_string_lossy().into_owned(),
            "-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(),
            "-crf".into(), "28".into(), "-pix_fmt".into(), "yuv420p".into(),
            "-fps_mode:v".into(), "vfr".into(),
        ]);
    } else if timing.video_offset_s != 0.0 {
        // Offset only the video input; preserve every early audio sample.
        let input = args.iter().position(|arg| arg == "-i").unwrap();
        args.splice(input..input, ["-itsoffset".into(), format!("{:.9}", timing.video_offset_s)]);
    }
    // Keep every captured audio sample even if video starts later or has fewer
    // final frames. -shortest can truncate that audio, or overshoot with copy/AAC.
    // Cap video to audio duration without letting the video EOF cut audio short.
    let shortest = args.iter().position(|arg| arg == "-shortest").unwrap();
    args.splice(shortest..shortest + 1, ["-t".into(), format!("{duration_s:.9}")]);
    args
}

/// Finalize into a temporary sibling and publish only a successful mux. Callers
/// retain raw media and pause metadata on failure, so recovery can safely retry.
pub fn mux_recording(ffmpeg: &Path, video: &Path, audio_wav: &Path, out: &Path) -> Result<(), String> {
    let metadata = timeline_path(video);
    let timing = match VideoTimeline::load(&metadata) {
        Ok(timeline) if timeline.transition_pending => return Err("video pause transition was interrupted; raw video and timeline were preserved".into()),
        Ok(timeline) if timeline.capture_error.is_some() => return Err(format!("screen capture failed: {}", timeline.capture_error.unwrap())),
        Ok(timeline) if timeline.clock.is_some() => {
            let start = read_video_start(&capture_clock_path(video))?;
            let clock_path = audio_wav.with_extension("clock.json");
            let bytes = fs::read(&clock_path).map_err(|e| format!("read audio capture clock: {e}"))?;
            let audio: AudioClockOrigin = serde_json::from_slice(&bytes).map_err(|e| format!("read audio capture clock: {e}"))?;
            if audio.version != 1 { return Err("unsupported audio capture clock version".into()); }
            MuxTiming::from_clocks(&timeline, start, audio.start_unix_us)?
        }
        Ok(timeline) => MuxTiming::legacy(timeline.normalized_pauses().map_err(|e| e.to_string())?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MuxTiming::legacy(Vec::new()), // Older recordings.
        Err(e) => return Err(format!("read video timeline: {e}")),
    };
    let audio = hound::WavReader::open(audio_wav).map_err(|e| format!("read mux audio: {e}"))?;
    let spec = audio.spec();
    let duration_s = audio.len() as f64 / (spec.sample_rate as f64 * spec.channels as f64);
    if !duration_s.is_finite() || duration_s <= 0.0 {
        return Err("cannot mux an empty audio recording".into());
    }
    let temp = out.with_extension("mux.mp4");
    let filter = out.with_extension("pause-filter.txt");
    if !timing.pauses.is_empty() {
        // A file avoids the Windows command-line size limit for many pauses.
        fs::write(&filter, pause_filter(&timing)).map_err(|e| format!("write video pause filter: {e}"))?;
    }
    let args = mux_args_with_timeline(video, audio_wav, &temp, &timing, duration_s, &filter);
    if let Err(error) = run_ffmpeg(ffmpeg, &args) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    fs::rename(&temp, out).map_err(|e| format!("publish muxed video: {e}"))?;
    let _ = fs::remove_file(filter);
    Ok(())
}

/// Cut `[start, end)` seconds out of `input` into `out` (mp4 or mp3 by extension).
pub fn ffmpeg_clip_args(input: &Path, start_s: f64, end_s: f64, out: &Path) -> Vec<String> {
    let is_audio = out.extension().map(|e| e.eq_ignore_ascii_case("mp3")).unwrap_or(false);
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{start_s:.3}"),
        "-to".into(),
        format!("{end_s:.3}"),
        "-i".into(),
        input.to_string_lossy().into_owned(),
    ];
    if is_audio {
        args.extend(["-vn".into(), "-c:a".into(), "libmp3lame".into(), "-q:a".into(), "4".into()]);
    } else {
        args.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "23".into(),
            "-c:a".into(),
            "aac".into(),
        ]);
    }
    args.extend(["-y".into(), out.to_string_lossy().into_owned()]);
    args
}

pub struct VideoRecorder {
    child: Child,
    pub output: PathBuf,
    started: Instant,
    timeline: VideoTimeline,
    stderr_thread: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    last_warning: Option<String>,
    stopped: bool,
    _environment: Option<CaptureEnvironmentGuard>,
}

fn drain_capture_stderr(mut stderr: impl Read, log: &Path) -> std::io::Result<Vec<u8>> {
    let mut tail = VecDeque::with_capacity(CAPTURE_LOG_LIMIT);
    let mut bytes = [0u8; 4096];
    loop {
        let read = match stderr.read(&mut bytes) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let excess = (tail.len() + read).saturating_sub(CAPTURE_LOG_LIMIT);
        tail.drain(..excess);
        tail.extend(&bytes[..read]);
    }
    let tail: Vec<u8> = tail.into_iter().collect();
    fs::write(log, &tail)?;
    Ok(tail)
}

fn capture_executable_path(ffmpeg: &Path) -> std::io::Result<PathBuf> {
    if ffmpeg.file_name() == Some(ffmpeg.as_os_str()) && !ffmpeg.exists() {
        // Keep Command's PATH lookup for a bare program name. An explicit or
        // existing local path must resolve before changing the child's cwd.
        Ok(ffmpeg.to_path_buf())
    } else {
        fs::canonicalize(ffmpeg)
    }
}

impl VideoRecorder {
    pub fn start(ffmpeg: &Path, target: &VideoTarget, out: &Path, fps: u32, max_height: u32) -> std::io::Result<VideoRecorder> {
        let environment = CaptureEnvironmentGuard::acquire()?;
        // Resolve before selecting the child's working directory. The executable
        // and output may otherwise become relative to the recording directory.
        let ffmpeg = capture_executable_path(ffmpeg)?;
        let out = if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir()?.join(out) };
        let monitor = match target {
            VideoTarget::Monitor { index } => crate::monitors::list_monitors(Some(&ffmpeg)).into_iter().find(|m| m.index == *index),
            VideoTarget::Window { .. } => None,
        };
        let mut timeline = VideoTimeline::new(fps);
        let started = Instant::now();
        if cfg!(windows) {
            let epoch_us = SystemTime::now().duration_since(UNIX_EPOCH).map_err(std::io::Error::other)?.as_micros();
            let epoch_unix_us = i64::try_from(epoch_us).map_err(std::io::Error::other)?;
            timeline.clock = Some(VideoClock { domain: "unix_us".into(), epoch_unix_us });
            match fs::remove_file(capture_clock_path(&out)) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        }
        timeline.save(&timeline_path(&out))?;
        let mut child = Command::new(ffmpeg)
            .current_dir(out.parent().ok_or_else(|| std::io::Error::other("video output directory is missing"))?)
            .args(ffmpeg_capture_args(target, monitor.as_ref(), &out, fps, max_height))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        tie_to_process(&child);
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other("video capture error pipe is missing"));
            }
        };
        let log = out.with_extension("capture.log");
        let stderr_thread = match std::thread::Builder::new().name("hark-video-errors".into()).spawn(move || drain_capture_stderr(stderr, &log)) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(VideoRecorder { child, output: out, started, timeline, stderr_thread: Some(stderr_thread), last_warning: None, stopped: false, _environment: Some(environment) })
    }

    /// Called off the UI thread. A known unavailable desktop or exited capture
    /// process is an actionable failure; pixel color alone is not a diagnosis.
    pub fn poll_warning(&mut self) -> Option<String> {
        let issue = match self.child.try_wait() {
            Ok(Some(status)) => {
                // An audio-only fallback no longer needs to keep the display awake.
                self._environment.take();
                Some(format!("Screen capture stopped unexpectedly ({status}). Audio recording continues."))
            }
            Err(error) => Some(format!("Could not check screen capture: {error}")),
            Ok(None) => check_desktop_available().err().map(|error| format!("Screen capture is unavailable: {error}. Audio recording continues.")),
        };
        if issue == self.last_warning { return None; }
        self.last_warning = issue.clone();
        if let Some(error) = &issue {
            self.timeline.capture_error = Some(error.clone());
            let _ = self.timeline.save(&timeline_path(&self.output));
        }
        issue
    }

    /// Capture stays continuous; mux removes this interval from the final video.
    pub fn pause(&mut self) -> std::io::Result<()> {
        self.pause_with(|| {})
    }

    pub fn resume(&mut self) -> std::io::Result<()> {
        self.resume_with(|| {})
    }

    /// Persist intent first, then timestamp the actual audio transition rather
    /// than backdating its boundary across a potentially slow filesystem sync.
    pub fn pause_with(&mut self, transition: impl FnOnce()) -> std::io::Result<()> {
        let started = self.started;
        let path = timeline_path(&self.output);
        self.timeline.transition_with(true, |timeline| timeline.save(&path), || {
            transition();
            started.elapsed().as_secs_f64()
        })
    }

    pub fn resume_with(&mut self, transition: impl FnOnce()) -> std::io::Result<()> {
        let started = self.started;
        let path = timeline_path(&self.output);
        self.timeline.transition_with(false, |timeline| timeline.save(&path), || {
            transition();
            started.elapsed().as_secs_f64()
        })
    }

    /// Ask ffmpeg to finish (writes the trailer), wait up to 10 s, else kill.
    pub fn stop(mut self) -> std::io::Result<()> {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = stdin.write_all(b"q\n");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut killed = false;
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                killed = true;
                break self.child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        self.stopped = true;
        let stderr = match self.stderr_thread.take().map(|thread| {
            thread.join().map_err(|_| std::io::Error::other("video error reader panicked"))?
        }).transpose() {
            Ok(stderr) => stderr.unwrap_or_default(),
            Err(error) => {
                self.timeline.capture_error = Some(format!("read screen capture diagnostics: {error}"));
                Vec::new()
            }
        };
        if killed || !status.success() {
            let tail = &stderr[stderr.len().saturating_sub(2048)..];
            self.timeline.capture_error = Some(format!("ffmpeg screen capture {} ({status}): {}", if killed { "did not stop before the timeout" } else { "failed" }, String::from_utf8_lossy(tail).trim()));
        }
        if self.timeline.clock.is_some() {
            if let Err(error) = read_video_start(&capture_clock_path(&self.output)) {
                self.timeline.capture_error.get_or_insert(error);
            }
        }
        // Repair a failed final transition write from the known actual state.
        // Do this after closing capture so a disk error cannot leave it running.
        self.timeline.save(&timeline_path(&self.output))?;
        match &self.timeline.capture_error {
            Some(error) => Err(std::io::Error::other(error.clone())),
            None => Ok(()),
        }
    }
}

impl Drop for VideoRecorder {
    fn drop(&mut self) {
        if !self.stopped {
            // A later start/store error must not leave an unowned recorder alive.
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.timeline.capture_error.get_or_insert_with(|| "screen capture was interrupted before finalization".into());
            let _ = self.timeline.save(&timeline_path(&self.output));
        }
    }
}

/// Make the OS kill ffmpeg if Hark dies, so a crash never leaves a screen
/// recorder running in the background. Windows job object; no-op elsewhere.
#[cfg(windows)]
fn tie_to_process(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let Ok(job) = CreateJobObjectW(None, None) else { return };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32).is_ok();
        if ok {
            let _ = AssignProcessToJobObject(job, HANDLE(child.as_raw_handle() as *mut _));
        }
        // fable: the job handle is deliberately leaked; it lives as long as this process,
        // which is exactly the lifetime that should kill ffmpeg.
    }
}

#[cfg(not(windows))]
fn tie_to_process(_child: &Child) {}

/// Run ffmpeg to completion with `args`; returns stderr on failure.
pub fn run_ffmpeg(ffmpeg: &Path, args: &[String]) -> Result<(), String> {
    let out = Command::new(ffmpeg).args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_args_use_platform_grabber_and_fragmented_mp4() {
        let out = Path::new("out.mp4");
        let a = ffmpeg_capture_args(&VideoTarget::Monitor { index: 0 }, None, out, 15, 1080);
        let joined = a.join(" ");
        if cfg!(windows) {
            assert!(joined.contains("-f gdigrab"));
            assert!(joined.contains("-i desktop"));
        } else if cfg!(target_os = "macos") {
            assert!(joined.contains("-f avfoundation"));
            assert!(joined.contains("-i 0:none"));
        }
        assert!(joined.contains("-framerate 15"));
        assert!(joined.contains("frag_keyframe"));
        assert!(joined.contains("min(1080,ih)"));
        assert_eq!(a.last().unwrap(), "out.mp4");
    }

    #[test]
    fn window_target_on_windows_uses_title() {
        let a = ffmpeg_capture_args(&VideoTarget::Window { title: "Zoom Meeting".into() }, None, Path::new("o.mp4"), 10, 720);
        if cfg!(windows) {
            assert!(a.contains(&"title=Zoom Meeting".to_string()));
        }
    }

    #[test]
    fn monitor_geometry_selects_a_region() {
        let m = MonitorInfo { index: 1, name: "DISPLAY2".into(), x: -1920, y: 0, width: 1920, height: 1080, primary: false, device: 2 };
        let a = ffmpeg_capture_args(&VideoTarget::Monitor { index: 1 }, Some(&m), Path::new("o.mp4"), 10, 720).join(" ");
        if cfg!(windows) {
            assert!(a.contains("-offset_x -1920 -offset_y 0 -video_size 1920x1080 -i desktop"));
        } else if cfg!(target_os = "macos") {
            assert!(a.contains("-i 2:none"));
        }
    }

    #[test]
    fn mux_and_clip_args() {
        let m = ffmpeg_mux_args(Path::new("v.mp4"), Path::new("mix.wav"), Path::new("screen.mp4"));
        assert!(m.join(" ").contains("-c:v copy -c:a aac"));
        assert_eq!(m.last().unwrap(), "screen.mp4");
        let c = ffmpeg_clip_args(Path::new("screen.mp4"), 1.5, 4.0, Path::new("clip.mp3"));
        assert!(c.join(" ").contains("-ss 1.500 -to 4.000"));
        assert!(c.contains(&"-vn".to_string()));
        let c2 = ffmpeg_clip_args(Path::new("screen.mp4"), 0.0, 2.0, Path::new("clip.mp4"));
        assert!(c2.contains(&"libx264".to_string()));
    }

    #[test]
    fn timeline_survives_restart_and_keeps_an_open_pause() {
        let dir = tempfile::tempdir().unwrap();
        let path = timeline_path(&dir.path().join("screen.raw.mp4"));
        let mut timeline = VideoTimeline::new(15);
        assert!(timeline.pause_at(2.0));
        assert!(!timeline.pause_at(3.0));
        timeline.save(&path).unwrap();
        let mut recovered = VideoTimeline::load(&path).unwrap();
        assert_eq!(recovered.normalized_pauses().unwrap(), vec![PauseInterval { start_s: 2.0, end_s: None }]);
        assert!(recovered.resume_at(5.0));
        assert!(!recovered.resume_at(6.0));
        recovered.save(&path).unwrap();
        assert_eq!(VideoTimeline::load(&path).unwrap().normalized_pauses().unwrap(),
            vec![PauseInterval { start_s: 2.0, end_s: Some(5.0) }]);
    }

    #[test]
    fn transition_boundaries_follow_audio_after_slow_write_ahead() {
        use std::cell::{Cell, RefCell};
        let clock = Cell::new(1.0);
        let writes = RefCell::new(Vec::new());
        let mut timeline = VideoTimeline::new(15);
        timeline.transition_with(true, |snapshot| {
            clock.set(clock.get() + 7.0); // Simulated slow fsync; no real sleep.
            writes.borrow_mut().push(snapshot.clone());
            Ok(())
        }, || clock.get()).unwrap();
        assert_eq!(timeline.pauses[0].start_s, 8.0);
        assert_eq!(clock.get(), 15.0);
        assert!(writes.borrow()[0].transition_pending);
        assert!(!writes.borrow()[1].transition_pending);
        clock.set(20.0);
        timeline.transition_with(false, |_| { clock.set(clock.get() + 7.0); Ok(()) }, || clock.get()).unwrap();
        assert_eq!(timeline.pauses[0].end_s, Some(27.0));
        assert_eq!(clock.get(), 34.0);
    }

    #[test]
    fn transition_write_failures_preserve_actual_state_and_block_unsafe_recovery() {
        use std::cell::Cell;
        let changed = Cell::new(false);
        let mut timeline = VideoTimeline::new(15);
        assert!(timeline.transition_with(true, |_| Err(std::io::Error::other("disk full")), || {
            changed.set(true);
            1.0
        }).is_err());
        assert!(!changed.get());
        assert!(timeline.pauses.is_empty());

        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("screen.raw.mp4");
        let path = timeline_path(&raw);
        let out = dir.path().join("screen.mp4");
        fs::write(&raw, b"captured raw media").unwrap();
        fs::write(&out, b"previous good video").unwrap();
        assert!(timeline.transition_with(true, |snapshot| {
            if snapshot.transition_pending { snapshot.save(&path) } else { Err(std::io::Error::other("final sync failed")) }
        }, || { changed.set(true); 2.5 }).is_err());
        assert!(changed.get());
        assert_eq!(timeline.pauses[0].start_s, 2.5);
        assert!(!timeline.transition_pending);
        assert!(VideoTimeline::load(&path).unwrap().transition_pending);
        let error = mux_recording(Path::new("not-an-executable"), &raw, &dir.path().join("mix.wav"), &out).unwrap_err();
        assert!(error.contains("interrupted"));
        assert_eq!(fs::read(&out).unwrap(), b"previous good video");
        assert_eq!(fs::read(&raw).unwrap(), b"captured raw media");

        // A repeated pause repairs the durable marker without moving its boundary.
        timeline.transition_with(true, |snapshot| snapshot.save(&path), || 4.0).unwrap();
        let repaired = VideoTimeline::load(&path).unwrap();
        assert!(!repaired.transition_pending);
        assert_eq!(repaired.pauses[0].start_s, 2.5);
        timeline.transition_with(false, |snapshot| snapshot.save(&path), || 5.0).unwrap();
        assert_eq!(VideoTimeline::load(&path).unwrap().pauses[0].end_s, Some(5.0));
    }

    #[test]
    fn timeline_normalizes_overlaps_without_double_removing_time() {
        let timeline = VideoTimeline { pauses: vec![
            PauseInterval { start_s: 5.0, end_s: Some(7.0) },
            PauseInterval { start_s: 2.0, end_s: Some(5.5) },
            PauseInterval { start_s: 9.0, end_s: Some(9.0) },
            PauseInterval { start_s: 10.0, end_s: None },
            PauseInterval { start_s: 12.0, end_s: Some(13.0) },
        ], ..VideoTimeline::new(15) };
        assert_eq!(timeline.normalized_pauses().unwrap(), vec![
            PauseInterval { start_s: 2.0, end_s: Some(7.0) },
            PauseInterval { start_s: 10.0, end_s: None },
        ]);
    }

    #[test]
    fn invalid_timeline_is_rejected_before_overwriting_a_good_output() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("screen.raw.mp4");
        let out = dir.path().join("screen.mp4");
        fs::write(&out, b"existing finished video").unwrap();
        fs::write(timeline_path(&raw), br#"{"version":2,"fps":15,"pauses":[]}"#).unwrap();
        let error = mux_recording(Path::new("not-an-executable"), &raw, &dir.path().join("mix.wav"), &out).unwrap_err();
        assert!(error.contains("timeline"));
        assert_eq!(fs::read(&out).unwrap(), b"existing finished video");
        for (start, end) in [(f64::NAN, Some(3.0)), (-1.0, None), (3.0, Some(2.0)), (1.0, Some(f64::INFINITY))] {
            let timeline = VideoTimeline { pauses: vec![PauseInterval { start_s: start, end_s: end }], ..VideoTimeline::new(15) };
            assert!(timeline.normalized_pauses().is_err());
        }
    }

    #[test]
    fn pause_mux_preserves_real_timestamps_and_caps_to_audio() {
        let raw = Path::new("raw.mp4");
        let audio = Path::new("mix.wav");
        let out = Path::new("out.mp4");
        let script = Path::new("filter.txt");
        let pauses = vec![PauseInterval { start_s: 1.125, end_s: Some(2.25) }, PauseInterval { start_s: 5.5, end_s: None }];
        let timing = MuxTiming::legacy(pauses);
        let args = mux_args_with_timeline(raw, audio, out, &timing, 4.375, script);
        assert!(args.windows(2).any(|pair| pair == ["-t", "4.375000000"]));
        assert!(!args.contains(&"-shortest".to_string()));
        assert!(args.windows(2).any(|pair| pair == ["-abort_on", "empty_output_stream"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|pair| pair == ["-filter_script:v", "filter.txt"]));
        let filter = pause_filter(&timing);
        assert!(filter.contains("gte(t,1.125000000)*lt(t,2.250000000)"));
        assert!(filter.contains("gte(t,5.500000000)"));
        assert!(filter.contains("gte(T,2.250000000)*1.125000000"));
        assert!(!filter.contains("N/"));
        let unpaused = mux_args_with_timeline(raw, audio, out, &MuxTiming::legacy(Vec::new()), 6.0, script);
        assert!(unpaused.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(!unpaused.contains(&"-filter_script:v".to_string()));
    }

    fn clocked_timeline(epoch_unix_us: i64) -> VideoTimeline {
        VideoTimeline {
            clock: Some(VideoClock { domain: "unix_us".into(), epoch_unix_us }),
            ..VideoTimeline::new(15)
        }
    }

    #[test]
    fn delayed_video_offsets_only_its_input_and_preserves_early_audio() {
        let audio_epoch = 1_793_400_000_000_000;
        let timeline = clocked_timeline(audio_epoch + 100_000);
        let timing = MuxTiming::from_clocks(&timeline, audio_epoch + 500_000, audio_epoch).unwrap();
        assert_eq!(timing.video_offset_s, 0.5);
        let args = mux_args_with_timeline(Path::new("raw.mp4"), Path::new("mix.wav"), Path::new("out.mp4"), &timing, 6.0, Path::new("filter.txt"));
        assert!(args.windows(4).any(|part| part == ["-itsoffset", "0.500000000", "-i", "raw.mp4"]));
        assert!(args.windows(2).any(|part| part == ["-i", "mix.wav"]));
        assert!(args.windows(2).any(|part| part == ["-c:v", "copy"]));
    }

    #[test]
    fn pause_before_first_frame_uses_absolute_clock_and_keeps_initial_active_time() {
        let epoch = 1_793_400_000_000_000;
        let mut timeline = clocked_timeline(epoch);
        timeline.pauses.push(PauseInterval { start_s: 0.25, end_s: Some(1.25) });
        timeline.pauses.push(PauseInterval { start_s: 2.0, end_s: None });
        let timing = MuxTiming::from_clocks(&timeline, epoch + 500_000, epoch).unwrap();
        assert_eq!(timing.pauses, vec![
            PauseInterval { start_s: -0.25, end_s: Some(0.75) },
            PauseInterval { start_s: 1.5, end_s: None },
        ]);
        assert_eq!(timing.removed, vec![(0.75, 1.0)]);
        let filter = pause_filter(&timing);
        assert!(filter.contains("gte(t,-0.250000000)*lt(t,0.750000000)"));
        assert!(filter.contains("gte(T,0.750000000)*1.000000000"));
        assert!(filter.contains("PTS+(0.500000000-("));
        // First retained frame at raw .75 is output at .25: the active audio
        // before the pause stays present even though no video was captured yet.
        assert_eq!(0.75 + timing.video_offset_s - timing.removed[0].1, 0.25);
    }

    #[test]
    fn pause_before_audio_origin_is_not_subtracted_twice() {
        let epoch = 1_793_400_000_000_000;
        let mut timeline = clocked_timeline(epoch);
        timeline.pauses.push(PauseInterval { start_s: 0.25, end_s: Some(1.0) });
        let timing = MuxTiming::from_clocks(&timeline, epoch + 1_000_000, epoch + 500_000).unwrap();
        assert_eq!(timing.removed, vec![(0.0, 0.5)]);
        assert_eq!(timing.video_offset_s - timing.removed[0].1, 0.0);
        assert!(MuxTiming::from_clocks(&timeline, 100, epoch).is_err());
        assert!(MuxTiming::from_clocks(&timeline, epoch, 0).is_err());
    }

    #[test]
    fn first_frame_clock_parser_is_bounded_and_requires_the_complete_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("first.txt");
        fs::write(&path, "frame:0    pts:1793400000123456 pts_time:1793400000.123456\nhark.clock=1\n").unwrap();
        assert_eq!(read_video_start(&path).unwrap(), 1_793_400_000_123_456);
        for invalid in ["", "frame:0 pts:10\n", "frame:0 pts:10\nhark.clock=0\n", "frame:1 pts:10\nhark.clock=1\n", "frame:0 pts:-1\nhark.clock=1\n", "frame:0 pts:1\nhark.clock=1\nframe:1 pts:2\n"] {
            fs::write(&path, invalid).unwrap();
            assert!(read_video_start(&path).is_err(), "accepted {invalid:?}");
        }
        fs::write(&path, vec![b'x'; CLOCK_FILE_LIMIT as usize + 1]).unwrap();
        assert!(read_video_start(&path).unwrap_err().contains("size limit"));
    }

    #[test]
    fn new_recordings_require_matching_clock_metadata_and_preserve_a_good_output() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("screen.raw.mp4");
        let audio = dir.path().join("mix.wav");
        let out = dir.path().join("screen.mp4");
        fs::write(&raw, b"raw capture").unwrap();
        fs::write(&out, b"previous good output").unwrap();
        let timeline = clocked_timeline(1_793_400_000_000_000);
        timeline.save(&timeline_path(&raw)).unwrap();
        assert!(mux_recording(Path::new("not-an-executable"), &raw, &audio, &out).unwrap_err().contains("first video frame clock"));
        fs::write(capture_clock_path(&raw), "frame:0 pts:1793400000500000\nhark.clock=1\n").unwrap();
        assert!(mux_recording(Path::new("not-an-executable"), &raw, &audio, &out).unwrap_err().contains("audio capture clock"));
        fs::write(audio.with_extension("clock.json"), br#"{"version":2,"start_unix_us":1793400000000000}"#).unwrap();
        assert!(mux_recording(Path::new("not-an-executable"), &raw, &audio, &out).unwrap_err().contains("version"));
        let mut failed = timeline;
        failed.capture_error = Some("capture process exited unexpectedly".into());
        failed.save(&timeline_path(&raw)).unwrap();
        assert!(mux_recording(Path::new("not-an-executable"), &raw, &audio, &out).unwrap_err().contains("capture process exited"));
        assert_eq!(fs::read(&out).unwrap(), b"previous good output");
        assert_eq!(fs::read(&raw).unwrap(), b"raw capture");
    }

    #[test]
    fn legacy_timeline_remains_readable_and_native_clock_domains_are_not_guessed() {
        let old: VideoTimeline = serde_json::from_str(r#"{"version":1,"fps":15,"pauses":[]}"#).unwrap();
        assert!(old.clock.is_none());
        assert!(old.capture_error.is_none());
        assert!(old.normalized_pauses().unwrap().is_empty());
        let mut native = clocked_timeline(1_793_400_000_000_000);
        native.clock.as_mut().unwrap().domain = "avfoundation_host_time".into();
        assert!(native.normalized_pauses().is_err());
    }

    #[test]
    fn capture_diagnostics_are_drained_with_a_bounded_persisted_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("capture.log");
        let mut data = vec![b'x'; CAPTURE_LOG_LIMIT * 3];
        data.extend_from_slice(b"actual capture failure\n");
        let tail = drain_capture_stderr(std::io::Cursor::new(&data), &log).unwrap();
        assert_eq!(tail.len(), CAPTURE_LOG_LIMIT);
        assert_eq!(tail, data[data.len() - CAPTURE_LOG_LIMIT..]);
        assert_eq!(fs::read(log).unwrap(), tail);
    }

    #[test]
    fn capture_executable_preserves_path_lookup_and_resolves_explicit_paths() {
        let bare = Path::new("hark-test-path-program-that-does-not-exist");
        assert_eq!(capture_executable_path(bare).unwrap(), bare);
        let dir = tempfile::tempdir_in(".").unwrap();
        let relative = dir.path().join("ffmpeg.exe");
        fs::write(&relative, b"test executable path").unwrap();
        assert_eq!(capture_executable_path(&relative).unwrap(), fs::canonicalize(&relative).unwrap());
        assert!(capture_executable_path(&dir.path().join("missing.exe")).is_err());
    }

    #[test]
    #[ignore = "set HARK_TEST_FFMPEG to a local ffmpeg executable; opens no capture devices"]
    fn ffmpeg_first_frame_marker_is_bounded_and_delayed_video_keeps_early_audio() {
        let ffmpeg = PathBuf::from(std::env::var_os("HARK_TEST_FFMPEG").expect("set HARK_TEST_FFMPEG"));
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("screen.raw.mp4");
        let audio = dir.path().join("mix.wav");
        let out = dir.path().join("screen.mp4");
        // Exercise exactly the capture filter with an absolute input timestamp,
        // then confirm it encodes zero-based media and only writes frame zero.
        let epoch = 1_793_400_000_000_000i64;
        let source = format!("color=red:s=32x32:r=10:d=1,settb=AVTB,setpts=PTS+{}/TB", (epoch + 500_000) as f64 / 1_000_000.0);
        let capture = Command::new(&ffmpeg).current_dir(dir.path()).args([
            "-v", "error", "-copyts", "-f", "lavfi", "-i", &source,
            "-vf", &capture_clock_filter(&raw), "-c:v", "libx264", "-pix_fmt", "yuv420p", "-y", raw.to_str().unwrap(),
        ]).output().unwrap();
        assert!(capture.status.success(), "{}", String::from_utf8_lossy(&capture.stderr));
        assert_eq!(read_video_start(&capture_clock_path(&raw)).unwrap(), epoch + 500_000);
        let mut writer = crate::wav::WavWriter::create(&audio, 48_000, 1).unwrap();
        writer.write(&vec![0.2; 2 * 48_000]).unwrap();
        writer.finish().unwrap();
        fs::write(audio.with_extension("clock.json"), format!(r#"{{"version":1,"start_unix_us":{epoch}}}"#)).unwrap();
        clocked_timeline(epoch).save(&timeline_path(&raw)).unwrap();
        mux_recording(&ffmpeg, &raw, &audio, &out).unwrap();
        let frames = Command::new(&ffmpeg).args([
            "-v", "info", "-copyts", "-i", out.to_str().unwrap(), "-map", "0:v:0", "-vf", "showinfo=checksum=0", "-frames:v", "1", "-f", "null", "-",
        ]).output().unwrap();
        assert!(frames.status.success());
        let log = String::from_utf8_lossy(&frames.stderr);
        let first = log.lines().find(|line| line.contains(" n:   0 ")).expect("first video frame diagnostics");
        assert!(first.contains("pts_time:0.5"), "{first}");
        let decoded = Command::new(&ffmpeg).args([
            "-v", "error", "-i", out.to_str().unwrap(), "-map", "0:a:0", "-f", "f32le", "-ac", "1", "-ar", "48000", "pipe:1",
        ]).output().unwrap();
        assert!(decoded.status.success());
        assert!((2 * 48_000..=2 * 48_000 + 1024).contains(&(decoded.stdout.len() / 4)));
        let first_audio = f32::from_le_bytes(decoded.stdout[..4].try_into().unwrap());
        assert!(first_audio > 0.05, "early audio was replaced with silence");

        // Pause begins before the first video frame. The first retained frame
        // is raw .8, wall time 1.3, and active recording time .3 seconds.
        let mut timeline = clocked_timeline(epoch);
        timeline.pauses.push(PauseInterval { start_s: 0.25, end_s: Some(1.25) });
        timeline.save(&timeline_path(&raw)).unwrap();
        mux_recording(&ffmpeg, &raw, &audio, &out).unwrap();
        let frames = Command::new(&ffmpeg).args([
            "-v", "info", "-copyts", "-i", out.to_str().unwrap(), "-map", "0:v:0", "-vf", "showinfo=checksum=0", "-fps_mode", "passthrough", "-f", "null", "-",
        ]).output().unwrap();
        assert!(frames.status.success());
        let log = String::from_utf8_lossy(&frames.stderr);
        let displayed: Vec<_> = log.lines().filter(|line| line.contains(" pts_time:")).collect();
        assert_eq!(displayed.len(), 2, "{log}");
        assert!(displayed[0].contains("pts_time:0.3"), "{}", displayed[0]);
        let good = fs::read(&out).unwrap();
        timeline.pauses[0].end_s = None;
        timeline.save(&timeline_path(&raw)).unwrap();
        assert!(mux_recording(&ffmpeg, &raw, &audio, &out).is_err());
        assert_eq!(fs::read(&out).unwrap(), good);
    }

    #[test]
    #[ignore = "set HARK_TEST_FFMPEG to a local ffmpeg executable; opens no capture devices"]
    fn ffmpeg_pause_mux_excludes_paused_frames_and_preserves_failed_inputs() {
        let ffmpeg = PathBuf::from(std::env::var_os("HARK_TEST_FFMPEG").expect("set HARK_TEST_FFMPEG"));
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("screen.raw.mp4");
        let audio = dir.path().join("mix.wav");
        let out = dir.path().join("screen.mp4");
        let source = "color=red:s=32x32:r=15:d=1[r];color=blue:s=32x32:r=15:d=1[p1];color=green:s=32x32:r=15:d=1[g];color=blue:s=32x32:r=15:d=1[p2];color=yellow:s=32x32:r=15:d=1[y];[r][p1][g][p2][y]concat=n=5:v=1:a=0";
        run_ffmpeg(&ffmpeg, &[
            "-v".into(), "error".into(), "-f".into(), "lavfi".into(), "-i".into(), source.into(),
            "-c:v".into(), "libx264".into(), "-pix_fmt".into(), "yuv420p".into(),
            "-y".into(), raw.to_string_lossy().into_owned(),
        ]).unwrap();
        let mut writer = crate::wav::WavWriter::create(&audio, 48_000, 1).unwrap();
        writer.write(&vec![0.2; 3 * 48_000]).unwrap();
        writer.finish().unwrap();
        let timeline = VideoTimeline { pauses: vec![
            PauseInterval { start_s: 1.0, end_s: Some(2.0) },
            PauseInterval { start_s: 3.0, end_s: Some(4.0) },
        ], ..VideoTimeline::new(15) };
        timeline.save(&timeline_path(&raw)).unwrap();
        mux_recording(&ffmpeg, &raw, &audio, &out).unwrap();
        let decoded = Command::new(&ffmpeg).args([
            "-v", "error", "-i", out.to_str().unwrap(), "-map", "0:v:0", "-fps_mode", "passthrough",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1",
        ]).output().unwrap();
        assert!(decoded.status.success());
        let frame_bytes = 32 * 32 * 3;
        assert_eq!(decoded.stdout.len(), 45 * frame_bytes);
        let means: Vec<[f64; 3]> = decoded.stdout.chunks_exact(frame_bytes).map(|frame| {
            let mut sum = [0.0; 3];
            for pixel in frame.as_chunks::<3>().0 {
                for channel in 0..3 { sum[channel] += pixel[channel] as f64; }
            }
            sum.map(|value| value / 1024.0)
        }).collect();
        assert!(means.iter().all(|rgb| rgb[2] < 40.0), "blue pause frames survived");
        assert!(means[..15].iter().all(|rgb| rgb[0] > 200.0 && rgb[1] < 40.0));
        assert!(means[15..30].iter().all(|rgb| rgb[1] > 80.0 && rgb[0] < 40.0));
        assert!(means[30..].iter().all(|rgb| rgb[0] > 200.0 && rgb[1] > 200.0));

        // A crash/stop during a pause excludes the remainder instead of reopening it.
        let open = VideoTimeline { pauses: vec![PauseInterval { start_s: 1.0, end_s: None }], ..VideoTimeline::new(15) };
        open.save(&timeline_path(&raw)).unwrap();
        mux_recording(&ffmpeg, &raw, &audio, &out).unwrap();
        // A shorter video must not truncate the preserved three-second audio.
        let decoded_audio = Command::new(&ffmpeg).args([
            "-v", "error", "-i", out.to_str().unwrap(), "-map", "0:a:0",
            "-f", "f32le", "-ac", "1", "-ar", "48000", "pipe:1",
        ]).output().unwrap();
        assert!(decoded_audio.status.success());
        let decoded_samples = decoded_audio.stdout.len() / std::mem::size_of::<f32>();
        assert!((3 * 48_000..=3 * 48_000 + 1_024).contains(&decoded_samples));
        let before_failed_retry = fs::read(&out).unwrap();
        let empty = VideoTimeline { pauses: vec![PauseInterval { start_s: 0.0, end_s: None }], ..VideoTimeline::new(15) };
        empty.save(&timeline_path(&raw)).unwrap();
        assert!(mux_recording(&ffmpeg, &raw, &audio, &out).is_err());
        assert_eq!(fs::read(&out).unwrap(), before_failed_retry);
        assert!(raw.exists());
        assert!(timeline_path(&raw).exists());
    }
}

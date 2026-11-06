//! Local CPU Nemotron 3 diarization through NeMo-Speech.cpp v0.3.0's C ABI.
//!
//! The runtime and checkpoint must already exist. A fresh native stream is used
//! per recording. We consume committed probabilities before native compaction
//! and apply one uniform short-turn policy ourselves: the native segments API
//! freezes old segments with a different, library-default policy.

use crate::engine::DiarizeError;
use bazza_diarize::Turn;
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr::NonNull;

pub const MAX_SPEAKERS: usize = 8;
const SAMPLE_RATE: usize = 16_000;
const CHUNK_SAMPLES: usize = 15 * SAMPLE_RATE;
// This bounds the scratch copy even if the native runtime cannot find a safe
// silent compaction boundary. It limits retained history, not recording length.
const MAX_RETAINED_FRAMES: usize = 360_000;
const FRAME_SECONDS: f64 = 0.01;
const THRESHOLD: f32 = 0.5;
const PAD_SECONDS: f64 = 0.05;
const MIN_GAP_SECONDS: f64 = 0.05;
const MIN_DURATION_SECONDS: f64 = 0.05;

fn error(message: impl Into<String>) -> DiarizeError {
    DiarizeError::Engine(format!("Nemotron: {}", message.into()))
}

#[repr(C)]
struct ModelConfig {
    size: usize,
    model_path: *const c_char,
    gpu: i32,
    preset: *const c_char,
    chunk_frames: i32,
    right_context_frames: i32,
    left_context_frames: i32,
    fifo_frames: i32,
    spkcache_frames: i32,
    update_period_frames: i32,
}

type Create = unsafe extern "C" fn(*const ModelConfig, *mut *mut c_void) -> c_int;
type Destroy = unsafe extern "C" fn(*mut c_void);
type NumSpeakers = unsafe extern "C" fn(*const c_void) -> i32;
type SecondsPerFrame = unsafe extern "C" fn(*const c_void) -> f64;
type StreamOpen = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int;
type StreamPush = unsafe extern "C" fn(*mut c_void, *const f32, usize, i32) -> c_int;
type StreamFinish = unsafe extern "C" fn(*mut c_void) -> c_int;
type FrameIndex = unsafe extern "C" fn(*const c_void) -> i64;
type FrameProbs = unsafe extern "C" fn(*const c_void, *mut f32, usize) -> c_int;
type LastError = unsafe extern "C" fn() -> *const c_char;

struct Api {
    create: Create,
    destroy: Destroy,
    num_speakers: NumSpeakers,
    seconds_per_frame: SecondsPerFrame,
    stream_open: StreamOpen,
    stream_push: StreamPush,
    stream_finish: StreamFinish,
    stream_close: Destroy,
    frame_count: FrameIndex,
    frame_probs_start: FrameIndex,
    frame_probs: FrameProbs,
    last_error: LastError,
    // v0.3.0's CPU/OpenMP runtime can crash when its DLL family is unloaded
    // after inference. Pin it until process exit; model and stream resources
    // still have normal RAII cleanup. The sidecar performs one job per process.
    _library: ManuallyDrop<Library>,
}

impl Api {
    fn load(runtime_dir: &Path) -> Result<Self, DiarizeError> {
        #[cfg(target_os = "windows")]
        let filename = "nemo_speech_asr_c.dll";
        #[cfg(target_os = "macos")]
        let filename = "libnemo_speech_asr_c.dylib";
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let filename = "libnemo_speech_asr_c.so";

        let path = runtime_dir.join(filename).canonicalize().map_err(|e| {
            error(format!(
                "runtime {}: {e}",
                runtime_dir.join(filename).display()
            ))
        })?;
        #[cfg(test)]
        eprintln!("Nemotron test: loading runtime {}", path.display());
        // Search the explicit runtime's directory for its dependent DLL family,
        // without changing PATH or the process-wide DLL search directory.
        #[cfg(target_os = "windows")]
        let library: Library = unsafe {
            const SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
            const SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
            libloading::os::windows::Library::load_with_flags(
                &path,
                SEARCH_DLL_LOAD_DIR | SEARCH_DEFAULT_DIRS,
            )
            .map_err(|e| error(format!("load runtime {}: {e}", path.display())))?
            .into()
        };
        #[cfg(not(target_os = "windows"))]
        let library = unsafe {
            Library::new(&path)
                .map_err(|e| error(format!("load runtime {}: {e}", path.display())))?
        };
        macro_rules! symbol {
            ($name:literal, $kind:ty) => {
                // The signatures below reproduce the pinned official C header.
                unsafe {
                    *library
                        .get::<$kind>(concat!($name, "\0").as_bytes())
                        .map_err(|e| error(format!("runtime symbol {}: {e}", $name)))?
                }
            };
        }
        #[cfg(test)]
        eprintln!("Nemotron test: runtime loaded, resolving symbols");
        Ok(Self {
            create: symbol!("nemo_speech_diar_create", Create),
            destroy: symbol!("nemo_speech_diar_destroy", Destroy),
            num_speakers: symbol!("nemo_speech_diar_num_speakers", NumSpeakers),
            seconds_per_frame: symbol!("nemo_speech_diar_seconds_per_frame", SecondsPerFrame),
            stream_open: symbol!("nemo_speech_diar_stream_open", StreamOpen),
            stream_push: symbol!("nemo_speech_diar_stream_push_f32", StreamPush),
            stream_finish: symbol!("nemo_speech_diar_stream_finish", StreamFinish),
            stream_close: symbol!("nemo_speech_diar_stream_close", Destroy),
            frame_count: symbol!("nemo_speech_diar_frame_count", FrameIndex),
            frame_probs_start: symbol!("nemo_speech_diar_frame_probs_start", FrameIndex),
            frame_probs: symbol!("nemo_speech_diar_frame_probs", FrameProbs),
            last_error: symbol!("nemo_speech_asr_last_error", LastError),
            _library: ManuallyDrop::new(library),
        })
    }

    fn check(&self, status: c_int) -> Result<(), DiarizeError> {
        if status == 0 {
            return Ok(());
        }
        let pointer = unsafe { (self.last_error)() };
        let detail = if pointer.is_null() {
            "no native error detail".into()
        } else {
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        };
        Err(error(format!("native status {status}: {detail}")))
    }
}

struct Stream<'a> {
    handle: NonNull<c_void>,
    api: &'a Api,
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.api.stream_close)(self.handle.as_ptr());
        }
    }
}

pub struct NemotronEngine {
    model: NonNull<c_void>,
    api: Api,
    consumed_frames: usize,
}

impl Drop for NemotronEngine {
    fn drop(&mut self) {
        // Api, including its Library, remains alive throughout model destruction.
        unsafe {
            (self.api.destroy)(self.model.as_ptr());
        }
        #[cfg(test)]
        eprintln!("Nemotron test: native model destroyed; runtime remains pinned");
    }
}

impl NemotronEngine {
    /// Load an already installed CPU runtime and eight-speaker Nemotron GGUF.
    /// The published large-context geometry is explicit, independent of presets.
    pub fn load(runtime_dir: &Path, model_path: &Path) -> Result<Self, DiarizeError> {
        if !model_path.is_file() {
            return Err(DiarizeError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }
        let model_path = model_path
            .canonicalize()
            .map_err(|e| error(e.to_string()))?;
        let model_path = model_path
            .to_str()
            .ok_or_else(|| error("model path is not valid UTF-8"))?;
        let model_path = CString::new(model_path).map_err(|e| error(e.to_string()))?;
        let api = Api::load(runtime_dir)?;
        let config = ModelConfig {
            size: std::mem::size_of::<ModelConfig>(),
            model_path: model_path.as_ptr(),
            gpu: -1,
            preset: std::ptr::null(),
            chunk_frames: 340,
            right_context_frames: 40,
            left_context_frames: 0,
            fifo_frames: 40,
            spkcache_frames: 264,
            update_period_frames: 300,
        };
        let mut model = std::ptr::null_mut();
        #[cfg(test)]
        eprintln!(
            "Nemotron test: creating model {}",
            model_path.to_string_lossy()
        );
        api.check(unsafe { (api.create)(&config, &mut model) })?;
        #[cfg(test)]
        eprintln!("Nemotron test: model created");
        let model =
            NonNull::new(model).ok_or_else(|| error("native create returned a null model"))?;
        let engine = Self {
            model,
            api,
            consumed_frames: 0,
        };
        let capacity = unsafe { (engine.api.num_speakers)(engine.model.as_ptr()) };
        let cadence = unsafe { (engine.api.seconds_per_frame)(engine.model.as_ptr()) };
        if capacity != MAX_SPEAKERS as i32
            || !cadence.is_finite()
            || (cadence - FRAME_SECONDS).abs() > 1e-9
        {
            return Err(error(format!(
                "expected 8 speakers at 10 ms, got {capacity} speakers at {cadence} s"
            )));
        }
        Ok(engine)
    }

    /// Frames consumed in the last job, including on a failed job for diagnostics.
    pub fn consumed_frame_count(&self) -> usize {
        self.consumed_frames
    }

    /// Speaker turns for mono 16 kHz audio, including recordings below one second.
    #[allow(dead_code)] // Convenience API; production reads WAVs through diarize_chunks.
    pub fn diarize(
        &mut self,
        pcm16k: &[f32],
        progress: impl Fn(f32),
    ) -> Result<Vec<Turn>, DiarizeError> {
        let mut chunks = pcm16k.chunks(CHUNK_SAMPLES);
        self.diarize_chunks(
            pcm16k.len(),
            || Ok(chunks.next().unwrap_or(&[]).to_vec()),
            progress,
        )
    }

    /// Bounded source entry point. Empty chunk means EOF; total_samples must be
    /// exact, and each nonempty chunk may contain at most 15 seconds of audio.
    pub fn diarize_chunks(
        &mut self,
        total_samples: usize,
        mut next_chunk: impl FnMut() -> Result<Vec<f32>, DiarizeError>,
        progress: impl Fn(f32),
    ) -> Result<Vec<Turn>, DiarizeError> {
        self.consumed_frames = 0;
        progress(0.0);
        let mut handle = std::ptr::null_mut();
        self.api
            .check(unsafe { (self.api.stream_open)(self.model.as_ptr(), &mut handle) })?;
        let stream = Stream {
            handle: NonNull::new(handle)
                .ok_or_else(|| error("native open returned a null stream"))?,
            api: &self.api,
        };
        let mut postprocessor = UniformPostprocessor::default();
        let mut scratch = Vec::new();
        let mut processed = 0usize;
        loop {
            let chunk = next_chunk()?;
            if chunk.is_empty() {
                break;
            }
            if chunk.len() > CHUNK_SAMPLES {
                return Err(error("source chunk exceeds the 15-second limit"));
            }
            processed = processed
                .checked_add(chunk.len())
                .ok_or_else(|| error("source sample count overflow"))?;
            if processed > total_samples {
                return Err(error("source produced more samples than declared"));
            }
            if !chunk.iter().all(|value| value.is_finite()) {
                return Err(error("source contains nonfinite audio samples"));
            }
            self.api.check(unsafe {
                (self.api.stream_push)(
                    stream.handle.as_ptr(),
                    chunk.as_ptr(),
                    chunk.len(),
                    SAMPLE_RATE as i32,
                )
            })?;
            consume_available(&stream, &mut scratch, &mut postprocessor)?;
            self.consumed_frames = postprocessor.next_frame;
            progress(if total_samples == 0 {
                0.0
            } else {
                processed as f32 / total_samples as f32
            });
        }
        if processed != total_samples {
            return Err(error(format!(
                "source ended after {processed} samples; expected {total_samples}"
            )));
        }
        if processed == 0 {
            progress(1.0);
            return Ok(Vec::new());
        }
        self.api
            .check(unsafe { (self.api.stream_finish)(stream.handle.as_ptr()) })?;
        consume_available(&stream, &mut scratch, &mut postprocessor)?;
        self.consumed_frames = postprocessor.next_frame;
        // Native centered analysis may contribute one final frame. Larger
        // discrepancies indicate truncation or an incompatible runtime/model.
        let expected_frames = total_samples.div_ceil(SAMPLE_RATE / 100);
        if self.consumed_frames.abs_diff(expected_frames) > 1 {
            return Err(error(format!(
                "incomplete timeline: {} frames for {total_samples} samples",
                self.consumed_frames
            )));
        }
        let turns = postprocessor.finish(total_samples as f64 / SAMPLE_RATE as f64);
        progress(1.0);
        Ok(turns)
    }
}

fn consume_available(
    stream: &Stream<'_>,
    scratch: &mut Vec<f32>,
    post: &mut UniformPostprocessor,
) -> Result<(), DiarizeError> {
    let count = unsafe { (stream.api.frame_count)(stream.handle.as_ptr()) };
    let start = unsafe { (stream.api.frame_probs_start)(stream.handle.as_ptr()) };
    if start < 0 || count < start {
        return Err(error("invalid native retained-frame range"));
    }
    let retained =
        usize::try_from(count - start).map_err(|_| error("retained frame count overflow"))?;
    if retained > MAX_RETAINED_FRAMES {
        return Err(error(format!("native retained window exceeds {MAX_RETAINED_FRAMES} frames; no safe compaction boundary found")));
    }
    scratch.resize(retained * MAX_SPEAKERS, 0.0);
    stream.api.check(unsafe {
        (stream.api.frame_probs)(stream.handle.as_ptr(), scratch.as_mut_ptr(), scratch.len())
    })?;
    post.ingest_window(start as usize, scratch)
}

#[derive(Clone, Copy)]
struct Span {
    start: f64,
    end: f64,
}

#[derive(Default)]
struct Channel {
    active_start: Option<usize>,
    pending: Option<Span>,
}

#[derive(Default)]
struct UniformPostprocessor {
    channels: [Channel; MAX_SPEAKERS],
    next_frame: usize,
    segments: Vec<(Span, usize)>,
}

impl UniformPostprocessor {
    fn ingest_window(&mut self, start: usize, probabilities: &[f32]) -> Result<(), DiarizeError> {
        if !probabilities.len().is_multiple_of(MAX_SPEAKERS) {
            return Err(error("invalid probability matrix shape"));
        }
        let end = start
            .checked_add(probabilities.len() / MAX_SPEAKERS)
            .ok_or_else(|| error("probability frame range overflow"))?;
        if start > self.next_frame {
            return Err(error(format!(
                "probability history gap at {} before {start}",
                self.next_frame
            )));
        }
        if end < self.next_frame {
            return Err(error("native frame count moved backwards"));
        }
        if !probabilities
            .iter()
            .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
        {
            return Err(error("nonfinite or out-of-range speaker probability"));
        }
        // The API returns the complete retained window. Old rows are overlap,
        // not new evidence: consume each absolute committed frame exactly once.
        for frame in self.next_frame..end {
            let row = &probabilities[(frame - start) * MAX_SPEAKERS..][..MAX_SPEAKERS];
            for (speaker, &p) in row.iter().enumerate() {
                if self.channels[speaker].active_start.is_none() && p > THRESHOLD {
                    self.channels[speaker].active_start = Some(frame);
                } else if p < THRESHOLD {
                    if let Some(first) = self.channels[speaker].active_start.take() {
                        self.candidate(
                            speaker,
                            Span {
                                start: (first as f64 * FRAME_SECONDS - PAD_SECONDS).max(0.0),
                                end: frame as f64 * FRAME_SECONDS + PAD_SECONDS,
                            },
                        );
                    }
                }
            }
        }
        self.next_frame = end;
        Ok(())
    }

    fn candidate(&mut self, speaker: usize, candidate: Span) {
        let channel = &mut self.channels[speaker];
        match channel.pending.as_mut() {
            Some(previous) if candidate.start - previous.end < MIN_GAP_SECONDS => {
                previous.end = previous.end.max(candidate.end);
            }
            _ => {
                if let Some(previous) = channel.pending.replace(candidate) {
                    if previous.end - previous.start >= MIN_DURATION_SECONDS {
                        self.segments.push((previous, speaker));
                    }
                }
            }
        }
    }

    fn finish(mut self, audio_seconds: f64) -> Vec<Turn> {
        let timeline_end = self.next_frame as f64 * FRAME_SECONDS;
        for speaker in 0..MAX_SPEAKERS {
            if let Some(first) = self.channels[speaker].active_start.take() {
                self.candidate(
                    speaker,
                    Span {
                        start: (first as f64 * FRAME_SECONDS - PAD_SECONDS).max(0.0),
                        end: timeline_end,
                    },
                );
            }
            if let Some(mut pending) = self.channels[speaker].pending.take() {
                pending.end = pending.end.min(timeline_end);
                if pending.end - pending.start >= MIN_DURATION_SECONDS {
                    self.segments.push((pending, speaker));
                }
            }
        }
        let audio_end_ms = (audio_seconds * 1000.0).round() as i64;
        let mut turns: Vec<_> = self
            .segments
            .into_iter()
            .filter_map(|(span, speaker)| {
                let start_ms = ((span.start * 1000.0).round() as i64).clamp(0, audio_end_ms);
                let end_ms = ((span.end * 1000.0).round() as i64).clamp(start_ms, audio_end_ms);
                (end_ms > start_ms).then_some(Turn {
                    start_ms,
                    end_ms,
                    cluster: speaker as i32 + 1,
                })
            })
            .collect();
        turns.sort_by_key(|turn| (turn.start_ms, turn.end_ms, turn.cluster));
        turns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probabilities(frames: usize) -> Vec<f32> {
        let mut p = vec![0.1; frames * MAX_SPEAKERS];
        for (speaker, a, b) in [(0, 10, 40), (1, 30, 60), (0, 50, 70), (7, 90, 160)] {
            for frame in a..b.min(frames) {
                p[frame * MAX_SPEAKERS + speaker] = 0.9;
            }
        }
        p
    }

    #[test]
    fn model_config_matches_the_pinned_64_bit_c_abi() {
        assert_eq!(std::mem::size_of::<usize>(), 8);
        assert_eq!(std::mem::size_of::<ModelConfig>(), 56);
        assert_eq!(std::mem::offset_of!(ModelConfig, model_path), 8);
        assert_eq!(std::mem::offset_of!(ModelConfig, gpu), 16);
        assert_eq!(std::mem::offset_of!(ModelConfig, preset), 24);
        assert_eq!(std::mem::offset_of!(ModelConfig, chunk_frames), 32);
        assert_eq!(std::mem::offset_of!(ModelConfig, update_period_frames), 52);
    }

    #[test]
    fn chunks_and_overlapping_compacted_windows_preserve_full_timeline_once() {
        let p = probabilities(160);
        let mut whole = UniformPostprocessor::default();
        whole.ingest_window(0, &p).unwrap();
        let expected = whole.finish(1.6);
        for size in [1, 7, 15, 31, 90] {
            let mut post = UniformPostprocessor::default();
            for (index, chunk) in p.chunks(size * MAX_SPEAKERS).enumerate() {
                post.ingest_window(index * size, chunk).unwrap();
            }
            assert_eq!(post.finish(1.6), expected);
        }
        let mut post = UniformPostprocessor::default();
        post.ingest_window(0, &p[..65 * MAX_SPEAKERS]).unwrap();
        post.ingest_window(0, &p[..65 * MAX_SPEAKERS]).unwrap();
        assert_eq!(post.next_frame, 65);
        post.ingest_window(20, &p[20 * MAX_SPEAKERS..105 * MAX_SPEAKERS])
            .unwrap();
        post.ingest_window(95, &p[95 * MAX_SPEAKERS..]).unwrap();
        assert_eq!(post.next_frame, 160);
        assert_eq!(post.finish(1.6), expected);
        assert!(expected.iter().any(|t| t.cluster == 8));
    }

    #[test]
    fn invalid_probabilities_gaps_and_resets_fail_without_partial_consumption() {
        let mut post = UniformPostprocessor::default();
        for bad in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let mut row = vec![0.0; MAX_SPEAKERS];
            row[3] = bad;
            assert!(post.ingest_window(0, &row).is_err());
            assert_eq!(post.next_frame, 0);
        }
        assert!(post.ingest_window(0, &[0.0; 7]).is_err());
        assert!(post.ingest_window(1, &[0.0; MAX_SPEAKERS]).is_err());
        post.ingest_window(0, &[0.0; MAX_SPEAKERS * 2]).unwrap();
        assert!(post.ingest_window(0, &[0.0; MAX_SPEAKERS]).is_err());
        assert_eq!(post.next_frame, 2);
    }

    #[test]
    fn uniform_padding_gap_merge_finish_and_clamping_match_declared_policy() {
        let mut p = vec![0.1; 100 * MAX_SPEAKERS];
        // Two short regions merge after padding; a later region remains separate.
        for (a, b) in [(10, 20), (30, 40), (70, 100)] {
            for f in a..b {
                p[f * MAX_SPEAKERS] = 0.9;
            }
        }
        p[5 * MAX_SPEAKERS + 1] = 0.5; // equality alone must not start speech
        let mut post = UniformPostprocessor::default();
        post.ingest_window(0, &p).unwrap();
        assert_eq!(
            post.finish(0.975),
            vec![
                Turn {
                    start_ms: 50,
                    end_ms: 450,
                    cluster: 1
                },
                Turn {
                    start_ms: 650,
                    end_ms: 975,
                    cluster: 1
                },
            ]
        );
    }

    #[test]
    fn finish_does_not_pad_active_tail_past_the_audio_or_reorder_overlaps() {
        let mut p = vec![0.1; 20 * MAX_SPEAKERS];
        for f in 1..20 {
            p[f * MAX_SPEAKERS + 7] = 0.9;
        }
        for f in 10..19 {
            p[f * MAX_SPEAKERS] = 0.9;
        }
        let mut post = UniformPostprocessor::default();
        post.ingest_window(0, &p).unwrap();
        assert_eq!(
            post.finish(0.193),
            vec![
                Turn {
                    start_ms: 0,
                    end_ms: 193,
                    cluster: 8
                },
                Turn {
                    start_ms: 50,
                    end_ms: 193,
                    cluster: 1
                },
            ]
        );
    }

    #[test]
    #[ignore = "requires explicit local NeMo CPU runtime, Nemotron GGUF and 16 kHz WAV"]
    fn real_cpu_runtime_processes_existing_audio_and_resets_between_jobs() {
        let runtime = std::env::var_os("BAZZA_TEST_NEMO_RUNTIME").expect("BAZZA_TEST_NEMO_RUNTIME");
        let model =
            std::env::var_os("BAZZA_TEST_NEMOTRON_MODEL").expect("BAZZA_TEST_NEMOTRON_MODEL");
        let wav = std::env::var_os("BAZZA_TEST_NEMOTRON_WAV").expect("BAZZA_TEST_NEMOTRON_WAV");
        let reader = hound::WavReader::open(wav).unwrap();
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE as u32);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_format, hound::SampleFormat::Int);
        assert_eq!(reader.spec().bits_per_sample, 16);
        let pcm: Vec<_> = reader
            .into_samples::<i16>()
            .map(|v| v.unwrap() as f32 / 32768.0)
            .collect();
        let mut engine = NemotronEngine::load(Path::new(&runtime), Path::new(&model)).unwrap();
        eprintln!("Nemotron test: model validated; first job");
        let first = engine
            .diarize(&pcm, |p| eprintln!("Nemotron test: first job {p}"))
            .unwrap();
        eprintln!("Nemotron test: first job finished");
        let consumed = engine.consumed_frame_count();
        assert!(!first.is_empty());
        assert!(first.iter().all(|t| (1..=8).contains(&t.cluster)
            && t.start_ms >= 0
            && t.end_ms <= (pcm.len() as f64 / 16.0).round() as i64));
        assert_eq!(
            first,
            engine
                .diarize(&pcm, |p| eprintln!("Nemotron test: second job {p}"))
                .unwrap()
        );
        eprintln!("Nemotron test: second job finished");
        assert_eq!(consumed, engine.consumed_frame_count());
        let mut once = Some(pcm[..pcm.len().min(CHUNK_SAMPLES)].to_vec());
        assert!(engine
            .diarize_chunks(
                pcm.len() + 1,
                || Ok(once.take().unwrap_or_default()),
                |_| {}
            )
            .is_err());
        eprintln!("Nemotron test: dropping model with the runtime pinned");
        drop(engine);
        eprintln!("Nemotron test: drop finished");
        let mut fresh = NemotronEngine::load(Path::new(&runtime), Path::new(&model)).unwrap();
        assert_eq!(first, fresh.diarize(&pcm, |_| {}).unwrap());
        drop(fresh);
        eprintln!("Nemotron test: fresh engine processed and dropped");
    }
}

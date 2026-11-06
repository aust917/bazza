//! whisper.cpp wrapper. One `WhisperEngine` per loaded model; `transcribe`
//! creates a fresh decoding state per call so it is safe to share behind `Arc`.
//! Independent windows in a sequential batch can reuse a state via `job`.

use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    ffi::{c_char, c_void, CStr},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Once,
    },
    time::{Duration, Instant},
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Caption {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// False for live (streaming) results that may be revised by the post-call pass.
    pub is_final: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("whisper: {0}")]
    Whisper(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("transcription cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperBackend {
    Cpu,
    Cuda,
    Metal,
    Unverified,
}

impl WhisperBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Metal => "metal",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendObservation {
    Cuda,
    Metal,
    GpuNotRequested,
    GpuBuildUnavailable,
    GpuContextInitFailed,
    GpuDeviceNotFound,
    GpuBackendInitFailed,
    Unverified,
}

impl BackendObservation {
    fn backend(self) -> WhisperBackend {
        match self {
            Self::Cuda => WhisperBackend::Cuda,
            Self::Metal => WhisperBackend::Metal,
            Self::Unverified => WhisperBackend::Unverified,
            _ => WhisperBackend::Cpu,
        }
    }

    fn reason(self) -> Option<&'static str> {
        Some(match self {
            Self::Cuda | Self::Metal => return None,
            Self::GpuNotRequested => "gpu_not_requested",
            Self::GpuBuildUnavailable => "gpu_build_unavailable",
            Self::GpuContextInitFailed => "gpu_context_init_failed",
            Self::GpuDeviceNotFound => "gpu_device_not_found",
            Self::GpuBackendInitFailed => "gpu_backend_init_failed",
            Self::Unverified => "gpu_backend_unverified",
        })
    }
}

#[derive(Debug, Default)]
pub struct WhisperJobProfile {
    pub windows_seen: usize,
    /// Inputs shorter than 100 ms and inputs containing only digital silence.
    pub windows_skipped: usize,
    pub windows_inferred: usize,
    pub state_creations: usize,
    pub state_init: Duration,
    pub inference: Duration,
}

/// A sequential job owns its decoder buffers. The model can still be shared
/// with other jobs, each of which owns separate mutable decoder state.
pub struct WhisperJob<'a> {
    engine: &'a WhisperEngine,
    state: Option<WhisperState>,
    backend: BackendObservation,
    profile: WhisperJobProfile,
}

impl WhisperJob<'_> {
    /// Backend observed when the job's state was initialized, or the known
    /// configuration when no state has been needed yet.
    pub fn backend(&self) -> WhisperBackend {
        self.backend.backend()
    }

    /// A diagnostic code when GPU execution is unavailable or unverified.
    pub fn backend_reason(&self) -> Option<&'static str> {
        self.backend.reason()
    }

    pub fn profile(&self) -> &WhisperJobProfile {
        &self.profile
    }

    /// Initialize the job's state early when the caller must verify the
    /// backend before inference. Repeated calls reuse that same state.
    pub fn prepare(&mut self) -> Result<WhisperBackend, AsrError> {
        if self.state.is_none() {
            let started = Instant::now();
            let (state, backend) = self.engine.create_job_state();
            self.profile.state_init += started.elapsed();
            self.state = Some(state?);
            self.profile.state_creations += 1;
            self.backend = backend;
        }
        Ok(self.backend())
    }

    pub fn transcribe_opts(
        &mut self,
        pcm16k: &[f32],
        options: &TranscribeOpts,
        cancel: Option<&AtomicBool>,
    ) -> Result<Transcribed, AsrError> {
        if cancellation_requested(cancel) {
            return Err(AsrError::Cancelled);
        }
        self.profile.windows_seen += 1;
        if pcm16k.len() < 1600 || is_digital_silence(pcm16k) {
            self.profile.windows_skipped += 1;
            return Ok(Transcribed::default());
        }
        self.prepare()?;
        if cancellation_requested(cancel) {
            return Err(AsrError::Cancelled);
        }
        self.profile.windows_inferred += 1;
        let result = self.engine.decode(
            self.state.as_mut().expect("decoder state initialized"),
            pcm16k,
            options,
            cancel,
            Some(&mut self.profile.inference),
            None,
        );
        if result.is_err() {
            // Do not reuse state after a cancelled or failed native decode.
            self.state = None;
        }
        result
    }
}

pub struct WhisperEngine {
    ctx: WhisperContext,
    threads: i32,
    gpu_requested: bool,
    pub gpu: bool,
    /// Catalog id of the loaded model ("" when loaded from an arbitrary path).
    pub model_id: String,
}

impl WhisperEngine {
    /// Load a ggml model. With `use_gpu`, tries the GPU build first and falls
    /// back to CPU if initialisation fails.
    pub fn load(model: &Path, use_gpu: bool) -> Result<Self, AsrError> {
        if !model.exists() {
            return Err(AsrError::ModelNotFound(model.display().to_string()));
        }
        install_backend_observer();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8) as i32;
        let try_load = |gpu: bool| {
            let mut p = WhisperContextParameters::default();
            p.use_gpu(gpu);
            p.flash_attn(gpu && cfg!(feature = "cuda"));
            WhisperContext::new_with_params(model, p)
        };
        if use_gpu {
            match try_load(true) {
                Ok(ctx) => {
                    return Ok(WhisperEngine {
                        ctx, threads, gpu_requested: true, gpu: true, model_id: String::new(),
                    });
                }
                Err(error) => tracing::warn!(%error, "whisper GPU init failed; falling back to CPU"),
            }
        }
        let ctx = try_load(false).map_err(|e| AsrError::Whisper(e.to_string()))?;
        Ok(WhisperEngine {
            ctx, threads, gpu_requested: use_gpu, gpu: false, model_id: String::new(),
        })
    }

    /// Tag the engine with the catalog id it was loaded from.
    pub fn tagged(mut self, model_id: &str) -> Self {
        self.model_id = model_id.to_string();
        self
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// A GPU-capable configuration requires a job state to verify the backend.
    pub fn backend(&self) -> WhisperBackend {
        self.configured_backend().backend()
    }

    fn configured_backend(&self) -> BackendObservation {
        if !self.gpu_requested {
            BackendObservation::GpuNotRequested
        } else if !cfg!(any(feature = "cuda", feature = "metal")) {
            BackendObservation::GpuBuildUnavailable
        } else if !self.gpu {
            BackendObservation::GpuContextInitFailed
        } else {
            BackendObservation::Unverified
        }
    }

    pub fn job(&self) -> WhisperJob<'_> {
        WhisperJob {
            engine: self,
            state: None,
            backend: self.configured_backend(),
            profile: WhisperJobProfile::default(),
        }
    }

    fn create_job_state(&self) -> (Result<WhisperState, AsrError>, BackendObservation) {
        let mut backend = self.configured_backend();
        let observe = backend == BackendObservation::Unverified;
        if observe {
            BACKEND_OBSERVATION.set(Some(backend));
        }
        let state = self.ctx.create_state().map_err(|e| AsrError::Whisper(e.to_string()));
        if observe {
            backend = BACKEND_OBSERVATION.take().unwrap_or(BackendObservation::Unverified);
        }
        (state, backend)
    }

    /// Transcribe 16 kHz mono f32 audio. Timestamps are shifted by `offset_ms`.
    /// `lang` is an ISO code like "en"; `None` = auto-detect.
    pub fn transcribe(&self, pcm16k: &[f32], offset_ms: i64, lang: Option<&str>, is_final: bool) -> Result<Vec<Caption>, AsrError> {
        self.transcribe_with(pcm16k, offset_ms, lang, is_final, 0)
    }

    /// Like `transcribe`, but splits segments at word boundaries so no segment
    /// exceeds `max_len` characters (0 = whisper's default segmentation).
    /// Short segments make speaker assignment much more precise.
    pub fn transcribe_with(&self, pcm16k: &[f32], offset_ms: i64, lang: Option<&str>, is_final: bool, max_len: i32) -> Result<Vec<Caption>, AsrError> {
        Ok(self.transcribe_opts(pcm16k, &TranscribeOpts { offset_ms, lang, is_final, max_len, ..TranscribeOpts::default() })?.captions)
    }

    /// Full-control transcription: optional prompt (text that came just before
    /// this audio, for continuity), a no-speech cut-off, and the language whisper
    /// settled on (useful to lock the language for the rest of a live session).
    pub fn transcribe_opts(&self, pcm16k: &[f32], o: &TranscribeOpts) -> Result<Transcribed, AsrError> {
        self.transcribe_once(pcm16k, o, None, None)
    }

    /// Transcribe with bounded updates from Whisper's audio progress. A final
    /// 100 is delivered only after successful decoding. Skipped audio has no
    /// decode progress. The observer is invoked on the calling thread.
    pub fn transcribe_opts_with_progress(
        &self,
        pcm16k: &[f32],
        options: &TranscribeOpts,
        progress: impl FnMut(u8),
    ) -> Result<Transcribed, AsrError> {
        self.transcribe_opts_with_control(pcm16k, options, None, progress)
    }

    pub(crate) fn transcribe_opts_with_control(
        &self,
        pcm16k: &[f32],
        options: &TranscribeOpts,
        cancel: Option<&AtomicBool>,
        mut progress: impl FnMut(u8),
    ) -> Result<Transcribed, AsrError> {
        self.transcribe_once(pcm16k, options, cancel, Some(&mut progress))
    }

    fn transcribe_once(
        &self,
        pcm16k: &[f32],
        options: &TranscribeOpts,
        cancel: Option<&AtomicBool>,
        progress: Option<&mut dyn FnMut(u8)>,
    ) -> Result<Transcribed, AsrError> {
        if cancellation_requested(cancel) {
            return Err(AsrError::Cancelled);
        }
        if pcm16k.len() < 1600 || is_digital_silence(pcm16k) {
            return Ok(Transcribed::default());
        }
        let mut state = self.ctx.create_state().map_err(|e| AsrError::Whisper(e.to_string()))?;
        if cancellation_requested(cancel) {
            return Err(AsrError::Cancelled);
        }
        self.decode(&mut state, pcm16k, options, cancel, None, progress)
    }

    fn decode(
        &self,
        state: &mut WhisperState,
        pcm16k: &[f32],
        o: &TranscribeOpts,
        cancel: Option<&AtomicBool>,
        inference: Option<&mut Duration>,
        progress: Option<&mut dyn FnMut(u8)>,
    ) -> Result<Transcribed, AsrError> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(o.lang);
        params.set_translate(false);
        params.set_no_context(true);
        params.set_single_segment(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_token_timestamps(true);
        params.set_max_len(o.max_len);
        params.set_split_on_word(o.max_len > 0);
        if let Some(p) = o.prompt.filter(|p| !p.trim().is_empty()) {
            params.set_initial_prompt(p);
        }
        if let Some(flag) = cancel {
            // SAFETY: whisper.cpp invokes the callback synchronously inside
            // full. The borrowed atomic outlives full, and the callback only
            // reads it. This also avoids a boxed allocation on each window.
            unsafe {
                params.set_abort_callback(Some(abort_callback));
                params.set_abort_callback_user_data(std::ptr::from_ref(flag).cast_mut().cast());
            }
        }
        let mut progress = progress.map(ProgressObserver::new);
        if let Some(observer) = progress.as_mut() {
            // SAFETY: the synchronous full call invokes the callback serially
            // on this thread. The stack observer stays in place and alive
            // through that call, and the callback does not access C state.
            unsafe {
                params.set_progress_callback(Some(progress_callback));
                params.set_progress_callback_user_data(std::ptr::from_mut(observer).cast());
            }
        }
        let started = inference.as_ref().map(|_| Instant::now());
        let result = state.full(params, pcm16k);
        if let (Some(total), Some(started)) = (inference, started) {
            *total += started.elapsed();
        }
        if cancellation_requested(cancel) {
            return Err(AsrError::Cancelled);
        }
        result.map_err(|e| AsrError::Whisper(e.to_string()))?;

        let lang = whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string);
        let mut out = Vec::new();
        for seg in state.as_iter() {
            let text = seg.to_str_lossy().map(|c| c.trim().to_string()).unwrap_or_default();
            if text.is_empty() || is_noise_marker(&text) {
                continue;
            }
            if let Some(max) = o.drop_no_speech_above {
                if seg.no_speech_probability() > max {
                    continue;
                }
            }
            // whisper timestamps are in 10 ms units.
            out.push(Caption {
                start_ms: seg.start_timestamp() * 10 + o.offset_ms,
                end_ms: seg.end_timestamp() * 10 + o.offset_ms,
                text,
                is_final: o.is_final,
            });
        }
        if let Some(observer) = &mut progress {
            observer.report(100, Instant::now());
        }
        Ok(Transcribed { captions: out, lang })
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

struct ProgressObserver<'a> {
    callback: &'a mut dyn FnMut(u8),
    last: Option<(u8, Instant)>,
    disabled: bool,
}

impl<'a> ProgressObserver<'a> {
    fn new(callback: &'a mut dyn FnMut(u8)) -> Self {
        Self { callback, last: None, disabled: false }
    }

    fn report(&mut self, percent: u8, now: Instant) {
        if self.disabled || self.last.is_some_and(|(last, time)| {
            percent <= last || (percent < 100 && now.duration_since(time) < PROGRESS_INTERVAL)
        }) {
            return;
        }
        self.last = Some((percent, now));
        // An observer is optional. Its panic must never unwind across C or
        // prevent transcription from completing.
        self.disabled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.callback)(percent);
        })).is_err();
    }
}

unsafe extern "C" fn progress_callback(
    _: *mut whisper_rs::WhisperSysContext,
    _: *mut whisper_rs::WhisperSysState,
    percent: i32,
    data: *mut c_void,
) {
    if (0..100).contains(&percent) {
        // SAFETY: decode installs a live observer for this synchronous call.
        let observer = unsafe { &mut *data.cast::<ProgressObserver<'_>>() };
        observer.report(percent as u8, Instant::now());
    }
}

fn cancellation_requested(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

unsafe extern "C" fn abort_callback(data: *mut c_void) -> bool {
    // SAFETY: decode supplies a live shared AtomicBool for this synchronous call.
    unsafe { (&*data.cast::<AtomicBool>()).load(Ordering::Relaxed) }
}

thread_local! {
    static BACKEND_OBSERVATION: Cell<Option<BackendObservation>> = const { Cell::new(None) };
}

fn install_backend_observer() {
    static INSTALL: Once = Once::new();
    // SAFETY: the callback is static, takes no user data, and does not unwind.
    // Install once, before any thread can create a Whisper context or state.
    INSTALL.call_once(|| unsafe {
        whisper_rs::set_log_callback(Some(observe_backend), std::ptr::null_mut());
    });
}

unsafe extern "C" fn observe_backend(
    _level: whisper_rs::whisper_rs_sys::ggml_log_level,
    message: *const c_char,
    _: *mut c_void,
) {
    if message.is_null() {
        return;
    }
    let _ = BACKEND_OBSERVATION.try_with(|observation| {
        if observation.get().is_none() {
            return;
        }
        // SAFETY: Whisper supplies a NUL-terminated string for this callback.
        if let Ok(line) = unsafe { CStr::from_ptr(message) }.to_str() {
            if let Some(backend) = backend_from_log(line.trim_end()) {
                observation.set(Some(backend));
            }
        }
    });
}

fn backend_from_log(line: &str) -> Option<BackendObservation> {
    // whisper-rs exposes no backend query for a state. The vendored Whisper
    // reports the selected backend here, followed by failure if init fails.
    let detail = line.strip_prefix("whisper_backend_init_gpu: ")?;
    if detail == "no GPU found" {
        return Some(BackendObservation::GpuDeviceNotFound);
    }
    if detail.starts_with("failed to initialize ") {
        return Some(BackendObservation::GpuBackendInitFailed);
    }
    let name = detail.strip_prefix("using ")?.strip_suffix(" backend")?;
    if name.starts_with("CUDA") {
        Some(BackendObservation::Cuda)
    } else if name == "Metal" {
        Some(BackendObservation::Metal)
    } else {
        Some(BackendObservation::Unverified)
    }
}

/// Options for [`WhisperEngine::transcribe_opts`].
#[derive(Debug, Clone, Default)]
pub struct TranscribeOpts<'a> {
    pub offset_ms: i64,
    /// ISO code like "en"; `None` = auto-detect.
    pub lang: Option<&'a str>,
    pub is_final: bool,
    /// Max characters per segment (0 = whisper's own segmentation).
    pub max_len: i32,
    /// Text spoken just before this audio; keeps names and phrasing consistent.
    pub prompt: Option<&'a str>,
    /// Drop segments whose no-speech probability exceeds this (hallucination guard).
    pub drop_no_speech_above: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct Transcribed {
    pub captions: Vec<Caption>,
    /// Language whisper decided on (auto-detect) or was told.
    pub lang: Option<String>,
}

/// An exactly zero signal carries no speech. Do not apply an energy threshold:
/// quiet speech and speech surrounded by silence still need normal inference.
fn is_digital_silence(pcm: &[f32]) -> bool {
    pcm.iter().all(|&sample| sample == 0.0)
}

/// whisper emits bracketed markers for non-speech; drop them.
pub fn is_noise_marker(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')')) || t == "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digital_silence_includes_empty_and_signed_zero_pcm() {
        assert!(is_digital_silence(&[]));
        assert!(is_digital_silence(&vec![0.0; 16_000 * 12]));
        assert!(is_digital_silence(&[0.0, -0.0, 0.0]));
    }

    #[test]
    fn quiet_or_sparse_nonzero_pcm_is_not_discarded_as_digital_silence() {
        let mut pcm = vec![0.0; 16_000 * 12];
        for sample in [f32::from_bits(1), -f32::from_bits(1), 0.000_001, -0.000_001] {
            pcm[16_000 * 6] = sample;
            assert!(!is_digital_silence(&pcm), "discarded nonzero sample {sample}");
        }
    }

    #[test]
    fn noise_markers_detected() {
        assert!(is_noise_marker("[BLANK_AUDIO]"));
        assert!(is_noise_marker(" (music) "));
        assert!(!is_noise_marker("hello [there]"));
    }

    #[test]
    fn progress_is_monotonic_and_rate_limited_until_completion() {
        let mut updates = Vec::new();
        let mut callback = |percent| updates.push(percent);
        let mut observer = ProgressObserver::new(&mut callback);
        let now = Instant::now();
        observer.report(0, now);
        observer.report(9, now + Duration::from_millis(1));
        observer.report(25, now + PROGRESS_INTERVAL);
        observer.report(24, now + PROGRESS_INTERVAL * 2);
        observer.report(25, now + PROGRESS_INTERVAL * 2);
        observer.report(100, now + PROGRESS_INTERVAL);
        assert_eq!(updates, vec![0, 25, 100]);
    }

    #[test]
    fn backend_observation_requires_state_initialization() {
        assert_eq!(backend_from_log("whisper_init: use gpu = 1"), None);
        assert_eq!(
            backend_from_log("whisper_backend_init_gpu: using CUDA0 backend"),
            Some(BackendObservation::Cuda),
        );
        assert_eq!(
            backend_from_log("whisper_backend_init_gpu: failed to initialize CUDA0 backend"),
            Some(BackendObservation::GpuBackendInitFailed),
        );
        assert_eq!(
            backend_from_log("whisper_backend_init_gpu: using Metal backend"),
            Some(BackendObservation::Metal),
        );
        assert_eq!(
            backend_from_log("whisper_backend_init_gpu: no GPU found"),
            Some(BackendObservation::GpuDeviceNotFound),
        );
    }

    #[test]
    #[ignore = "needs a Whisper model; set BAZZA_TEST_MODEL"]
    fn job_reuses_cpu_state_with_independent_windows() {
        let model = std::env::var_os("BAZZA_TEST_MODEL").expect("BAZZA_TEST_MODEL");
        let engine = WhisperEngine::load(Path::new(&model), false).unwrap();
        let wav = std::io::Cursor::new(include_bytes!("../tests/fixtures/jfk.wav"));
        let pcm: Vec<f32> = hound::WavReader::new(wav)
            .unwrap()
            .into_samples::<i16>()
            .map(|sample| f32::from(sample.unwrap()) / 32768.0)
            .collect();
        let options = TranscribeOpts {
            lang: Some("en"),
            is_final: true,
            max_len: 80,
            ..TranscribeOpts::default()
        };
        let other_pcm = &pcm[pcm.len() / 2..];
        let expected = engine.transcribe_opts(&pcm, &options).unwrap();
        let mut updates = Vec::new();
        let other_expected = engine.transcribe_opts_with_progress(other_pcm, &options, |percent| {
            updates.push(percent);
        }).unwrap();
        assert_eq!(updates.first(), Some(&0));
        assert_eq!(updates.last(), Some(&100));
        assert!(!expected.captions.is_empty());

        let mut job = engine.job();
        assert!(matches!(
            job.transcribe_opts(&pcm, &options, Some(&AtomicBool::new(true))),
            Err(AsrError::Cancelled),
        ));
        assert_eq!(job.profile().state_creations, 0);
        let silent = job.transcribe_opts(&[0.0; 1600], &options, None).unwrap();
        assert!(silent.captions.is_empty());
        assert_eq!(job.profile().state_creations, 0);
        assert_eq!(job.prepare().unwrap(), WhisperBackend::Cpu);
        assert_eq!(job.prepare().unwrap(), WhisperBackend::Cpu);
        assert_eq!(job.backend_reason(), Some("gpu_not_requested"));
        assert_eq!(job.profile().state_creations, 1);
        for (samples, reference) in [(&pcm[..], &expected), (other_pcm, &other_expected), (&pcm[..], &expected)] {
            let actual = job.transcribe_opts(samples, &options, None).unwrap();
            assert_eq!(actual.captions, reference.captions);
            assert_eq!(actual.lang, reference.lang);
        }
        assert_eq!(job.backend(), WhisperBackend::Cpu);
        assert_eq!(job.profile().state_creations, 1);
        assert_eq!(job.profile().windows_seen, 4);
        assert_eq!(job.profile().windows_skipped, 1);
        assert_eq!(job.profile().windows_inferred, 3);
    }
}

//! Windows capture: a microphone, selected output endpoint, or one process tree.
//! Native packet frame positions keep continuous PCM independent of QPC jitter.

use crate::dsp::Resampler;
use crate::stream::{qpc_now, resampled_time, unix_now_us, AudioCaptureTime, AudioSink, CaptureClock, ErrorSink, NativePacketClock, StreamError, TimedAudioSink};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::core::{implement, Interface, Ref, Result as WinResult, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, EDataFlow, eCapture, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, BLOB, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{CreateEventW, OpenProcess, SetEvent, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::System::Variant::VT_BLOB;

/// Format we ask the loopback client for (process loopback has no mix format
/// to query, the caller picks one).
const CAPTURE_HZ: u32 = 48_000;
const CHANNELS: u16 = 2;

/// A running native loopback capture. Dropping it stops the thread without
/// waiting for it (a wedged WASAPI call must never block the app).
pub struct ProcessLoopback {
    stop: Arc<AtomicBool>,
    wake: Arc<WakeEvent>,
    _thread: Option<JoinHandle<()>>,
}

unsafe impl Send for ProcessLoopback {}

struct WakeEvent(HANDLE);
// The event may be signalled from the owner while the capture thread waits.
// Both hold an Arc, so the handle closes only after neither can use it.
unsafe impl Send for WakeEvent {}
unsafe impl Sync for WakeEvent {}
impl Drop for WakeEvent { fn drop(&mut self) { let _ = unsafe { CloseHandle(self.0) }; } }

impl Drop for ProcessLoopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            let _ = SetEvent(self.wake.0);
        }
        // The worker owns the other event reference. No join or handle-close race.
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion {
    tx: crossbeam_channel::Sender<WinResult<IAudioClient>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(&self, op: Ref<IActivateAudioInterfaceAsyncOperation>) -> WinResult<()> {
        let result = (|| -> WinResult<IAudioClient> {
            let op = op.ok()?;
            let mut hr = windows::core::HRESULT(0);
            let mut unk = None;
            unsafe { op.GetActivateResult(&mut hr, &mut unk)? };
            hr.ok()?;
            let unk: windows::core::IUnknown = unk.ok_or_else(|| windows::core::Error::from(hr))?;
            unk.cast::<IAudioClient>()
        })();
        let _ = self.tx.send(result);
        Ok(())
    }
}

/// Start capturing the audio of `pid` (and its child processes). `on_audio`
/// receives mono f32 at `target_hz`.
pub fn open_process_loopback(pid: u32, target_hz: u32, on_audio: AudioSink, on_error: ErrorSink) -> Result<ProcessLoopback, StreamError> {
    open_process_loopback_timed(pid, target_hz, Arc::new(move |pcm, _| on_audio(pcm)), on_error)
}

pub fn open_process_loopback_timed(pid: u32, target_hz: u32, on_audio: TimedAudioSink, on_error: ErrorSink) -> Result<ProcessLoopback, StreamError> {
    open_loopback(Source::Process(pid), target_hz, on_audio, on_error).map(|(stream, _)| stream)
}

/// Capture the exact selected IMMDevice endpoint. Following the default is an
/// explicit caller choice, never inferred from an endpoint currently being default.
pub(crate) fn open_endpoint_loopback_timed(endpoint: String, follow_default: bool, target_hz: u32,
    on_audio: TimedAudioSink, on_error: ErrorSink, on_poll: Option<crate::stream::HealthSink>) -> Result<(ProcessLoopback, u32), StreamError> {
    open_loopback(Source::Endpoint { endpoint, follow_default, on_poll }, target_hz, on_audio, on_error)
}

/// Native microphone input; no LOOPBACK flag is applied to a capture endpoint.
pub(crate) fn open_microphone_timed(endpoint: String, follow_default: bool, target_hz: u32,
    on_audio: TimedAudioSink, on_error: ErrorSink) -> Result<(ProcessLoopback, u32), StreamError> {
    open_loopback(Source::Microphone { endpoint, follow_default }, target_hz, on_audio, on_error)
}

enum Source {
    Process(u32),
    Endpoint { endpoint: String, follow_default: bool, on_poll: Option<crate::stream::HealthSink> },
    Microphone { endpoint: String, follow_default: bool },
}

fn packet_position(source: &Source, flags: u32, position: u64) -> Option<u64> {
    // The virtual process-loopback client can return zero for every device
    // position while QPC and the audio advance normally. Those zeros are not
    // duplicate packets. Keep process PCM in delivered frame order; endpoint
    // and microphone clients retain native gap/duplicate handling.
    if matches!(source, Source::Process(_)) || flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
        None
    } else { Some(position) }
}

fn should_report_discontinuity(source: &Source, discontinuity: bool, position: u64, had_packet: bool) -> bool {
    // A zero startup position is not an overrun. Later process discontinuities
    // still need a visible warning because this virtual counter can stay zero.
    // Missing duration is unknown; no gap is inferred from packet QPC jitter.
    discontinuity && (position != 0 || (matches!(source, Source::Process(_)) && had_packet))
}

fn capture_flags(source: &Source) -> u32 {
    if matches!(source, Source::Microphone { .. }) { AUDCLNT_STREAMFLAGS_EVENTCALLBACK }
    else { AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK }
}

fn default_flow(source: &Source) -> EDataFlow {
    if matches!(source, Source::Microphone { .. }) { eCapture } else { eRender }
}

fn endpoint_buffer_duration(source: &Source) -> i64 {
    // Preserve CPAL BufferSize::Default for microphone input. Existing endpoint
    // loopback retains its requested 200 ms ring-buffer headroom.
    if matches!(source, Source::Microphone { .. }) { 0 } else { 2_000_000 }
}

fn open_loopback(source: Source, target_hz: u32, on_audio: TimedAudioSink, on_error: ErrorSink) -> Result<(ProcessLoopback, u32), StreamError> {
    let stop = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(WakeEvent(unsafe { CreateEventW(None, false, false, None) }.map_err(|e| StreamError::Cpal(e.to_string()))?));
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<u32, String>>(1);
    let stop2 = stop.clone();
    let thread_wake = wake.clone();
    let name = if matches!(&source, Source::Microphone { .. }) { "bazza-native-microphone" } else { "bazza-system-loopback" };
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let r = unsafe { run(source, target_hz, thread_wake.0, stop2, on_audio, on_error, ready_tx) };
            if let Err(e) = r {
                tracing::warn!("native audio capture ended: {e}");
            }
        })
        .map_err(|e| StreamError::Cpal(e.to_string()))?;
    match ready_rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(rate)) => Ok((ProcessLoopback { stop, wake, _thread: Some(thread) }, rate)),
        Ok(Err(e)) => {
            stop.store(true, Ordering::SeqCst);
            Err(StreamError::Cpal(e))
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            Err(StreamError::Cpal("native audio capture did not start in time".into()))
        }
    }
}

fn validate_process_target(pid: u32) -> Result<(), String> {
    // WASAPI can successfully activate a silent virtual client for a PID that
    // does not exist. Reject a provably absent/exited target so the existing
    // caller fallback is visible. Other query failures do not prove absence:
    // let WASAPI decide, including for protected targets. This startup check
    // does not monitor root exit while descendants may still be playing.
    let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => handle,
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => return Err(format!("selected audio process is unavailable: {error}")),
        Err(_) => return Ok(()),
    };
    let state = unsafe { WaitForSingleObject(handle, 0) };
    let _ = unsafe { CloseHandle(handle) };
    match state {
        WAIT_OBJECT_0 => Err("selected audio process has already exited".into()),
        _ => Ok(()),
    }
}

unsafe fn activate_process(pid: u32) -> Result<IAudioClient, String> {
        validate_process_target(pid)?;
        // Activation parameters travel as a VT_BLOB PROPVARIANT.
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE },
            },
        };
        let blob = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB, wReserved1: 0, wReserved2: 0, wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB { cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: &mut params as *mut _ as *mut u8 },
                    },
                }),
            },
        });
        let (tx, rx) = crossbeam_channel::bounded::<WinResult<IAudioClient>>(1);
        let handler: IActivateAudioInterfaceCompletionHandler = Completion { tx }.into();
        let _op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*blob as *const PROPVARIANT), &handler)
            .map_err(|e| format!("activate process loopback: {e}"))?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "activation timed out".to_string())?.map_err(|e| format!("activate process loopback: {e}"))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Encoding { Unsigned8, SignedPcm, Float }

#[derive(Clone, Copy, Debug)]
struct CaptureFormat { rate: u32, channels: usize, bytes: usize, block_align: usize, encoding: Encoding }

impl CaptureFormat {
    fn parse(format: WAVEFORMATEX, subtype: Option<GUID>, valid_bits: Option<u16>) -> Result<Self, String> {
        let bits = format.wBitsPerSample;
        let tag = format.wFormatTag;
        let pcm = tag == 1 || (tag == 0xfffe && subtype == Some(GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)));
        let float = tag == WAVE_FORMAT_IEEE_FLOAT as u16 || (tag == 0xfffe && subtype == Some(GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71)));
        let encoding = match (pcm, float, bits) {
            (true, _, 8) => Encoding::Unsigned8,
            (true, _, 16 | 24 | 32 | 64) => Encoding::SignedPcm,
            (_, true, 32 | 64) => Encoding::Float,
            _ => return Err("unsupported Windows loopback sample format".into()),
        };
        let channels = format.nChannels as usize;
        let bytes = bits as usize / 8;
        let rate = format.nSamplesPerSec;
        let block_align = format.nBlockAlign as usize;
        if rate == 0 || channels == 0 || channels > 256 || block_align != channels * bytes
            || valid_bits.is_some_and(|v| v > bits || (v == 0 && encoding != Encoding::Float)) {
            return Err("invalid Windows loopback audio format".into());
        }
        Ok(Self { rate, channels, bytes, block_align, encoding })
    }

    /// WASAPI integer samples occupy the most-significant bits of their
    /// container (including 24 valid bits in a 32-bit PCM container).
    fn sample(&self, bytes: &[u8]) -> f32 {
        match self.encoding {
            Encoding::Unsigned8 => (bytes[0] as f32 - 128.0) / 128.0,
            Encoding::SignedPcm => match self.bytes {
                2 => i16::from_le_bytes(bytes.try_into().unwrap()) as f32 / 32768.0,
                3 => { let v = ((bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16)) << 8 >> 8; v as f32 / 8_388_608.0 },
                4 => i32::from_le_bytes(bytes.try_into().unwrap()) as f32 / 2_147_483_648.0,
                8 => (i64::from_le_bytes(bytes.try_into().unwrap()) as f64 / 9_223_372_036_854_775_808.0) as f32,
                _ => unreachable!(),
            },
            Encoding::Float => match self.bytes {
                4 => f32::from_le_bytes(bytes.try_into().unwrap()),
                8 => f64::from_le_bytes(bytes.try_into().unwrap()) as f32,
                _ => unreachable!(),
            },
        }
    }

    fn decode(&self, data: &[u8], frames: usize, silent: bool) -> Result<Vec<f32>, String> {
        if silent { return Ok(vec![0.0; frames]); }
        let length = frames.checked_mul(self.block_align).ok_or("loopback packet size overflow")?;
        if data.len() != length { return Err("invalid Windows loopback packet length".into()); }
        Ok(data.chunks_exact(self.block_align).map(|frame| frame.chunks_exact(self.bytes).map(|s| self.sample(s)).sum::<f32>() / self.channels as f32).collect())
    }
}

unsafe fn decode_packet(format: &CaptureFormat, data: *const u8, frames: usize, max_frames: usize, flags: u32) -> Result<Vec<f32>, String> {
    if frames > max_frames { return Err("Windows loopback packet exceeds capture buffer".into()); }
    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
        return format.decode(&[], frames, true);
    }
    if data.is_null() { return Err("Windows loopback returned a null non-silent packet".into()); }
    let length = frames.checked_mul(format.block_align).ok_or("Windows loopback packet size overflow")?;
    format.decode(std::slice::from_raw_parts(data, length), frames, false)
}

struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat { fn drop(&mut self) { unsafe { CoTaskMemFree(Some(self.0.cast())); } } }

struct ComLease;
impl Drop for ComLease { fn drop(&mut self) { unsafe { CoUninitialize(); } } }

struct StartedClient<'a>(&'a IAudioClient);
impl Drop for StartedClient<'_> { fn drop(&mut self) { let _ = unsafe { self.0.Stop() }; } }

unsafe fn initialize(source: &Source) -> Result<(IAudioClient, CaptureFormat), String> {
    let flags = capture_flags(source);
    match source {
        Source::Process(pid) => {
            let client = activate_process(*pid)?;
            let fmt = WAVEFORMATEX { wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16, nChannels: CHANNELS,
                nSamplesPerSec: CAPTURE_HZ, nAvgBytesPerSec: CAPTURE_HZ * CHANNELS as u32 * 4,
                nBlockAlign: CHANNELS * 4, wBitsPerSample: 32, cbSize: 0 };
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt, None).map_err(|e| format!("initialize process loopback: {e}"))?;
            Ok((client, CaptureFormat::parse(fmt, None, None)?))
        },
        Source::Endpoint { endpoint, .. } | Source::Microphone { endpoint, .. } => {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("audio enumerator: {e}"))?;
            let id: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
            let device = enumerator.GetDevice(PCWSTR(id.as_ptr())).map_err(|e| format!("selected audio endpoint: {e}"))?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| format!("activate selected audio endpoint: {e}"))?;
            let mix = MixFormat(client.GetMixFormat().map_err(|e| format!("read endpoint mix format: {e}"))?);
            if mix.0.is_null() { return Err("Windows returned no endpoint mix format".into()); }
            let fmt = std::ptr::read_unaligned(mix.0);
            let (subtype, valid_bits) = if fmt.wFormatTag == 0xfffe {
                if fmt.cbSize < 22 { return Err("incomplete Windows extensible mix format".into()); }
                let ext = std::ptr::read_unaligned(mix.0.cast::<WAVEFORMATEXTENSIBLE>());
                (Some(ext.SubFormat), Some(ext.Samples.wValidBitsPerSample))
            } else { (None, None) };
            let format = CaptureFormat::parse(fmt, subtype, valid_bits)?;
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, endpoint_buffer_duration(source), 0, mix.0, None).map_err(|e| format!("initialize audio endpoint capture: {e}"))?;
            Ok((client, format))
        },
    }
}

unsafe fn default_endpoint(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Result<String, String> {
    let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole).map_err(|e| format!("default audio endpoint unavailable: {e}"))?;
    let id = device.GetId().map_err(|e| format!("read default audio endpoint: {e}"))?;
    let result = id.to_string().map_err(|e| format!("read default audio endpoint identity: {e}"));
    CoTaskMemFree(Some(id.0.cast()));
    result
}

/// Handle native missing/duplicate frames before SRC. Small gaps pass through
/// the filter as zeros; long gaps retain only its bounded tail and leave the
/// sparse interior to the archive writer. QPC changes never create a gap here.
struct PacketResampler {
    filter: Resampler,
    from_hz: u32,
    to_hz: u32,
    input_frames: u64,
    output_frames: u64,
    next_native: Option<u64>,
    last_end: Option<AudioCaptureTime>,
    estimated_pending: bool,
}

impl PacketResampler {
    fn new(from_hz: u32, to_hz: u32) -> Self {
        Self { filter: Resampler::new(from_hz, to_hz), from_hz, to_hz, input_frames: 0, output_frames: 0, next_native: None, last_end: None, estimated_pending: false }
    }

    fn shifted(&self, time: AudioCaptureTime, frames: usize) -> AudioCaptureTime {
        AudioCaptureTime { unix_us: time.unix_us.saturating_add((frames as u128 * 1_000_000 / self.from_hz as u128).min(i64::MAX as u128) as i64), ..time }
    }

    fn push(&mut self, input: &[f32], time: AudioCaptureTime) -> (Vec<f32>, AudioCaptureTime) {
        let mut output_time = resampled_time(time, self.input_frames, self.output_frames, self.from_hz, self.to_hz);
        let output = self.filter.process(input);
        self.estimated_pending |= !time.device_timestamp;
        if !output.is_empty() && self.estimated_pending { output_time.device_timestamp = false; self.estimated_pending = false; }
        self.input_frames += input.len() as u64;
        self.output_frames += output.len() as u64;
        self.last_end = Some(self.shifted(time, input.len()));
        (output, output_time)
    }

    fn finish_segment(&mut self) -> Option<(Vec<f32>, AudioCaptureTime)> {
        let end = self.last_end?;
        let count = ((self.input_frames as u128 * self.to_hz as u128).div_ceil(self.from_hz as u128)).saturating_sub(self.output_frames as u128) as usize;
        let mut time = resampled_time(end, self.input_frames, self.output_frames, self.from_hz, self.to_hz);
        let mut tail = if count > 0 { self.filter.process(&[0.0; 32]) } else { Vec::new() };
        tail.truncate(count);
        if !tail.is_empty() && self.estimated_pending { time.device_timestamp = false; self.estimated_pending = false; }
        self.output_frames += tail.len() as u64;
        self.filter = Resampler::after_silence(self.from_hz, self.to_hz, self.input_frames);
        self.last_end = None;
        Some((tail, time))
    }

    fn process(&mut self, mut input: &[f32], mut time: AudioCaptureTime, position: Option<u64>, discontinuity: bool) -> Vec<(Vec<f32>, AudioCaptureTime)> {
        let mut blocks = Vec::with_capacity(3);
        let previous = self.next_native;
        let reset = discontinuity && (!time.device_timestamp || position.zip(previous).is_some_and(|(p, end)| p < end));
        if reset {
            if let Some(tail) = self.finish_segment() { blocks.push(tail); }
            self.next_native = position.map(|p| p.saturating_add(input.len() as u64));
        } else if let Some((position, end)) = position.zip(previous) {
            self.next_native = Some(end.max(position.saturating_add(input.len() as u64)));
            if position < end {
                let duplicate = end.saturating_sub(position).min(input.len() as u64) as usize;
                input = &input[duplicate..];
                time = self.shifted(time, duplicate);
            } else if position > end {
                let gap = position - end;
                // This threshold selects dense versus sparse representation,
                // not whether a native gap exists. Every missing frame counts.
                let dense_limit = (self.from_hz as u64 / 100).max(64);
                if gap <= dense_limit {
                    if let Some(end) = self.last_end { blocks.push(self.push(&vec![0.0; gap as usize], end)); }
                } else {
                    // Preserve both finite filter edges of the real silent
                    // interval, including ringing after the old packet and
                    // pre-echo before the next. Only its all-zero middle skips SRC.
                    const EDGE: usize = 32;
                    if let Some(end) = self.last_end { blocks.push(self.push(&[0.0; EDGE], end)); }
                    self.input_frames = self.input_frames.saturating_add(gap - 2 * EDGE as u64);
                    self.output_frames = (self.input_frames as u128 * self.to_hz as u128).div_ceil(self.from_hz as u128).min(u64::MAX as u128) as u64;
                    self.filter = Resampler::after_silence(self.from_hz, self.to_hz, self.input_frames);
                    let suffix_time = AudioCaptureTime { unix_us: time.unix_us.saturating_sub((EDGE as u128 * 1_000_000 / self.from_hz as u128) as i64), ..time };
                    blocks.push(self.push(&[0.0; EDGE], suffix_time));
                }
            }
        } else {
            self.next_native = match position {
                Some(p) => Some(p.saturating_add(input.len() as u64)),
                None => self.next_native.map(|p| p.saturating_add(input.len() as u64)),
            };
        }
        if !input.is_empty() { blocks.push(self.push(input, time)); }
        blocks
    }
}

unsafe fn run(
    source: Source,
    target_hz: u32,
    wake: HANDLE,
    stop: Arc<AtomicBool>,
    on_audio: TimedAudioSink,
    on_error: ErrorSink,
    ready: crossbeam_channel::Sender<Result<u32, String>>,
) -> Result<(), String> {
    if let Err(error) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
        let error = format!("initialize audio capture COM thread: {error}");
        let _ = ready.try_send(Err(error.clone()));
        return Err(error);
    }
    let _com = ComLease;
    let mut started = false;
    let result = (|| -> Result<(), String> {
        let (client, format) = initialize(&source)?;
        if stop.load(Ordering::SeqCst) { return Err("native audio capture start was cancelled".into()); }
        let following = match &source { Source::Endpoint { endpoint, follow_default: true, .. } | Source::Microphone { endpoint, follow_default: true } => Some(endpoint), _ => None };
        let enumerator: Option<IMMDeviceEnumerator> = if following.is_some() {
            Some(CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("audio default monitor: {e}"))?)
        } else { None };
        let mut next_default_check = Instant::now() + Duration::from_secs(1);
        let max_frames = match client.GetBufferSize() {
            Ok(frames) if frames > 0 => frames as usize,
            // Some virtual process-loopback clients do not expose a useful
            // endpoint ring size. Keep the prior path usable, bounded by its
            // requested 200 ms buffer rather than rejecting every packet.
            _ if matches!(&source, Source::Process(_)) => (format.rate as usize).div_ceil(5),
            Ok(_) => return Err("Windows audio endpoint returned an empty capture buffer".into()),
            Err(e) => return Err(format!("capture buffer size: {e}")),
        };
        client.SetEventHandle(wake).map_err(|e| format!("event: {e}"))?;
        let capture: IAudioCaptureClient = client.GetService().map_err(|e| format!("capture client: {e}"))?;
        let clock = CaptureClock::new();
        let before = unix_now_us();
        if let Some(now) = qpc_now() {
            let after = unix_now_us();
            clock.anchor(now, before + (after - before) / 2);
        }
        if stop.load(Ordering::SeqCst) { return Err("native audio capture start was cancelled".into()); }
        client.Start().map_err(|e| format!("start: {e}"))?;
        let _client_stop = StartedClient(&client);
        started = true;
        let _ = ready.send(Ok(format.rate));
        if matches!(&source, Source::Microphone { .. }) {
            tracing::info!(rate = format.rate, channels = format.channels, bytes_per_sample = format.bytes, "native Windows microphone capture started");
        } else {
            tracing::info!(rate = format.rate, channels = format.channels, bytes_per_sample = format.bytes, "native Windows loopback capture started");
        }

        let mut packet_clock = NativePacketClock::new(format.rate);
        let mut resampler = PacketResampler::new(format.rate, target_hz);
        let mut had_packet = false;
        // Explicit local diagnostics: bounded numeric packet metadata, never PCM.
        // Kept off by default, including for ordinary microphone recordings.
        let mut trace_remaining = if std::env::var_os("BAZZA_CAPTURE_PACKET_TRACE").is_some() { 64u8 } else { 0 };
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            // Process loopback does not always signal the event; poll on timeout too.
            let _ = WaitForSingleObject(wake, 100);
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if Instant::now() >= next_default_check {
                next_default_check = Instant::now() + Duration::from_secs(1);
                if let (Some(enumerator), Some(expected)) = (&enumerator, following) {
                    if &default_endpoint(enumerator, default_flow(&source))? != expected {
                        return Err(if matches!(&source, Source::Microphone { .. }) {
                            "Windows default microphone changed; reopening capture"
                        } else { "Windows default output changed; reopening system audio" }.into());
                    }
                }
            }
            loop {
                if stop.load(Ordering::SeqCst) { break; }
                let packet = match capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(e) => {
                        return Err(format!("native audio capture: {e}"));
                    }
                };
                if packet == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc_position = 0u64;
                let mut device_position = 0u64;
                if let Err(e) = capture.GetBuffer(&mut data, &mut frames, &mut flags, Some(&mut device_position), Some(&mut qpc_position)) {
                    return Err(format!("native audio capture: {e}"));
                }
                if frames == 0 { break; }
                let timing = if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 {
                    qpc_now().map(|callback| clock.timestamp(cpal::InputStreamTimestamp {
                        capture: cpal::StreamInstant::from_micros(qpc_position / 10), callback,
                    }, frames as usize, format.rate)).unwrap_or_else(|| clock.estimate(frames as usize, format.rate))
                } else { clock.estimate(frames as usize, format.rate) };
                let position = packet_position(&source, flags, device_position);
                let valid_position = position.is_some();
                let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
                let timing = packet_clock.timestamp(timing, position, frames as usize, discontinuity);
                // SILENT packets may have null or unspecified buffer contents.
                // Copy/convert while the buffer is owned, then always release it.
                let decoded = decode_packet(&format, data, frames as usize, max_frames, flags);
                let released = capture.ReleaseBuffer(frames).map_err(|e| format!("release loopback packet: {e}"));
                let mono = decoded?;
                released?;
                if should_report_discontinuity(&source, discontinuity, device_position, had_packet) {
                    on_error(if matches!(&source, Source::Microphone { .. }) {
                        "microphone overrun: native packet discontinuity"
                    } else { "system loopback overrun: native packet discontinuity" }.into());
                }
                had_packet = true;
                let previous_native_end = resampler.next_native;
                let blocks = resampler.process(&mono, timing, position, discontinuity);
                if trace_remaining > 0 {
                    trace_remaining -= 1;
                    let source = match &source { Source::Microphone { .. } => "microphone", Source::Endpoint { .. } => "endpoint", Source::Process(_) => "process" };
                    let emitted_frames: usize = blocks.iter().map(|(audio, _)| audio.len()).sum();
                    tracing::info!(source, frames, flags, device_position, qpc_position, valid_position,
                        discontinuity, previous_native_end, emitted_frames, "native capture packet trace");
                }
                for (audio, timing) in blocks {
                    if !audio.is_empty() { on_audio(&audio, timing); }
                }
            }
            // A working output can be idle and yield no packets. A completed
            // poll distinguishes that from a capture thread that stopped responding.
            if let Source::Endpoint { on_poll: Some(on_poll), .. } = &source {
                on_poll();
            }
        }
        if let Some((tail, timing)) = resampler.finish_segment() {
            if !tail.is_empty() { on_audio(&tail, timing); }
        }
        Ok(())
    })();
    if let Err(e) = &result {
        if started { on_error(e.clone()); }
        let _ = ready.try_send(Err(e.clone()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_target_rejects_absent_and_exited_processes_but_accepts_live_pid() {
        use std::os::windows::process::CommandExt;
        assert!(validate_process_target(std::process::id()).is_ok());
        assert!(validate_process_target(0).is_err());
        let mut child = std::process::Command::new("cmd.exe").args(["/d", "/c", "exit 0"])
            .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        assert!(validate_process_target(pid).is_err());
    }

    #[test]
    fn absent_process_fails_startup_without_opening_an_audio_client() {
        let result = open_process_loopback_timed(0, 16_000,
            Arc::new(|_, _| panic!("an absent process must not emit audio")),
            Arc::new(|_| panic!("startup failure must be returned to the caller")));
        assert!(result.is_err());
    }

    fn format(tag: u16, bits: u16, channels: u16) -> WAVEFORMATEX {
        WAVEFORMATEX { wFormatTag: tag, nChannels: channels, nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * channels as u32 * bits as u32 / 8,
            nBlockAlign: channels * bits / 8, wBitsPerSample: bits, cbSize: if tag == 0xfffe { 22 } else { 0 } }
    }

    fn signal(start: u64, len: usize) -> Vec<f32> {
        (start..start + len as u64).map(|n| (((n.wrapping_mul(1664525).wrapping_add(1013904223)) >> 5) % 997) as f32 / 1994.0 - 0.25).collect()
    }

    fn time(frame: u64, rate: u32, jitter: i64) -> AudioCaptureTime {
        AudioCaptureTime { unix_us: 1_000_000 + (frame * 1_000_000 / rate as u64) as i64 + jitter, device_timestamp: true }
    }

    #[test]
    fn process_discontinuity_is_visible_after_startup_even_with_zero_positions() {
        let process = Source::Process(42);
        assert!(!should_report_discontinuity(&process, true, 0, false));
        assert!(should_report_discontinuity(&process, true, 0, true));
        assert!(!should_report_discontinuity(&process, false, 0, true));
        assert!(!should_report_discontinuity(&process, false, 480, true));
        let endpoint = Source::Endpoint { endpoint: "output".into(), follow_default: false, on_poll: None };
        let microphone = Source::Microphone { endpoint: "input".into(), follow_default: false };
        for source in [endpoint, microphone] {
            assert!(!should_report_discontinuity(&source, true, 0, false));
            assert!(!should_report_discontinuity(&source, true, 0, true));
            assert!(should_report_discontinuity(&source, true, 480, false));
            assert!(should_report_discontinuity(&source, true, 480, true));
        }
        assert!(!crate::recorder::is_fatal_stream_error("system loopback overrun: native packet discontinuity"));
    }

    #[test]
    fn process_packets_with_constant_zero_positions_preserve_every_delivered_frame() {
        let source = Source::Process(42);
        let expected = signal(0, 64 * 480);
        let mut clock = NativePacketClock::new(48_000);
        let mut resampler = PacketResampler::new(48_000, 48_000);
        let mut actual = Vec::new();
        for (index, pcm) in expected.chunks(480).enumerate() {
            let position = packet_position(&source, 0, 0);
            let observed = time((index * 480) as u64, 48_000, [0, 1700, -900][index % 3]);
            let timing = clock.timestamp(observed, position, pcm.len(), index == 0);
            for (audio, timing) in resampler.process(pcm, timing, position, index == 0) {
                assert_eq!(timing.unix_us, 1_000_000 + index as i64 * 10_000);
                assert!(!timing.device_timestamp, "virtual zero positions must not be advertised as native counters");
                actual.extend(audio);
            }
        }
        if let Some((audio, _)) = resampler.finish_segment() { actual.extend(audio); }
        assert_eq!(actual, expected, "zero process positions must not discard real audio as duplicates");
        let endpoint = Source::Endpoint { endpoint: "output".into(), follow_default: false, on_poll: None };
        let microphone = Source::Microphone { endpoint: "input".into(), follow_default: false };
        for source in [endpoint, microphone] {
            assert_eq!(packet_position(&source, 0, 960), Some(960));
            assert_eq!(packet_position(&source, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32, 960), None);
        }
    }

    #[test]
    fn microphone_endpoint_uses_capture_flow_without_loopback_and_outputs_are_unchanged() {
        let microphone = Source::Microphone { endpoint: "test-input".into(), follow_default: true };
        assert_eq!(capture_flags(&microphone), AUDCLNT_STREAMFLAGS_EVENTCALLBACK);
        assert_eq!(capture_flags(&microphone) & AUDCLNT_STREAMFLAGS_LOOPBACK, 0);
        assert_eq!(default_flow(&microphone), eCapture);
        assert_eq!(endpoint_buffer_duration(&microphone), 0);
        for source in [Source::Process(42), Source::Endpoint { endpoint: "test-output".into(), follow_default: false, on_poll: None }] {
            assert_eq!(capture_flags(&source), AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK);
            assert_eq!(default_flow(&source), eRender);
            assert_eq!(endpoint_buffer_duration(&source), 2_000_000);
        }
    }

    #[test]
    fn endpoint_float_and_integer_containers_preserve_channels_and_levels() {
        let float = CaptureFormat::parse(format(0xfffe, 32, 2), Some(GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71)), Some(32)).unwrap();
        let bytes: Vec<u8> = [0.25f32, 0.75, -0.75, 0.25].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(float.decode(&bytes, 2, false).unwrap(), vec![0.5, -0.25]);
        let integer = CaptureFormat::parse(format(0xfffe, 32, 2), Some(GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)), Some(24)).unwrap();
        let bytes: Vec<u8> = [0x40000000i32, i32::MIN].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(integer.decode(&bytes, 1, false).unwrap(), vec![-0.25]);
        let packed = CaptureFormat::parse(format(1, 24, 2), None, None).unwrap();
        assert_eq!(packed.decode(&[0, 0, 0x40, 0, 0, 0x80], 1, false).unwrap(), vec![-0.25]);
        assert!(CaptureFormat::parse(format(1, 12, 2), None, None).is_err());
        assert!(float.decode(&[], 1, false).is_err());
    }

    #[test]
    fn silent_flag_ignores_the_pointer_and_normal_packets_are_owned_before_release() {
        let float = CaptureFormat::parse(format(3, 32, 2), None, None).unwrap();
        assert_eq!(unsafe { decode_packet(&float, std::ptr::null(), 4, 480, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) }.unwrap(), vec![0.0; 4]);
        assert!(unsafe { decode_packet(&float, std::ptr::null(), 4, 480, 0) }.is_err());
        let mut bytes: Vec<u8> = [0.25f32, 0.75].iter().flat_map(|s| s.to_le_bytes()).collect();
        let decoded = unsafe { decode_packet(&float, bytes.as_ptr(), 1, 480, 0) }.unwrap();
        bytes.fill(0xff);
        assert_eq!(decoded, vec![0.5]);
        assert!(unsafe { decode_packet(&float, bytes.as_ptr(), 481, 480, 0) }.is_err());
    }

    #[test]
    fn jitter_does_not_modify_pcm_and_small_native_gaps_and_overlaps_are_exact() {
        let mut clock = NativePacketClock::new(48_000);
        let mut src = PacketResampler::new(48_000, 48_000);
        let mut actual = Vec::new();
        for (i, (position, frames)) in [(0, 480), (497, 480), (928, 480), (1457, 480)].into_iter().enumerate() {
            let observed = time(position, 48_000, if i % 2 == 0 { 0 } else { 1521 });
            let normalized = clock.timestamp(observed, Some(position), frames, false);
            for (audio, _) in src.process(&signal(position, frames), normalized, Some(position), false) { actual.extend(audio); }
        }
        let mut expected = signal(0, 1937);
        expected[480..497].fill(0.0);
        expected[1408..1457].fill(0.0);
        assert_eq!(actual, expected);
    }

    #[test]
    fn uncertain_packet_without_reset_retains_counter_prediction_for_the_next_small_gap() {
        let mut clock = NativePacketClock::new(48_000);
        let mut src = PacketResampler::new(48_000, 48_000);
        let mut actual = Vec::new();
        for (virtual_position, native_position) in [(0, Some(0)), (480, None), (983, Some(983))] {
            let normalized = clock.timestamp(time(virtual_position, 48_000, 1521), native_position, 480, false);
            for (audio, _) in src.process(&signal(virtual_position, 480), normalized, native_position, false) { actual.extend(audio); }
        }
        let mut expected = signal(0, 1463);
        expected[960..983].fill(0.0);
        assert_eq!(actual, expected);
    }

    #[test]
    fn resampled_history_keeps_uncertain_origin_visible_on_the_first_output() {
        let mut clock = NativePacketClock::new(44_100);
        let mut src = PacketResampler::new(44_100, 48_000);
        let initial = clock.timestamp(AudioCaptureTime { unix_us: 1_000_000, device_timestamp: false }, None, 8, false);
        assert!(src.process(&signal(0, 8), initial, None, false).iter().all(|(audio, _)| audio.is_empty()));
        let valid = clock.timestamp(time(8, 44_100, 1521), Some(8), 441, false);
        let output = src.process(&signal(8, 441), valid, Some(8), false);
        let first = output.iter().find(|(audio, _)| !audio.is_empty()).unwrap();
        assert!(!first.1.device_timestamp, "a buffered uncertain packet must still mark the archive clock estimated");
    }

    fn resampled_with_partition(input: &[f32], sizes: &[usize]) -> Vec<f32> {
        let mut clock = NativePacketClock::new(44_100);
        let mut src = PacketResampler::new(44_100, 48_000);
        let mut actual = Vec::new();
        let mut offset = 0;
        for size in sizes.iter().cycle() {
            if offset == input.len() { break; }
            let count = (*size).min(input.len() - offset);
            let normalized = clock.timestamp(time(offset as u64, 44_100, if offset == 0 { 0 } else { 1521 }), Some(offset as u64), count, false);
            for (audio, _) in src.process(&input[offset..offset + count], normalized, Some(offset as u64), false) { actual.extend(audio); }
            offset += count;
        }
        if let Some((tail, _)) = src.finish_segment() { actual.extend(tail); }
        actual
    }

    #[test]
    fn nonidentity_src_preserves_sample_count_independent_of_packet_partition() {
        let input = signal(0, 44_100);
        let whole = resampled_with_partition(&input, &[input.len()]);
        let packets = resampled_with_partition(&input, &[73, 211, 441, 997, 128]);
        assert_eq!(whole.len(), 48_000);
        assert_eq!(packets.len(), whole.len());
        let error = whole.iter().zip(&packets).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(error < 1e-5, "packet-dependent SRC error: {error}");
    }

    #[test]
    fn sparse_gap_src_matches_dense_silence_without_a_filter_phase_jump() {
        let (rate, target) = (44_100, 48_000);
        let first = signal(0, 443);
        let second = signal(4860, 441);
        let mut dense = first.clone(); dense.extend(vec![0.0; 4417]); dense.extend(&second);
        let expected = resampled_with_partition(&dense, &[211, 441]);
        let mut clock = NativePacketClock::new(rate);
        let mut src = PacketResampler::new(rate, target);
        let mut actual = vec![0.0; expected.len()];
        let mut write = |blocks: Vec<(Vec<f32>, AudioCaptureTime)>| {
            for (audio, timing) in blocks {
                let start = (((timing.unix_us - 1_000_000) as f64 * target as f64 / 1_000_000.0).round() as usize).min(actual.len());
                let count = audio.len().min(actual.len() - start);
                actual[start..start + count].copy_from_slice(&audio[..count]);
            }
        };
        for (position, input) in [(0, &first), (4860, &second)] {
            let normalized = clock.timestamp(time(position, rate, 0), Some(position), input.len(), false);
            write(src.process(input, normalized, Some(position), false));
        }
        if let Some(tail) = src.finish_segment() { write(vec![tail]); }
        let error = expected.iter().zip(&actual).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(error < 1e-5, "sparse-gap SRC differs from equivalent dense silence: {error}");
    }

    #[test]
    fn shared_wake_event_closes_only_after_owner_and_worker_release_it() {
        use windows::Win32::Foundation::GetHandleInformation;
        let event = Arc::new(WakeEvent(unsafe { CreateEventW(None, false, false, None) }.unwrap()));
        let handle = event.0;
        let worker = event.clone();
        drop(event);
        let mut flags = 0;
        assert!(unsafe { GetHandleInformation(handle, &mut flags) }.is_ok());
        drop(worker);
        assert!(unsafe { GetHandleInformation(handle, &mut flags) }.is_err());
    }
}

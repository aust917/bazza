//! Bridge a device's monotonic sample clock to the recording's Unix clock.

use cpal::{InputStreamTimestamp, StreamInstant};
use parking_lot::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug)]
pub struct AudioCaptureTime {
    /// Unix microseconds of the first sample, not callback delivery time.
    pub unix_us: i64,
    pub device_timestamp: bool,
}

/// Map native frame positions to one capture origin. Packet QPC snapshots may
/// jitter even when the device delivered consecutive samples; using each QPC
/// value to edit PCM turns that jitter into repeated zero inserts/sample trims.
/// Native positions are stream-relative frame counts, not callback arrival time.
#[cfg(any(windows, test))]
pub(crate) struct NativePacketClock {
    rate: u32,
    origin_unix_us: Option<i64>,
    /// Virtual frame = native position + bias. A bias also attaches the first
    /// reliable counter continuously after packets with invalid timestamps.
    native_bias: Option<i128>,
    next_frame: i128,
    pending_discontinuity: bool,
}

#[cfg(any(windows, test))]
impl NativePacketClock {
    pub(crate) fn new(rate: u32) -> Self {
        Self { rate: rate.max(1), origin_unix_us: None, native_bias: None, next_frame: 0, pending_discontinuity: false }
    }

    /// `position == None` means WASAPI marked the timestamp uncertain. Retain
    /// PCM continuously and mark timing estimated instead of snapping to QPC.
    /// A counter reset with DATA_DISCONTINUITY reanchors at the accepted tail;
    /// valid forward gaps remain gaps, and duplicate/overlapping positions keep
    /// their old time so the recorder can omit already accepted samples.
    pub(crate) fn timestamp(&mut self, observed: AudioCaptureTime, position: Option<u64>, frames: usize, discontinuity: bool) -> AudioCaptureTime {
        let first = self.origin_unix_us.is_none();
        let origin = *self.origin_unix_us.get_or_insert(observed.unix_us);
        let mut reset = false;
        let frame = match position {
            Some(position) => {
                // An uncertain discontinuity invalidates the counter epoch,
                // including resets near zero that still exceed the last known
                // packet start. Reattach the next valid packet at accepted PCM.
                reset = self.pending_discontinuity || (discontinuity && self.native_bias.is_some_and(|bias| position as i128 + bias < self.next_frame));
                if self.native_bias.is_none() || reset {
                    self.native_bias = Some(self.next_frame - position as i128);
                }
                self.pending_discontinuity = false;
                position as i128 + self.native_bias.unwrap()
            }
            None => { self.pending_discontinuity |= discontinuity; self.next_frame },
        };
        self.next_frame = self.next_frame.max(frame + frames as i128);
        let unix_us = (origin as i128 + frame * 1_000_000 / self.rate as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        AudioCaptureTime { unix_us, device_timestamp: position.is_some() && !reset && (!first || observed.device_timestamp) }
    }
}

pub(crate) fn unix_now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros().min(i64::MAX as u128) as i64
}

/// The first output sample can belong to retained input history. Timestamp it
/// by output sample position instead of assigning the next native packet's start.
pub(crate) fn resampled_time(time: AudioCaptureTime, input_frames: u64, output_frames: u64,
    input_rate: u32, output_rate: u32) -> AudioCaptureTime {
    let numerator = output_frames as i128 * input_rate as i128 - input_frames as i128 * output_rate as i128;
    let denominator = input_rate.max(1) as i128 * output_rate.max(1) as i128;
    let delta_us = (numerator * 1_000_000 / denominator).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    AudioCaptureTime { unix_us: time.unix_us.saturating_add(delta_us), ..time }
}

pub(crate) struct CaptureClock {
    anchor: Mutex<Option<(StreamInstant, i64)>>,
    created: Instant,
    created_unix_us: i64,
}

impl CaptureClock {
    pub(crate) fn new() -> Self {
        Self { anchor: Mutex::new(None), created: Instant::now(), created_unix_us: unix_now_us() }
    }

    pub(crate) fn anchor(&self, device_now: StreamInstant, unix_us: i64) {
        *self.anchor.lock() = Some((device_now, unix_us));
    }

    pub(crate) fn timestamp(&self, timestamp: InputStreamTimestamp, frames: usize, rate: u32) -> AudioCaptureTime {
        let fallback = self.estimate(frames, rate);
        // WASAPI's callback instant comes from a separate IAudioClock snapshot.
        // It can be zero at startup or slightly older than a valid GetBuffer
        // capture timestamp. Validate capture against our anchored monotonic
        // clock instead of mistaking that snapshot for callback delivery time.
        if timestamp.capture == StreamInstant::ZERO { return fallback; }
        let Some((instant, unix_us)) = *self.anchor.lock() else { return fallback };
        let delta = signed_us(timestamp.capture, instant);
        let measured = unix_us.saturating_add(delta);
        let now = self.created_unix_us.saturating_add(self.created.elapsed().as_micros().min(i64::MAX as u128) as i64);
        if measured > now.saturating_add(100_000) || now.saturating_sub(measured) > 10_000_000 { return fallback; }
        AudioCaptureTime { unix_us: measured, device_timestamp: true }
    }

    pub(crate) fn estimate(&self, frames: usize, rate: u32) -> AudioCaptureTime {
        let packet_us = (frames as u128 * 1_000_000 / rate.max(1) as u128).min(i64::MAX as u128) as i64;
        let now = self.created_unix_us.saturating_add(self.created.elapsed().as_micros().min(i64::MAX as u128) as i64);
        AudioCaptureTime { unix_us: now.saturating_sub(packet_us), device_timestamp: false }
    }
}

fn signed_us(value: StreamInstant, origin: StreamInstant) -> i64 {
    if let Some(delta) = value.checked_duration_since(origin) {
        delta.as_micros().min(i64::MAX as u128) as i64
    } else {
        -(origin.duration_since(value).as_micros().min(i64::MAX as u128) as i64)
    }
}

#[cfg(windows)]
pub(crate) fn qpc_now() -> Option<StreamInstant> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryPerformanceCounter(counter: *mut i64) -> i32;
        fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    }
    let (mut counter, mut frequency) = (0i64, 0i64);
    if unsafe { QueryPerformanceCounter(&mut counter) } == 0
        || unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 || counter < 0 || frequency <= 0 { return None; }
    let nanos = counter as u128 * 1_000_000_000 / frequency as u128;
    Some(StreamInstant::new((nanos / 1_000_000_000) as u64, (nanos % 1_000_000_000) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(unix_us: i64, device_timestamp: bool) -> AudioCaptureTime {
        AudioCaptureTime { unix_us, device_timestamp }
    }

    #[test]
    fn native_frame_clock_ignores_qpc_jitter_and_preserves_real_gaps_and_duplicates() {
        let mut clock = NativePacketClock::new(48_000);
        assert_eq!(clock.timestamp(observed(1_000_000, true), Some(10_000), 480, true).unix_us, 1_000_000);
        assert_eq!(clock.timestamp(observed(1_011_521, true), Some(10_480), 480, false).unix_us, 1_010_000);
        assert_eq!(clock.timestamp(observed(1_018_479, true), Some(10_960), 480, false).unix_us, 1_020_000);
        // A genuine 10 ms native-position gap survives independently of QPC.
        assert_eq!(clock.timestamp(observed(1_041_521, true), Some(11_920), 480, true).unix_us, 1_040_000);
        // A repeated packet maps to its old time, not a new chunk of the stream.
        assert_eq!(clock.timestamp(observed(1_050_000, true), Some(11_920), 480, false).unix_us, 1_040_000);
        assert_eq!(clock.timestamp(observed(1_060_000, true), Some(12_400), 480, false).unix_us, 1_050_000);
    }

    #[test]
    fn invalid_native_timestamp_preserves_pcm_then_rebases_first_valid_counter() {
        let mut clock = NativePacketClock::new(48_000);
        let first = clock.timestamp(observed(1_000_000, false), None, 480, true);
        assert_eq!(first.unix_us, 1_000_000);
        assert!(!first.device_timestamp);
        let valid = clock.timestamp(observed(1_012_000, true), Some(50_000), 480, false);
        assert_eq!(valid.unix_us, 1_010_000);
        assert!(!valid.device_timestamp, "the first counter after an uncertain discontinuity is a continuous estimated reanchor");
        assert_eq!(clock.timestamp(observed(1_042_000, true), Some(51_440), 480, true).unix_us, 1_040_000);
    }

    #[test]
    fn native_counter_reset_reanchors_without_deleting_pcm_and_reopen_has_its_own_origin() {
        let mut clock = NativePacketClock::new(48_000);
        clock.timestamp(observed(1_000_000, true), Some(10_000), 480, true);
        let reset = clock.timestamp(observed(1_012_000, true), Some(0), 480, true);
        assert_eq!(reset.unix_us, 1_010_000);
        assert!(!reset.device_timestamp);
        assert_eq!(clock.timestamp(observed(1_022_000, true), Some(480), 480, false).unix_us, 1_020_000);
        let mut reopened = NativePacketClock::new(48_000);
        assert_eq!(reopened.timestamp(observed(2_000_000, true), Some(0), 480, true).unix_us, 2_000_000);
        // A flagged epoch change can repeat a positive counter or overlap only
        // part of the previous packet; its PCM must not be silently discarded.
        let mut positive = NativePacketClock::new(48_000);
        positive.timestamp(observed(1_000_000, true), Some(10_000), 480, true);
        let overlap = positive.timestamp(observed(1_010_000, true), Some(10_320), 480, true);
        assert_eq!(overlap.unix_us, 1_010_000);
        assert!(!overlap.device_timestamp);
    }

    #[test]
    fn native_reset_flag_survives_a_packet_with_an_uncertain_timestamp() {
        let mut clock = NativePacketClock::new(48_000);
        clock.timestamp(observed(1_000_000, true), Some(10_000), 480, true);
        assert_eq!(clock.timestamp(observed(1_011_500, false), None, 480, true).unix_us, 1_010_000);
        let resumed = clock.timestamp(observed(1_021_500, true), Some(480), 480, false);
        assert_eq!(resumed.unix_us, 1_020_000);
        assert!(!resumed.device_timestamp);
        assert_eq!(clock.timestamp(observed(1_031_500, true), Some(960), 480, false).unix_us, 1_030_000);
        // A reset can still be above the previous valid packet's start.
        let mut clock = NativePacketClock::new(48_000);
        clock.timestamp(observed(1_000_000, true), Some(0), 480, true);
        clock.timestamp(observed(1_010_000, false), None, 480, true);
        assert_eq!(clock.timestamp(observed(1_020_000, true), Some(480), 480, false).unix_us, 1_020_000);
        assert_eq!(clock.timestamp(observed(1_030_000, true), Some(960), 480, false).unix_us, 1_030_000);
    }

    #[test]
    fn native_clock_uses_rational_positions_at_44100_hz_without_packet_rounding_drift() {
        let mut clock = NativePacketClock::new(44_100);
        let mut position = 123_456u64;
        for size in [257, 443, 511, 128, 3, 997].into_iter().cycle().take(10_000) {
            let time = clock.timestamp(observed(1_000_000, true), Some(position), size, false);
            assert_eq!(time.unix_us, 1_000_000 + ((position - 123_456) * 1_000_000 / 44_100) as i64);
            position += size as u64;
        }
    }

    #[test]
    fn sample_clock_mapping_does_not_include_callback_delivery_delay() {
        let clock = CaptureClock::new();
        let unix_us = unix_now_us();
        clock.anchor(StreamInstant::from_secs_f64(100.0), unix_us);
        let capture = StreamInstant::from_secs_f64(99.975);
        let first = clock.timestamp(InputStreamTimestamp { capture, callback: StreamInstant::from_secs_f64(100.0) }, 480, 48_000);
        let delayed = clock.timestamp(InputStreamTimestamp { capture, callback: StreamInstant::from_secs_f64(100.5) }, 480, 48_000);
        assert!(first.device_timestamp && delayed.device_timestamp);
        assert_eq!(first.unix_us, unix_us - 25_000);
        assert_eq!(first.unix_us, delayed.unix_us);
    }

    #[test]
    fn invalid_device_timestamp_uses_an_explicit_estimate() {
        let clock = CaptureClock::new();
        clock.anchor(StreamInstant::from_secs_f64(100.0), unix_now_us());
        let timing = clock.timestamp(InputStreamTimestamp { capture: StreamInstant::ZERO, callback: StreamInstant::from_secs_f64(100.0) }, 480, 48_000);
        assert!(!timing.device_timestamp);
        assert!((unix_now_us() - timing.unix_us).abs() < 100_000);
    }

    #[test]
    fn valid_capture_survives_a_zero_or_older_callback_clock_snapshot() {
        let clock = CaptureClock::new();
        let unix_us = unix_now_us();
        clock.anchor(StreamInstant::from_secs_f64(100.0), unix_us);
        let capture = StreamInstant::from_secs_f64(99.975);
        for callback in [StreamInstant::ZERO, StreamInstant::from_secs_f64(99.97495)] {
            let timing = clock.timestamp(InputStreamTimestamp { capture, callback }, 480, 48_000);
            assert!(timing.device_timestamp);
            assert_eq!(timing.unix_us, unix_us - 25_000);
        }
    }

    #[test]
    fn unavailable_future_or_old_capture_times_still_use_an_estimate() {
        let clock = CaptureClock::new();
        clock.anchor(StreamInstant::from_secs_f64(100.0), unix_now_us());
        for capture in [StreamInstant::ZERO, StreamInstant::from_secs_f64(100.2), StreamInstant::from_secs_f64(89.0)] {
            let timing = clock.timestamp(InputStreamTimestamp { capture, callback: capture }, 480, 48_000);
            assert!(!timing.device_timestamp, "accepted {capture:?}");
            assert!((unix_now_us() - timing.unix_us).abs() < 100_000);
        }
        // A zero timestamp must also be rejected shortly after system boot,
        // when mapping it would otherwise fall inside the ten-second age bound.
        clock.anchor(StreamInstant::from_secs_f64(0.005), unix_now_us());
        let timing = clock.timestamp(InputStreamTimestamp { capture: StreamInstant::ZERO, callback: StreamInstant::ZERO }, 480, 48_000);
        assert!(!timing.device_timestamp);

        let unanchored = CaptureClock::new();
        let capture = StreamInstant::from_secs_f64(100.0);
        assert!(!unanchored.timestamp(InputStreamTimestamp { capture, callback: capture }, 480, 48_000).device_timestamp);
    }

    #[test]
    fn resampled_timestamp_accounts_for_retained_history_at_low_native_rates() {
        let time = AudioCaptureTime { unix_us: 1_010_000, device_timestamp: true };
        // At 8 kHz, 16 retained input samples are 2 ms, not callback latency.
        let time = resampled_time(time, 80, 384, 8_000, 48_000);
        assert_eq!(time.unix_us, 1_008_000);
        assert!(time.device_timestamp);
    }
}

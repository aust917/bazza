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

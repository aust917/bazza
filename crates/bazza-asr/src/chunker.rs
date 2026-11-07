//! Splits a continuous 16 kHz mono stream into overlapping windows for live ASR.

use std::time::Duration;

pub struct Chunker {
    sample_rate: usize,
    window: usize,
    overlap: usize,
    buf: Vec<f32>,
    /// Absolute sample index of `buf[0]`.
    base: usize,
}

impl Chunker {
    pub fn new(sample_rate: u32, window: Duration, overlap: Duration) -> Self {
        let sr = sample_rate as usize;
        let window = (window.as_secs_f64() * sr as f64) as usize;
        let overlap = ((overlap.as_secs_f64() * sr as f64) as usize).min(window / 2);
        Chunker { sample_rate: sr, window, overlap, buf: Vec::with_capacity(window * 2), base: 0 }
    }

    /// Push audio; returns `(start_seconds, samples)` when a full window is ready.
    pub fn push(&mut self, pcm: &[f32]) -> Option<(f64, Vec<f32>)> {
        self.buf.extend_from_slice(pcm);
        if self.buf.len() < self.window {
            return None;
        }
        let start = self.base as f64 / self.sample_rate as f64;
        let chunk = self.buf[..self.window].to_vec();
        let advance = self.window - self.overlap;
        self.buf.drain(..advance);
        self.base += advance;
        Some((start, chunk))
    }

    /// Whatever is left (>= 0.5 s), e.g. at stop.
    pub fn flush(&mut self) -> Option<(f64, Vec<f32>)> {
        if self.buf.len() < self.sample_rate / 2 {
            self.buf.clear();
            return None;
        }
        let start = self.base as f64 / self.sample_rate as f64;
        let chunk = std::mem::take(&mut self.buf);
        self.base += chunk.len();
        Some((start, chunk))
    }

    /// Seconds of audio consumed so far (start of the next window).
    pub fn position(&self) -> f64 {
        self.base as f64 / self.sample_rate as f64
    }
}

/// One recognition window of a participant track.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechWindow {
    pub start_ms: i64,
    pub samples: Vec<f32>,
    /// Starts inside speech that the previous window had to cut at `max`.
    pub continued: bool,
    /// Ends inside speech; the next window repeats the overlap.
    pub continues: bool,
}

/// Splits a 16 kHz participant track into speech windows.
///
/// Participant tracks are exact (near-)zero where nothing was received, so
/// fixed windows decode long silent stretches. Whisper then stretches the first
/// caption back to the window start or invents text for the silence. Windows
/// here start shortly before speech and end after `gap` of silence. Continuous
/// speech longer than `max` falls back to overlapping fixed windows.
pub struct SpeechWindows {
    rate: usize,
    max: usize,
    overlap: usize,
    gap: usize,
    pad: usize,
    min_voiced: usize,
    level: f32,
    buf: Vec<f32>,
    /// Absolute sample index of `buf[0]`.
    base: usize,
    voiced: Option<(usize, usize)>,
    continued: bool,
}

impl SpeechWindows {
    pub fn new(sample_rate: u32, max: Duration, overlap: Duration, gap: Duration) -> Self {
        let rate = sample_rate as usize;
        let samples = |d: Duration| (d.as_secs_f64() * rate as f64) as usize;
        let max = samples(max).max(rate);
        Self {
            rate,
            max,
            overlap: samples(overlap).min(max / 2),
            gap: samples(gap).max(1),
            pad: rate / 5,
            min_voiced: rate * 3 / 20,
            level: SILENCE_LEVEL,
            buf: Vec::with_capacity(max + rate),
            base: 0,
            voiced: None,
            continued: false,
        }
    }

    fn ms(&self, index: usize) -> i64 {
        ((self.base + index) as u64 * 1000 / self.rate as u64) as i64
    }

    fn emit(&mut self, end: usize, continues: bool, out: &mut Vec<SpeechWindow>) {
        let Some((first, last)) = self.voiced else { return };
        let start = if self.continued { 0 } else { first.saturating_sub(self.pad) };
        if last - first + 1 >= self.min_voiced || self.continued || continues {
            out.push(SpeechWindow {
                start_ms: self.ms(start),
                samples: self.buf[start..end].to_vec(),
                continued: self.continued,
                continues,
            });
        }
    }

    pub fn push(&mut self, pcm: &[f32]) -> Vec<SpeechWindow> {
        let mut out = Vec::new();
        for &sample in pcm {
            self.buf.push(sample);
            let mut index = self.buf.len() - 1;
            if sample.abs() > self.level {
                if self.voiced.is_none() && index > self.pad {
                    // Speech onset: keep exactly `pad` samples of pre-roll.
                    let drop = index - self.pad;
                    self.buf.drain(..drop);
                    self.base += drop;
                    index = self.pad;
                }
                self.voiced = Some(self.voiced.map_or((index, index), |(first, _)| (first, index)));
            }
            match self.voiced {
                None => {
                    // Silence only: keep a short pre-roll, trimming in batches.
                    if self.buf.len() >= self.pad * 4 {
                        let drop = self.buf.len() - self.pad;
                        self.buf.drain(..drop);
                        self.base += drop;
                    }
                }
                Some((_, last)) if index - last >= self.gap => {
                    let end = (last + self.pad + 1).min(self.buf.len());
                    self.emit(end, false, &mut out);
                    let drop = self.buf.len() - self.pad.min(self.buf.len());
                    self.buf.drain(..drop);
                    self.base += drop;
                    self.voiced = None;
                    self.continued = false;
                }
                Some(_) if self.buf.len() >= self.max => {
                    self.emit(self.max, true, &mut out);
                    let advance = self.max - self.overlap;
                    self.buf.drain(..advance);
                    self.base += advance;
                    let first = self.buf.iter().position(|x| x.abs() > self.level);
                    let last = self.buf.iter().rposition(|x| x.abs() > self.level);
                    self.voiced = first.zip(last);
                    self.continued = true;
                }
                Some(_) => {}
            }
        }
        out
    }

    /// Emit the last speech at end of input.
    pub fn flush(&mut self) -> Option<SpeechWindow> {
        let mut out = Vec::new();
        if let Some((_, last)) = self.voiced {
            let end = (last + self.pad + 1).min(self.buf.len());
            self.emit(end, false, &mut out);
        }
        self.base += self.buf.len();
        self.buf.clear();
        self.voiced = None;
        self.continued = false;
        out.pop()
    }
}

/// Below about -84 dBFS after resampling; exact digital silence and filter ringing.
pub const SILENCE_LEVEL: f32 = 2.0 / 32768.0;

/// Trim a caption to the signal it covers in `samples` (which start at
/// `start_ms`). `None` means the caption lies entirely on silence.
pub fn trim_to_signal(samples: &[f32], start_ms: i64, rate: u32, caption: (i64, i64)) -> Option<(i64, i64)> {
    let per_ms = rate as i64 / 1000;
    let index = |ms: i64| ((ms - start_ms).max(0) * per_ms).min(samples.len() as i64) as usize;
    let (from, to) = (index(caption.0), index(caption.1));
    let span = samples.get(from..to)?;
    let first = span.iter().position(|x| x.abs() > SILENCE_LEVEL)?;
    let last = span.iter().rposition(|x| x.abs() > SILENCE_LEVEL)?;
    let ms = |i: usize| start_ms + ((from + i) as i64) / per_ms;
    Some((ms(first), (ms(last) + 1).max(ms(first) + 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ: usize = 16_000;

    fn speech(track: &mut [f32], from_ms: usize, to_ms: usize) {
        for (i, x) in track[from_ms * 16..to_ms * 16].iter_mut().enumerate() {
            *x = 0.1 + ((i as f32) * 0.07).sin() * 0.05;
        }
    }

    fn windows(track: &[f32]) -> Vec<SpeechWindow> {
        let mut splitter = SpeechWindows::new(HZ as u32, Duration::from_secs(30), Duration::from_secs(4), Duration::from_millis(1500));
        let mut out = Vec::new();
        for block in track.chunks(8192) {
            out.extend(splitter.push(block));
        }
        out.extend(splitter.flush());
        out
    }

    #[test]
    fn speech_windows_skip_silence_and_start_just_before_speech() {
        let mut track = vec![0.0; 90 * HZ];
        speech(&mut track, 17_680, 27_924);
        speech(&mut track, 29_000, 31_000); // pause shorter than the gap stays in one window
        speech(&mut track, 60_000, 60_100); // a click is not a window
        speech(&mut track, 70_000, 72_000);
        // Residue of a removed copy below two PCM16 steps is silence, not speech.
        track[80_000 * 16..85_000 * 16].fill(2.0 / 32768.0);
        let found = windows(&track);
        let spans: Vec<_> = found.iter().map(|w| (w.start_ms, w.start_ms + (w.samples.len() * 1000 / HZ) as i64)).collect();
        assert_eq!(spans, vec![(17_480, 31_200), (69_800, 72_200)]);
        assert!(found.iter().all(|w| !w.continued && !w.continues));
    }

    #[test]
    fn continuous_speech_longer_than_the_maximum_uses_overlapping_windows() {
        let mut track = vec![0.0; 80 * HZ];
        speech(&mut track, 5_000, 70_000);
        let found = windows(&track);
        let shape: Vec<_> = found.iter().map(|w| (w.start_ms, w.samples.len() / HZ, w.continued, w.continues)).collect();
        assert_eq!(shape, vec![(4_800, 30, false, true), (30_800, 30, true, true), (56_800, 13, true, false)]);
    }

    #[test]
    fn captions_are_trimmed_to_signal_and_silent_captions_are_rejected() {
        let mut window = vec![0.0; 10 * HZ];
        speech(&mut window, 3_000, 5_000);
        assert_eq!(trim_to_signal(&window, 26_000, HZ as u32, (26_000, 33_000)), Some((29_000, 31_000)));
        assert_eq!(trim_to_signal(&window, 26_000, HZ as u32, (26_000, 28_900)), None);
        assert_eq!(trim_to_signal(&window, 26_000, HZ as u32, (30_000, 40_000)), Some((30_000, 31_000)));
    }

    #[test]
    fn windows_advance_by_window_minus_overlap() {
        let mut c = Chunker::new(16000, Duration::from_secs(6), Duration::from_secs(1));
        assert!(c.push(&vec![0.0; 16000 * 5]).is_none());
        let (s, chunk) = c.push(&vec![0.0; 16000 * 2]).unwrap(); // 7 s total
        assert_eq!(s, 0.0);
        assert_eq!(chunk.len(), 16000 * 6);
        // 7 s - 5 s advance = 2 s buffered; need 4 more.
        assert!(c.push(&vec![0.0; 16000 * 3]).is_none());
        let (s2, _) = c.push(&vec![0.0; 16000]).unwrap();
        assert_eq!(s2, 5.0);
    }

    #[test]
    fn flush_returns_remainder_only_if_long_enough() {
        let mut c = Chunker::new(16000, Duration::from_secs(6), Duration::from_secs(1));
        c.push(&vec![0.0; 4000]); // 0.25 s
        assert!(c.flush().is_none());
        c.push(&vec![0.0; 16000]); // 1 s
        let (s, chunk) = c.flush().unwrap();
        assert_eq!(s, 0.0);
        assert_eq!(chunk.len(), 16000);
        assert_eq!(c.position(), 1.0);
    }
}

//! Corroborate a speech offset using waveform matches in disjoint sections.

use realfft::{num_complex::Complex32, RealFftPlanner};

const ALIGN_HZ: u32 = 8_000;
const MIN_SCORE: f64 = 0.35;
const MIN_DISTINCT_GAP: f64 = 0.10;
const MIN_VARIANCE: f64 = (20.0 / 32768.0) * (20.0 / 32768.0);

/// Locate a normalized mono reference in a bounded local-mix window.
///
/// Both inputs must be sampled at 8 kHz, using the same sample time convention.
/// The reference is 0.6 to 5 seconds, with up to 8 seconds of extra search
/// audio in `observed`. The result is an offset in `observed` samples.
/// Place the middle third of the reference inside the speech being located.
pub fn waveform_offset(reference: &[f32], observed: &[f32], hz: u32) -> Option<usize> {
    WaveformMatcher::default().offset(reference, observed, hz)
}

/// Reuse FFT plans and bounded scratch buffers across a meeting's captions.
#[derive(Default)]
pub struct WaveformMatcher {
    planner: RealFftPlanner<f32>,
    padded: Vec<f32>,
    observed_fft: Vec<Complex32>,
    section_fft: Vec<Complex32>,
    correlation: Vec<f32>,
    forward_scratch: Vec<Complex32>,
    inverse_scratch: Vec<Complex32>,
    cumulative: Vec<(f64, f64)>,
    scores: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformMatch {
    Located(usize),
    Missing,
    Ambiguous,
}

impl WaveformMatcher {
    /// Match using the same evidence and bounds as [`waveform_offset`].
    pub fn offset(&mut self, reference: &[f32], observed: &[f32], hz: u32) -> Option<usize> {
        let mut ambiguous = false;
        self.offset_inner(reference, observed, hz, false, &mut ambiguous)
    }

    /// Expose strong competing evidence when comparing separate search windows.
    /// A search with such evidence cannot certify a match from another window.
    pub fn search(&mut self, reference: &[f32], observed: &[f32], hz: u32) -> WaveformMatch {
        self.search_with_prefix(reference, observed, hz, false)
    }

    /// Verify the prefix and middle near an already established media lag.
    pub fn search_near_onset(&mut self, reference: &[f32], observed: &[f32], hz: u32) -> WaveformMatch {
        self.search_with_prefix(reference, observed, hz, true)
    }

    fn search_with_prefix(&mut self, reference: &[f32], observed: &[f32], hz: u32, require_prefix: bool) -> WaveformMatch {
        let mut ambiguous = false;
        let offset = self.offset_inner(reference, observed, hz, require_prefix, &mut ambiguous);
        match (ambiguous, offset) {
            (true, _) => WaveformMatch::Ambiguous,
            (false, Some(offset)) => WaveformMatch::Located(offset),
            (false, None) => WaveformMatch::Missing,
        }
    }

    fn offset_inner(
        &mut self,
        reference: &[f32],
        observed: &[f32],
        hz: u32,
        require_prefix: bool,
        ambiguous: &mut bool,
    ) -> Option<usize> {
        if hz != ALIGN_HZ {
            return None;
        }
        let rate = hz as usize;
        let length = reference.len();
        let search_len = observed.len().checked_sub(length)?;
        if !(rate * 3 / 5..=rate * 5).contains(&length)
            || search_len > rate * 8 + rate / 100
            || !reference
                .iter()
                .chain(observed)
                .all(|sample| sample.is_finite())
        {
            return None;
        }
        // Different input rates can leave a few different trailing samples
        // after resampling. Keep the accepted lag search within eight seconds.
        let search_len = search_len.min(rate * 8);
        let observed = &observed[..length + search_len];

        let boundaries = [0, (length + 1) / 3, (2 * length + 1) / 3, length];
        let fft_len = (observed.len() + length.div_ceil(3) - 1).next_power_of_two();
        let Self {
            planner,
            padded,
            observed_fft,
            section_fft,
            correlation,
            forward_scratch,
            inverse_scratch,
            cumulative,
            scores,
        } = self;
        let forward = planner.plan_fft_forward(fft_len);
        let inverse = planner.plan_fft_inverse(fft_len);
        padded.resize(fft_len, 0.0);
        padded.fill(0.0);
        observed_fft.resize(forward.complex_len(), Complex32::default());
        section_fft.resize(forward.complex_len(), Complex32::default());
        correlation.resize(fft_len, 0.0);
        forward_scratch.resize(forward.get_scratch_len(), Complex32::default());
        inverse_scratch.resize(inverse.get_scratch_len(), Complex32::default());

        padded[..observed.len()].copy_from_slice(observed);
        forward
            .process_with_scratch(padded, observed_fft, forward_scratch)
            .ok()?;

        cumulative.clear();
        cumulative.reserve(observed.len() + 1);
        let (mut sum, mut sum_sq) = (0.0, 0.0);
        cumulative.push((sum, sum_sq));
        for &sample in observed {
            let value = f64::from(sample);
            sum += value;
            sum_sq += value * value;
            cumulative.push((sum, sum_sq));
        }

        scores.resize(search_len + 1, 0.0);
        let mut matches = Vec::with_capacity(3);
        let distinct = if require_prefix { rate / 50 } else { rate / 10 };
        let mut prefix_matched = false;
        let mut middle_matched = false;
        for (section_index, limits) in boundaries.windows(2).enumerate() {
            let (start, end) = (limits[0], limits[1]);
            let section = &reference[start..end];
            let count = section.len() as f64;
            let mean = section.iter().map(|&sample| f64::from(sample)).sum::<f64>() / count;
            let mut energy = 0.0;
            padded.fill(0.0);
            for (slot, &sample) in padded.iter_mut().zip(section.iter().rev()) {
                let centered = f64::from(sample) - mean;
                *slot = centered as f32;
                energy += centered * centered;
            }
            if energy / count < MIN_VARIANCE {
                continue;
            }
            // Convolution with the reversed section gives the raw cross-correlation
            // at `offset + end - 1`. The FFT can be shared by all three sections.
            forward
                .process_with_scratch(padded, section_fft, forward_scratch)
                .ok()?;
            for (value, &observed) in section_fft.iter_mut().zip(observed_fft.iter()) {
                *value *= observed;
            }
            inverse
                .process_with_scratch(section_fft, correlation, inverse_scratch)
                .ok()?;

            for (offset, score) in scores.iter_mut().enumerate() {
                let (left_sum, left_energy) = cumulative[offset + start];
                let (right_sum, right_energy) = cumulative[offset + end];
                let sum = right_sum - left_sum;
                let variance = right_energy - left_energy - sum * sum / count;
                *score = if variance / count > MIN_VARIANCE {
                    let dot = f64::from(correlation[offset + end - 1]) / fft_len as f64;
                    dot.abs() / (energy * variance).sqrt()
                } else {
                    0.0
                };
            }
            let (peak_at, &peak) = scores
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))?;
            let competitor = scores
                .iter()
                .enumerate()
                .filter(|(offset, _)| offset.abs_diff(peak_at) > distinct)
                .map(|(_, &score)| score)
                .fold(0.0, f64::max);
            if peak >= MIN_SCORE && peak - competitor >= MIN_DISTINCT_GAP {
                matches.push((peak_at, peak));
                prefix_matched |= section_index == 0;
                middle_matched |= section_index == 1;
            } else if peak >= MIN_SCORE && competitor >= MIN_SCORE {
                *ambiguous = true;
            }
        }

        // Two temporally disjoint sections must agree. If a third strong section
        // disagrees, the reference may cross a discontinuity or a repeated phrase.
        if matches.len() < 2 || !middle_matched || (require_prefix && !prefix_matched) {
            return None;
        }
        let first = matches.iter().map(|&(offset, _)| offset).min()?;
        let last = matches.iter().map(|&(offset, _)| offset).max()?;
        if last - first > rate / 50 {
            *ambiguous = true;
            return None;
        }
        matches
            .into_iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(offset, _)| offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(seed: u32, len: usize, gain: f32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                ((state as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32) * gain
            })
            .collect()
    }

    #[test]
    fn nearby_onset_verification_refuses_two_strong_copies() {
        let source = signal(89, 9_600, 0.3);
        let mut observed = vec![0.0; source.len() + 960];
        let mut matcher = WaveformMatcher::default();
        observed[120..120 + source.len()].copy_from_slice(&source);
        assert_eq!(matcher.search_near_onset(&source, &observed, ALIGN_HZ), WaveformMatch::Located(120));
        for (mixed, sample) in observed[600..].iter_mut().zip(&source) {
            *mixed += sample;
        }
        assert_eq!(matcher.search_near_onset(&source, &observed, ALIGN_HZ), WaveformMatch::Ambiguous);
    }

    #[test]
    fn locates_a_voice_under_stronger_overlap() {
        let source = signal(81, 12_000, 0.2);
        let mut mix = signal(43, 76_005, 0.4);
        for (output, sample) in mix[5_731..].iter_mut().zip(&source) {
            *output += sample;
        }
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), Some(5_731));
    }

    #[test]
    fn reusing_buffers_does_not_carry_a_match_into_the_next_window() {
        let source = signal(81, 12_000, 0.2);
        let mut matcher = WaveformMatcher::default();
        for (length, offset) in [(76_005, 5_731), (30_000, 1_281), (70_000, 47_031)] {
            let mut mix = signal(43, length, 0.4);
            for (output, sample) in mix[offset..].iter_mut().zip(&source) {
                *output += sample;
            }
            assert_eq!(matcher.offset(&source, &mix, ALIGN_HZ), Some(offset));
            mix.fill(0.0);
            assert_eq!(matcher.offset(&source, &mix, ALIGN_HZ), None);
        }
    }

    #[test]
    fn requires_two_distinct_pieces_of_evidence() {
        let source = signal(81, 12_000, 0.2);
        let mut mix = signal(43, 31_000, 0.4);
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), None);
        for (output, sample) in mix[5_731..].iter_mut().zip(&source[..4_000]) {
            *output += sample;
        }
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), None);
        for (output, sample) in mix[9_731..].iter_mut().zip(&source[4_000..8_000]) {
            *output += sample;
        }
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), Some(5_731));
    }

    #[test]
    fn does_not_map_missing_speech_using_context_alone() {
        let source = signal(81, 12_000, 0.2);
        let mut mix = vec![0.0; 30_000];
        mix[5_731..9_731].copy_from_slice(&source[..4_000]);
        mix[13_731..17_731].copy_from_slice(&source[8_000..]);
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), None);

        mix[9_731..13_731].copy_from_slice(&source[4_000..8_000]);
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), Some(5_731));
    }

    #[test]
    fn rejects_repeats_and_discontinuous_alignment() {
        let source = signal(81, 12_000, 0.2);
        let mut mix = vec![0.0; 41_000];
        mix[1_000..13_000].copy_from_slice(&source);
        mix[22_000..34_000].copy_from_slice(&source);
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), None);

        mix.fill(0.0);
        mix[1_000..5_000].copy_from_slice(&source[..4_000]);
        mix[5_400..9_400].copy_from_slice(&source[4_000..8_000]);
        mix[9_800..13_800].copy_from_slice(&source[8_000..]);
        assert_eq!(waveform_offset(&source, &mix, ALIGN_HZ), None);
    }
}

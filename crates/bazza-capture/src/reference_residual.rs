//! Conservative subtraction of a known waveform from an aligned analysis copy.
//!
//! This is a bounded, constant-gain cancellation experiment, not an acoustic
//! echo canceller or a speaker separator. Callers must first establish that a
//! reference is a unique occurrence in the same recording interval. In
//! particular, a speaker ID or a similar transcript is not sufficient evidence.
//! Uncertain windows are left byte-for-byte unchanged. Original archive audio
//! must never be passed as the mutable analysis buffer.

const MIN_CORRELATION: f64 = 0.65;
const MIN_BLOCK_CORRELATION: f64 = 0.45;
const MIN_RMS: f64 = 20.0 / 32768.0;
const BLOCKS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalRejection {
    InvalidInput,
    Clipped,
    Silent,
    Unrelated,
    AmbiguousDelay,
    InconsistentGain,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemovalEvidence {
    Removed {
        /// Positive means reference[i] was removed from observed[i + delay].
        delay_samples: i32,
        gain: f32,
        correlation: f32,
        supporting_blocks: usize,
    },
    Unchanged(RemovalRejection),
}

impl RemovalEvidence {
    pub fn removed(self) -> bool {
        matches!(self, Self::Removed { .. })
    }
}

#[derive(Clone, Copy)]
struct Fit {
    gain: f64,
    correlation: f64,
    reference_rms: f64,
    observed_rms: f64,
}

fn fit(reference: &[f32], observed: &[f32], stride: usize) -> Fit {
    let (mut count, mut x, mut y, mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (&a, &b) in reference.iter().zip(observed).step_by(stride) {
        let (a, b) = (f64::from(a), f64::from(b));
        count += 1.0;
        x += a;
        y += b;
        xx += a * a;
        yy += b * b;
        xy += a * b;
    }
    let vx = (xx - x * x / count).max(0.0);
    let vy = (yy - y * y / count).max(0.0);
    let covariance = xy - x * y / count;
    Fit {
        gain: if vx > 0.0 { covariance / vx } else { 0.0 },
        correlation: if vx * vy > 0.0 {
            covariance.abs() / (vx * vy).sqrt()
        } else {
            0.0
        },
        reference_rms: (vx / count).sqrt(),
        observed_rms: (vy / count).sqrt(),
    }
}

fn aligned<'a>(reference: &'a [f32], observed: &'a [f32], delay: i32) -> (&'a [f32], &'a [f32]) {
    if delay >= 0 {
        let delay = delay as usize;
        (&reference[..reference.len() - delay], &observed[delay..])
    } else {
        let delay = (-delay) as usize;
        (&reference[delay..], &observed[..observed.len() - delay])
    }
}

/// Remove a verified reference from equal-length, coarsely aligned mono windows.
///
/// Accepts 0.6 to 5 seconds at 8--48 kHz. A fine search allows up to +/-5 ms of
/// remaining delay. A constant gain must agree in at least four disjoint blocks;
/// changing gain, competing delays, clipping and weak evidence cause a no-op.
/// This deliberately leaves difficult codec/filter/dynamic-gain cases unresolved.
///
/// Independent overlapping speech is not muted: only a fitted, time-aligned copy
/// of the supplied samples is subtracted. A component correlated with the exact
/// reference cannot be distinguished from that reference by this algorithm.
pub fn remove_reference(observed: &mut [f32], reference: &[f32], hz: u32) -> RemovalEvidence {
    use RemovalEvidence::Unchanged;
    use RemovalRejection::*;

    let rate = hz as usize;
    if !(8_000..=48_000).contains(&hz)
        || observed.len() != reference.len()
        || !(rate * 3 / 5..=rate * 5).contains(&reference.len())
        || !reference
            .iter()
            .chain(observed.iter())
            .all(|x| x.is_finite())
    {
        return Unchanged(InvalidInput);
    }
    if reference
        .iter()
        .chain(observed.iter())
        .any(|x| x.abs() >= 0.999)
    {
        return Unchanged(Clipped);
    }
    let initial = fit(reference, observed, 1);
    if initial.reference_rms < MIN_RMS || initial.observed_rms < MIN_RMS {
        return Unchanged(Silent);
    }

    // Search at no more than 8 kHz; refine the best small neighborhood at the
    // original sample rate. The reference must already have a unique coarse
    // match: this local search cannot disambiguate repeated phrases seconds apart.
    let limit = (rate / 200) as i32;
    let lag_step = (rate / 8_000).max(1) as i32;
    let sample_step = (rate / 2_000).max(1);
    let mut candidates = Vec::with_capacity((2 * limit / lag_step + 1) as usize);
    let mut delay = -limit;
    while delay <= limit {
        let (a, b) = aligned(reference, observed, delay);
        candidates.push((delay, fit(a, b, sample_step).correlation));
        delay += lag_step;
    }
    let &(coarse, _) = candidates
        .iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .expect("bounded nonempty lag search");
    let mut best_delay = coarse;
    let mut best = fit(
        aligned(reference, observed, coarse).0,
        aligned(reference, observed, coarse).1,
        1,
    );
    for delay in (coarse - lag_step).max(-limit)..=(coarse + lag_step).min(limit) {
        let (a, b) = aligned(reference, observed, delay);
        let candidate = fit(a, b, 1);
        if candidate.correlation > best.correlation {
            best_delay = delay;
            best = candidate;
        }
    }
    if best.correlation < MIN_CORRELATION {
        return Unchanged(Unrelated);
    }
    // Neighboring samples share a correlation lobe. A separate peak at least
    // two milliseconds away must be clearly weaker, otherwise phase is unsafe.
    let competitor = candidates
        .iter()
        .filter(|(delay, _)| delay.abs_diff(best_delay) > (rate / 500) as u32)
        .map(|&(_, score)| score)
        .fold(0.0, f64::max);
    if competitor >= MIN_CORRELATION && best.correlation - competitor < 0.08 {
        return Unchanged(AmbiguousDelay);
    }

    let (a, b) = aligned(reference, observed, best_delay);
    let mut gains = Vec::with_capacity(BLOCKS);
    for block in 0..BLOCKS {
        let start = a.len() * block / BLOCKS;
        let end = a.len() * (block + 1) / BLOCKS;
        let part = fit(&a[start..end], &b[start..end], 1);
        if part.reference_rms < MIN_RMS {
            continue;
        }
        if part.correlation < MIN_BLOCK_CORRELATION || part.gain.signum() != best.gain.signum() {
            return Unchanged(InconsistentGain);
        }
        gains.push(part.gain);
    }
    if gains.len() < 4 {
        return Unchanged(InconsistentGain);
    }
    gains.sort_by(f64::total_cmp);
    let median = (gains[(gains.len() - 1) / 2] + gains[gains.len() / 2]) / 2.0;
    if !(0.02..=4.0).contains(&median.abs())
        || gains.last().unwrap() - gains.first().unwrap() > (median.abs() * 0.25).max(0.01)
    {
        return Unchanged(InconsistentGain);
    }
    // Use the smaller magnitude of the robust block estimate and whole-window
    // fit. This favors a little remaining known signal over an aggressive fit to
    // a coincident component of the desired speech. Do not normalize the result.
    let gain = best.gain.signum() * best.gain.abs().min(median.abs());
    let (reference_start, observed_start) = if best_delay >= 0 {
        (0, best_delay as usize)
    } else {
        ((-best_delay) as usize, 0)
    };
    for (output, &input) in observed[observed_start..]
        .iter_mut()
        .zip(&reference[reference_start..])
    {
        *output = (f64::from(*output) - gain * f64::from(input)) as f32;
    }
    RemovalEvidence::Removed {
        delay_samples: best_delay,
        gain: gain as f32,
        correlation: best.correlation as f32,
        supporting_blocks: gains.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HZ: u32 = 16_000;
    const LEN: usize = HZ as usize;

    // Different deterministic, band-limited, changing speech-like waveforms.
    // They are not real speech or a codec-quality acceptance corpus.
    fn voice(seed: u32, gain: f32) -> Vec<f32> {
        let mut random = seed;
        let mut low = 0.0;
        (0..LEN)
            .map(|i| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                let sample = random as f64 / u32::MAX as f64 * 2.0 - 1.0;
                low = low * 0.65 + sample * 0.35;
                let envelope = 0.4 + 0.6 * ((i as f64 * 0.0017 + seed as f64).sin()).abs();
                (low * envelope) as f32 * gain
            })
            .collect()
    }

    fn mse(actual: &[f32], desired: &[f32]) -> f64 {
        actual
            .iter()
            .zip(desired)
            .map(|(&a, &b)| f64::from(a - b).powi(2))
            .sum::<f64>()
            / actual.len() as f64
    }

    fn assert_noop(mut observed: Vec<f32>, reference: &[f32], reason: RemovalRejection) {
        let before: Vec<_> = observed.iter().map(|x| x.to_bits()).collect();
        assert_eq!(
            remove_reference(&mut observed, reference, HZ),
            RemovalEvidence::Unchanged(reason)
        );
        assert_eq!(
            observed.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn participant_only_is_removed_without_touching_dc_or_amplifying_output() {
        let reference = voice(42, 0.5);
        let mut observed: Vec<_> = reference.iter().map(|x| 0.7 * x + 0.002).collect();
        let desired = vec![0.002; LEN];
        let baseline = mse(&observed, &desired);
        assert!(remove_reference(&mut observed, &reference, HZ).removed());
        assert!(mse(&observed, &desired) < baseline * 0.0001);
    }

    #[test]
    fn independent_quiet_speech_is_preserved_while_known_speech_is_removed() {
        let reference = voice(42, 0.6);
        let desired = voice(91, 0.09);
        let mut observed: Vec<_> = reference
            .iter()
            .zip(&desired)
            .map(|(a, b)| 0.8 * a + b)
            .collect();
        let baseline = mse(&observed, &desired);
        assert!(remove_reference(&mut observed, &reference, HZ).removed());
        let damage = mse(&observed, &desired);
        let desired_power = mse(&desired, &vec![0.0; LEN]);
        eprintln!(
            "quiet overlap: desired-waveform error before={baseline:.9}, after={damage:.9}, desired power={desired_power:.9}"
        );
        assert!(
            damage < baseline * 0.001,
            "must improve desired waveform, not only lower energy"
        );
        assert!(
            damage < desired_power * 0.02,
            "desired-source distortion exceeds two percent"
        );
        assert!(
            mse(&vec![0.0; LEN], &desired) > damage * 20.0,
            "muting is not a successful residual"
        );
    }

    #[test]
    fn two_independent_references_can_be_removed_sequentially() {
        let first = voice(42, 0.4);
        let second = voice(91, 0.4);
        let desired = voice(57, 0.05);
        let mut observed: Vec<_> = first
            .iter()
            .zip(&second)
            .zip(&desired)
            .map(|((a, b), c)| a + b + c)
            .collect();
        let baseline = mse(&observed, &desired);
        assert!(remove_reference(&mut observed, &first, HZ).removed());
        assert!(remove_reference(&mut observed, &second, HZ).removed());
        assert!(mse(&observed, &desired) < baseline * 0.002);
        assert!(mse(&observed, &desired) < mse(&desired, &vec![0.0; LEN]) * 0.1);
    }

    #[test]
    fn bounded_delay_and_negative_gain_are_fitted_for_each_window() {
        let reference = voice(42, 0.5);
        for (delay, gain) in [(37, 0.3), (-43, -0.9), (0, 1.2)] {
            let mut observed = vec![0.0; LEN];
            for (i, x) in reference.iter().enumerate() {
                let target = i as i32 + delay;
                if (0..LEN as i32).contains(&target) {
                    observed[target as usize] = gain * x;
                }
            }
            let baseline = mse(&observed, &vec![0.0; LEN]);
            let evidence = remove_reference(&mut observed, &reference, HZ);
            assert!(
                matches!(evidence, RemovalEvidence::Removed { delay_samples, .. } if delay_samples == delay),
                "{evidence:?}"
            );
            assert!(mse(&observed, &vec![0.0; LEN]) < baseline * 0.0001);
        }
    }

    #[test]
    fn changing_gain_inside_window_is_not_cancelled_with_an_unsafe_average() {
        let reference = voice(42, 0.4);
        let observed = reference
            .iter()
            .enumerate()
            .map(|(i, x)| x * if i < LEN / 2 { 0.4 } else { 1.0 })
            .collect();
        assert_noop(observed, &reference, RemovalRejection::InconsistentGain);
    }

    #[test]
    fn periodic_phase_ambiguity_is_a_noop() {
        let reference: Vec<_> = (0..LEN)
            .map(|i| ((i as f32 * std::f32::consts::TAU * 250.0 / HZ as f32).sin()) * 0.2)
            .collect();
        assert_noop(
            reference.clone(),
            &reference,
            RemovalRejection::AmbiguousDelay,
        );
    }

    #[test]
    fn two_nearby_copies_cannot_be_disambiguated_by_a_speaker_or_waveform_id() {
        let reference = voice(42, 0.4);
        let mut observed = vec![0.0; LEN];
        for delay in [-48, 48] {
            for (i, x) in reference.iter().enumerate() {
                let target = i as i32 + delay;
                if (0..LEN as i32).contains(&target) {
                    observed[target as usize] += x;
                }
            }
        }
        assert_noop(observed, &reference, RemovalRejection::AmbiguousDelay);
    }

    #[test]
    fn silence_unrelated_and_clipped_windows_are_unchanged() {
        let reference = voice(42, 0.4);
        assert_noop(vec![0.0; LEN], &reference, RemovalRejection::Silent);
        assert_noop(reference.clone(), &vec![0.0; LEN], RemovalRejection::Silent);
        assert_noop(voice(91, 0.4), &reference, RemovalRejection::Unrelated);
        let mut clipped = reference.clone();
        clipped[17] = 1.0;
        assert_noop(clipped, &reference, RemovalRejection::Clipped);
    }

    #[test]
    fn malformed_input_and_nonfinite_samples_do_not_modify_output() {
        let reference = voice(42, 0.4);
        assert_noop(
            vec![0.1; LEN - 1],
            &reference,
            RemovalRejection::InvalidInput,
        );
        let mut malformed = reference.clone();
        malformed[17] = f32::NAN;
        assert_noop(malformed, &reference, RemovalRejection::InvalidInput);
    }
}

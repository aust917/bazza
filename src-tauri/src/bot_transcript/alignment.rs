//! Locate the same speech in the local mix before enabling a player seek.

use std::{fs::File, io::BufReader};

use bazza_capture::{
    dsp::Resampler,
    recorder::AudioTiming,
    speech_alignment::{WaveformMatch, WaveformMatcher},
};
use bazza_discord::tracks::{CaptureTimeline, TICK_MS};
use bazza_media_gate::{MediaReadGuard, PlaybackMediaKind};
use hound::{SampleFormat, WavReader};

use super::{read_json, BotSegment};

const ALIGN_HZ: u32 = 8_000;
const CONTEXT_MS: i64 = 1_100;
const SEARCH_MS: i64 = 4_000;
const LAG_CLUSTER_MS: u64 = 250;
const MATCH_AGREEMENT_MS: u64 = 20;
const REFERENCE_MAX_MS: i64 = 5_000;
const MIN_TRIM_SILENCE_MS: i64 = 160;
const MAX_TRIM_SILENCE_MS: i64 = 3_000;
const ONSET_PRE_ROLL_MS: i64 = 25;
const ONSET_REFERENCE_MS: i64 = 1_200;
const ONSET_SEARCH_MS: i64 = 60;
type Wave = WavReader<BufReader<File>>;

pub(super) enum Anchors {
    Meet(Option<u64>),
    Discord(Option<CaptureTimeline>),
}

impl Anchors {
    fn in_same_discord_span(&self, from_ms: i64, to_ms: i64) -> bool {
        let Self::Discord(Some(clock)) = self else { return false; };
        let (Ok(from), Ok(to)) = (u64::try_from(from_ms), u64::try_from(to_ms)) else { return false; };
        from <= to && !clock.pause_seen && !clock.truncated && clock.spans.iter().any(|span| {
            span.start_tick <= from / TICK_MS && to / TICK_MS < span.end_tick
        })
    }

    fn permits_media_lag(&self) -> bool {
        matches!(self, Self::Discord(Some(clock)) if !clock.pause_seen && clock.spans.len() > 1)
    }

    fn hint_ms(&self, source_ms: i64, mix_origin_us: i64) -> Option<i64> {
        let (origin_us, relative_us) = match self {
            Self::Meet(Some(origin_ms)) => (origin_ms.checked_mul(1000)?, source_ms * 1000),
            Self::Discord(Some(clock)) if !clock.pause_seen => {
                let tick = u64::try_from(source_ms).ok()? / TICK_MS;
                let span = clock
                    .spans
                    .iter()
                    .find(|span| (span.start_tick..span.end_tick).contains(&tick))?;
                let elapsed_us = i64::try_from(span.first_elapsed_us).ok()?;
                let source_span_start_ms =
                    i64::try_from(span.start_tick.checked_mul(TICK_MS)?).ok()?;
                (
                    clock.attempt_started_unix_us?,
                    elapsed_us.checked_add((source_ms - source_span_start_ms) * 1000)?,
                )
            }
            _ => return None,
        };
        i64::try_from(origin_us)
            .ok()?
            .checked_sub(mix_origin_us)?
            .checked_add(relative_us)
            .map(|us| us / 1000)
    }
}

#[derive(Default)]
struct MediaLag {
    values: [i64; 5],
    written: usize,
    last_source_ms: Option<i64>,
}

impl MediaLag {
    fn remember(&mut self, source_ms: i64, lag_ms: i64) {
        // Adjacent rows can use the same reference audio. Require distinct
        // source regions before treating repeated lags as independent evidence.
        if self.last_source_ms.is_some_and(|last| last.abs_diff(source_ms) < REFERENCE_MAX_MS as u64) {
            return;
        }
        let index = self.written % self.values.len();
        self.values[index] = lag_ms;
        self.written += 1;
        self.last_source_ms = Some(source_ms);
    }

    fn corroborated(&self) -> Option<i64> {
        let mut values = self.values;
        let count = self.written.min(values.len());
        let recent = &mut values[..count];
        recent.sort_unstable();
        let median = *recent.get(count / 2)?;
        (recent.iter().filter(|lag| lag.abs_diff(median) <= LAG_CLUSTER_MS).count() >= 3)
            .then_some(median)
    }
}

pub(super) struct Playback {
    mix: Wave,
    origin_us: Option<i64>,
    duration_ms: i64,
    matcher: WaveformMatcher,
    media_lag: MediaLag,
}

impl Playback {
    pub(super) fn open(guard: &MediaReadGuard) -> Option<Self> {
        let mix = WavReader::new(BufReader::new(
            guard.open_playback_file(PlaybackMediaKind::Audio).ok()?,
        ))
        .ok()?;
        let spec = mix.spec();
        if spec.sample_format != SampleFormat::Int
            || spec.bits_per_sample != 16
            || !matches!(spec.channels, 1 | 2)
            || !matches!(spec.sample_rate, 16_000 | 44_100 | 48_000)
        {
            return None;
        }
        let duration_ms = i64::from(mix.duration()) * 1000 / i64::from(spec.sample_rate);
        let origin_us = guard
            .open_audio_clock()
            .ok()
            .and_then(|file| read_json::<AudioTiming>(file, 2048).ok())
            .filter(|clock| clock.version == 1 && clock.start_unix_us > 0)
            .map(|clock| clock.start_unix_us);
        Some(Self {
            mix,
            origin_us,
            duration_ms,
            matcher: WaveformMatcher::default(),
            media_lag: MediaLag::default(),
        })
    }

    pub(super) fn align(&mut self, track: &mut Wave, anchors: &Anchors, segment: &mut BotSegment) {
        let track_duration_ms = i64::from(track.duration()) * 1000 / i64::from(track.spec().sample_rate);
        let segment_duration_ms = segment.end_ms - segment.start_ms;
        // Keep the center third inside the requested speech, including near a track edge.
        let padding = CONTEXT_MS
            .min(segment_duration_ms - 30)
            .min(segment.start_ms)
            .min(track_duration_ms - segment.end_ms)
            .max(0);
        let context_start = segment.start_ms - padding;
        let context_end = (segment.end_ms + padding).min(context_start + REFERENCE_MAX_MS);
        let Some(reference) = pcm_window(track, context_start, context_end) else {
            return;
        };
        let clock_hint = self
            .origin_us
            .and_then(|origin| anchors.hint_ms(segment.start_ms, origin))
            .filter(|hint| (-SEARCH_MS..=self.duration_ms + SEARCH_MS).contains(hint));
        let hint = clock_hint.unwrap_or(segment.start_ms);
        let media_hint = clock_hint
            .filter(|_| anchors.permits_media_lag())
            .and_then(|_| self.media_lag.corroborated())
            .map(|lag| segment.start_ms + lag)
            .filter(|media| media.abs_diff(hint) > SEARCH_MS as u64);

        let row_offset = segment.start_ms - context_start;
        let context_duration = context_end - context_start;
        let primary = self.find_start(&reference, hint, row_offset, context_duration, media_hint.is_some());
        let start = if let Some(media) = media_hint {
            // A Discord wall-clock span can jump even when both WAV timelines
            // stay continuous. Prior audio only proposes another bounded window.
            let alternate = self.find_start(&reference, media, row_offset, context_duration, true);
            match (primary, alternate) {
                (Ok(Some(a)), Ok(Some(b))) if a.abs_diff(b) <= MATCH_AGREEMENT_MS => Some(a),
                (Ok(None), Ok(found)) | (Ok(found), Ok(None)) => found,
                _ => None,
            }
        } else {
            primary.ok().flatten()
        };
        let Some(start) = start else { return; };
        let end = start + segment_duration_ms;
        if start >= 0 && end <= self.duration_ms {
            segment.playback_start_ms = Some(
                self.trim_verified_silence(track, anchors, segment, start).unwrap_or(start)
            );
            segment.playback_end_ms = Some(end);
            if anchors.permits_media_lag() {
                self.media_lag.remember(segment.start_ms, start - segment.start_ms);
            }
        }
    }

    fn trim_verified_silence(
        &mut self,
        track: &mut Wave,
        anchors: &Anchors,
        segment: &BotSegment,
        mapped_start: i64,
    ) -> Option<i64> {
        if !anchors.in_same_discord_span(segment.start_ms, segment.start_ms) {
            return None;
        }
        let source_limit = segment.end_ms.min(segment.start_ms + MAX_TRIM_SILENCE_MS);
        let onset = first_signal_or_end_ms(track, segment.start_ms, source_limit)?;
        if onset >= source_limit || onset - segment.start_ms < MIN_TRIM_SILENCE_MS {
            return None;
        }
        let source = onset - ONSET_PRE_ROLL_MS;
        let end = segment.end_ms.min(source + ONSET_REFERENCE_MS);
        if !anchors.in_same_discord_span(segment.start_ms, end - 1) {
            return None;
        }

        let target = mapped_start + (source - segment.start_ms);
        // A bot can miss speech that is present in the recording. Verify the
        // skipped range is silent in the mix as well, across all channels.
        if first_signal_or_end_ms(&mut self.mix, mapped_start, target)? != target {
            return None;
        }
        let search_start = (target - ONSET_SEARCH_MS).max(0);
        let search_end = (target + (end - source) + ONSET_SEARCH_MS).min(self.duration_ms);
        let reference = pcm_window(track, source, end)?;
        let observed = pcm_window(&mut self.mix, search_start, search_end)?;
        let WaveformMatch::Located(offset) = self.matcher.search_near_onset(&reference, &observed, ALIGN_HZ) else {
            return None;
        };
        let located = search_start + samples_to_ms(offset);
        (target.abs_diff(located) <= MATCH_AGREEMENT_MS).then_some(target)
    }

    // An error means a strong ambiguous waveform, rather than missing evidence.
    fn find_start(
        &mut self,
        reference: &[f32],
        hint: i64,
        row_offset: i64,
        context_duration: i64,
        check_ambiguity: bool,
    ) -> Result<Option<i64>, ()> {
        let context_hint = hint - row_offset;
        let search_start = (context_hint - SEARCH_MS).max(0);
        let search_end = context_hint
            .saturating_add(SEARCH_MS + context_duration)
            .min(self.duration_ms);
        let Some(mut observed) = pcm_window(&mut self.mix, search_start, search_end) else {
            return Ok(None);
        };
        observed.truncate(reference.len() + (2 * SEARCH_MS * i64::from(ALIGN_HZ) / 1000) as usize);
        let offset = if check_ambiguity {
            match self.matcher.search(reference, &observed, ALIGN_HZ) {
                WaveformMatch::Located(offset) => Some(offset),
                WaveformMatch::Missing => None,
                WaveformMatch::Ambiguous => return Err(()),
            }
        } else {
            self.matcher.offset(reference, &observed, ALIGN_HZ)
        };
        Ok(offset.map(|offset| {
            search_start + samples_to_ms(offset) + row_offset
        }))
    }
}

fn samples_to_ms(offset: usize) -> i64 {
    (offset as i64 * 1000 + i64::from(ALIGN_HZ) / 2) / i64::from(ALIGN_HZ)
}

/// The seek may skip exact zero samples only. Even very quiet preceding
/// audio makes the earlier position ineligible.
fn first_signal_or_end_ms(wave: &mut Wave, start: i64, end: i64) -> Option<i64> {
    if end <= start {
        return None;
    }
    let spec = wave.spec();
    let rate = i64::from(spec.sample_rate);
    let first = u32::try_from(start * rate / 1000).ok()?;
    let last = u32::try_from(end * rate / 1000).ok()?;
    if last > wave.duration() {
        return None;
    }
    wave.seek(first).ok()?;
    let mut samples = wave.samples::<i16>();
    for frame in first..last {
        for _ in 0..spec.channels {
            if samples.next()?.ok()? != 0 {
                return Some(i64::from(frame) * 1000 / rate);
            }
        }
    }
    Some(end)
}

fn pcm_window(wave: &mut Wave, start_ms: i64, end_ms: i64) -> Option<Vec<f32>> {
    if start_ms < 0 || end_ms <= start_ms {
        return None;
    }
    let spec = wave.spec();
    let start = u32::try_from(start_ms * i64::from(spec.sample_rate) / 1000).ok()?;
    let end = (end_ms * i64::from(spec.sample_rate) / 1000).min(i64::from(wave.duration())) as u32;
    let frame_count = end.checked_sub(start)?;
    wave.seek(start).ok()?;
    let mut samples = wave.samples::<i16>();
    let mut frames = Vec::with_capacity(frame_count as usize);
    for _ in 0..frame_count {
        let mut mono = 0.0;
        for _ in 0..spec.channels {
            mono += f32::from(samples.next()?.ok()?);
        }
        frames.push(mono / (f32::from(spec.channels) * 32768.0));
    }
    Some(Resampler::new(spec.sample_rate, ALIGN_HZ).process_offline(&frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_discord::tracks::CaptureSpan;
    use std::io::{Seek, SeekFrom};

    const EARLY: [usize; 3] = [6_000, 14_000, 22_000];
    const LATE: usize = 35_000;
    const CONTINUOUS_LAG: usize = 800;
    const LATE_CLOCK_LAG: usize = 7_400;
    const DURATION: usize = 60_000;

    fn wave(samples: &[i16]) -> Wave {
        let mut file = tempfile::tempfile().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: ALIGN_HZ,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut file, spec).unwrap();
        for &sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        WavReader::new(BufReader::new(file)).unwrap()
    }

    fn fixture(late_lags: &[usize]) -> (Playback, Wave, Anchors) {
        let hz_per_ms = ALIGN_HZ as usize / 1000;
        let mut track = vec![0; DURATION * hz_per_ms];
        let mut mix = vec![0; DURATION * hz_per_ms];
        let mut random = 40_u32;
        for start in EARLY.into_iter().chain([LATE]) {
            let first = (start - 1_100) * hz_per_ms;
            let last = (start + 3_100) * hz_per_ms;
            for sample in &mut track[first..last] {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *sample = ((random >> 17) as i16) - 16_384;
            }
            let lags = if start == LATE { late_lags } else { &[CONTINUOUS_LAG] };
            for &lag in lags {
                let offset = lag * hz_per_ms;
                mix[first + offset..last + offset].copy_from_slice(&track[first..last]);
            }
        }
        let origin = 10_000_000_u64;
        let span = |start_ms: usize, end_ms: usize, lag: usize| CaptureSpan {
            start_tick: start_ms as u64 / TICK_MS,
            end_tick: end_ms as u64 / TICK_MS,
            first_elapsed_us: (start_ms + lag) as u64 * 1000,
            last_elapsed_us: (end_ms + lag - TICK_MS as usize) as u64 * 1000,
        };
        let anchors = Anchors::Discord(Some(CaptureTimeline {
            attempt_started_unix_us: Some(origin),
            local_epoch_unix_us: Some(origin),
            local_epoch_elapsed_us: Some(0),
            capture_closed_elapsed_us: (DURATION + LATE_CLOCK_LAG) as u64 * 1000,
            pause_seen: false,
            spans: vec![
                span(0, 30_000, CONTINUOUS_LAG),
                span(30_000, DURATION, LATE_CLOCK_LAG),
            ],
            writer_drops: vec![],
            truncated: false,
        }));
        (
            Playback {
                mix: wave(&mix),
                origin_us: Some(origin as i64),
                duration_ms: DURATION as i64,
                matcher: WaveformMatcher::default(),
                media_lag: MediaLag::default(),
            },
            wave(&track),
            anchors,
        )
    }

    fn segment(start: usize) -> BotSegment {
        BotSegment {
            speaker_id: "test".into(),
            speaker_name: None,
            start_ms: start as i64,
            end_ms: start as i64 + 2_000,
            playback_start_ms: None,
            playback_end_ms: None,
            text: "synthetic".into(),
        }
    }

    fn establish_lag(playback: &mut Playback, track: &mut Wave, anchors: &Anchors) {
        for start in EARLY {
            let mut row = segment(start);
            playback.align(track, anchors, &mut row);
            assert_eq!(row.playback_start_ms, Some((start + CONTINUOUS_LAG) as i64));
        }
    }

    #[test]
    fn discord_recovers_continuous_media_after_wall_clock_jump() {
        let (mut playback, mut track, anchors) = fixture(&[CONTINUOUS_LAG]);
        let mut row = segment(LATE);
        playback.align(&mut track, &anchors, &mut row);
        assert_eq!(row.playback_start_ms, None);
        establish_lag(&mut playback, &mut track, &anchors);
        playback.align(&mut track, &anchors, &mut row);
        assert_eq!(row.playback_start_ms, Some((LATE + CONTINUOUS_LAG) as i64));
        assert_eq!(row.playback_end_ms, Some((LATE + CONTINUOUS_LAG + 2_000) as i64));
    }

    #[test]
    fn discord_keeps_a_real_media_jump_when_only_the_clock_match_is_present() {
        let (mut playback, mut track, anchors) = fixture(&[LATE_CLOCK_LAG]);
        establish_lag(&mut playback, &mut track, &anchors);
        let mut row = segment(LATE);
        playback.align(&mut track, &anchors, &mut row);
        assert_eq!(row.playback_start_ms, Some((LATE + LATE_CLOCK_LAG) as i64));
    }

    #[test]
    fn discord_leaves_missing_or_competing_waveforms_unlinked() {
        for lags in [vec![], vec![CONTINUOUS_LAG, LATE_CLOCK_LAG], vec![CONTINUOUS_LAG, 5_700, 10_100]] {
            let (mut playback, mut track, anchors) = fixture(&lags);
            establish_lag(&mut playback, &mut track, &anchors);
            let mut row = segment(LATE);
            playback.align(&mut track, &anchors, &mut row);
            assert_eq!(row.playback_start_ms, None, "lags {lags:?}");
        }
    }

    const RAW: usize = 5_000;
    const ONSET: usize = 6_440;
    const END: usize = 9_000;

    fn trim_fixture(earlier_mix: bool, missing_mix: usize, span_split: bool) -> (Playback, Wave, Anchors) {
        let hz = ALIGN_HZ as usize / 1000;
        let duration = 14_000;
        let mut source = vec![0_i16; duration * hz];
        let mut mixed = vec![0_i16; source.len()];
        let mut seed = 72_u32;
        for (index, sample) in source[ONSET * hz..8_800 * hz].iter_mut().enumerate() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 17) as i16) - 16_384;
            *sample = if index < 100 * hz { noise / 2000 } else { noise };
        }
        source[ONSET * hz] = 1;
        let lag = CONTINUOUS_LAG * hz;
        mixed[lag..12_000 * hz].copy_from_slice(&source[..12_000 * hz - lag]);
        if earlier_mix {
            mixed[(RAW + CONTINUOUS_LAG + 10) * hz] = 1;
        }
        let local_onset = ONSET + CONTINUOUS_LAG;
        mixed[local_onset * hz..(local_onset + missing_mix) * hz].fill(0);
        let span = |first_ms: usize, last_ms: usize| CaptureSpan {
            start_tick: first_ms as u64 / TICK_MS,
            end_tick: last_ms as u64 / TICK_MS,
            first_elapsed_us: (first_ms + CONTINUOUS_LAG) as u64 * 1000,
            last_elapsed_us: (last_ms + CONTINUOUS_LAG) as u64 * 1000 - TICK_MS * 1000,
        };
        let origin = 10_000_000_u64;
        let anchors = Anchors::Discord(Some(CaptureTimeline {
            attempt_started_unix_us: Some(origin),
            local_epoch_unix_us: Some(origin),
            local_epoch_elapsed_us: Some(0),
            capture_closed_elapsed_us: (duration + CONTINUOUS_LAG) as u64 * 1000,
            pause_seen: false,
            spans: if span_split { vec![span(0, 6_000), span(6_000, duration)] } else { vec![span(0, duration)] },
            writer_drops: vec![],
            truncated: false,
        }));
        (Playback {
            mix: wave(&mixed), origin_us: Some(origin as i64), duration_ms: duration as i64,
            matcher: WaveformMatcher::default(), media_lag: MediaLag::default(),
        }, wave(&source), anchors)
    }

    #[test]
    fn verified_link_skips_only_exact_silence_before_a_quiet_attack() {
        let (mut playback, mut source, anchors) = trim_fixture(false, 0, false);
        let mut row = segment(RAW);
        row.end_ms = END as i64;
        playback.align(&mut source, &anchors, &mut row);
        assert_eq!(row.playback_start_ms, Some((ONSET + CONTINUOUS_LAG) as i64 - ONSET_PRE_ROLL_MS));
        assert_eq!(row.playback_end_ms, Some((END + CONTINUOUS_LAG) as i64));
        assert_eq!((row.start_ms, row.end_ms, row.text.as_str()), (RAW as i64, END as i64, "synthetic"));
    }

    #[test]
    fn onset_trim_refuses_earlier_mix_audio_missing_onset_and_span_boundaries() {
        for (earlier, missing, split) in [(true, 0, false), (false, 400, false), (false, 0, true)] {
            let (mut playback, mut source, anchors) = trim_fixture(earlier, missing, split);
            let mut row = segment(RAW);
            row.end_ms = END as i64;
            assert_eq!(playback.trim_verified_silence(&mut source, &anchors, &row, (RAW + CONTINUOUS_LAG) as i64), None);
        }
    }
}

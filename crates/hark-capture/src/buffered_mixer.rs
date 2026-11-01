//! Align audio by sample position, retaining callback remainders between ticks.

use crate::dsp::mix;
use std::collections::VecDeque;

enum Block { Silence(usize), Samples { data: Vec<f32>, offset: usize } }

#[derive(Default)]
struct SampleQueue { blocks: VecDeque<Block>, len: usize }

impl SampleQueue {
    fn push(&mut self, samples: &[f32]) {
        if !samples.is_empty() { self.blocks.push_back(Block::Samples { data: samples.to_vec(), offset: 0 }); self.len += samples.len(); }
    }
    fn silence(&mut self, count: usize) {
        if count == 0 { return; }
        if let Some(Block::Silence(last)) = self.blocks.back_mut() { *last += count; }
        else { self.blocks.push_back(Block::Silence(count)); }
        self.len += count;
    }
    fn discard(&mut self, mut count: usize) {
        count = count.min(self.len);
        self.len -= count;
        while count > 0 {
            match self.blocks.front_mut().unwrap() {
                Block::Silence(available) => { let take = count.min(*available); *available -= take; count -= take; if *available == 0 { self.blocks.pop_front(); } }
                Block::Samples { data, offset } => { let take = count.min(data.len() - *offset); *offset += take; count -= take; if *offset == data.len() { self.blocks.pop_front(); } }
            }
        }
    }
    fn take(&mut self, count: usize) -> Vec<f32> {
        let count = count.min(self.len);
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            let available = count - out.len();
            match self.blocks.front().unwrap() {
                Block::Silence(n) => out.resize(out.len() + available.min(*n), 0.0),
                Block::Samples { data, offset } => out.extend_from_slice(&data[*offset..*offset + available.min(data.len() - *offset)]),
            }
            let taken = out.len() - (count - available);
            self.discard(taken);
        }
        out
    }
    fn retain_newest(&mut self, count: usize) { self.discard(self.len.saturating_sub(count)); }
    fn clear(&mut self) { self.blocks.clear(); self.len = 0; }
    #[cfg(test)] fn len(&self) -> usize { self.len }
    #[cfg(test)] fn is_empty(&self) -> bool { self.len == 0 }
}

pub(super) struct BufferedMixer {
    mic: SampleQueue,
    sys: SampleQueue,
    has_sys: bool,
    max_pending: usize,
    total_mic: usize,
    total_sys: usize,
    emitted: usize,
    degraded: bool,
}

impl BufferedMixer {
    pub(super) fn new(has_sys: bool, max_pending: usize) -> Self {
        Self { mic: SampleQueue::default(), sys: SampleQueue::default(), has_sys, max_pending, total_mic: 0, total_sys: 0, emitted: 0, degraded: false }
    }

    #[cfg(test)]
    pub(super) fn push(&mut self, mic: &[f32], sys: &[f32]) -> Vec<f32> {
        self.enqueue(mic, 0, sys, 0);
        self.drain_ready(usize::MAX)
    }

    /// Leading silence is a logical span, not a proportional allocation. Silence
    /// already output while waiting for a late source is accounted for only once.
    #[cfg(test)]
    pub(super) fn enqueue(&mut self, mic: &[f32], mic_gap: usize, sys: &[f32], sys_gap: usize) {
        self.enqueue_source(0, mic, mic_gap);
        self.enqueue_source(1, sys, sys_gap);
        self.finish_enqueue();
    }

    pub(super) fn enqueue_source(&mut self, source: usize, samples: &[f32], gap: usize) {
        if source == 1 && !self.has_sys { return; }
        let (queue, total) = if source == 0 { (&mut self.mic, &mut self.total_mic) } else { (&mut self.sys, &mut self.total_sys) };
        let covered = self.emitted.saturating_sub(*total);
        let gap_covered = covered.min(gap);
        let samples_covered = covered.saturating_sub(gap).min(samples.len());
        queue.silence(gap - gap_covered);
        queue.push(&samples[samples_covered..]);
        *total += gap + samples.len();
    }

    pub(super) fn finish_enqueue(&mut self) {
        if self.has_sys && self.mic.len.abs_diff(self.sys.len) > self.max_pending { self.degraded = true; }
    }

    pub(super) fn drain_ready(&mut self, max: usize) -> Vec<f32> {
        let count = if self.degraded {
            let count = self.total_mic.max(self.total_sys) - self.emitted;
            let retain = count;
            self.mic.retain_newest(retain); self.sys.retain_newest(retain);
            count
        } else if self.has_sys { self.mic.len.min(self.sys.len) } else { self.mic.len };
        self.take(count.min(max))
    }

    #[cfg(test)]
    pub(super) fn finish(&mut self) -> Vec<f32> { self.finish_block(usize::MAX) }

    pub(super) fn finish_block(&mut self, max: usize) -> Vec<f32> {
        let remaining = self.total_mic.max(self.total_sys) - self.emitted;
        if self.degraded { self.mic.retain_newest(remaining); self.sys.retain_newest(remaining); }
        let mixed = self.take(remaining.min(max));
        if self.total_mic.max(self.total_sys) == self.emitted {
            self.mic.clear(); self.sys.clear(); self.total_mic = 0; self.total_sys = 0; self.emitted = 0; self.degraded = false;
        }
        mixed
    }

    pub(super) fn is_degraded(&self) -> bool { self.degraded }

    fn take(&mut self, count: usize) -> Vec<f32> {
        let mic = self.mic.take(count);
        let sys = self.sys.take(count);
        let mut mixed = mix(&mic, &sys, 1.0, 1.0);
        mixed.resize(count, 0.0);
        self.emitted += count;
        mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_source_prefixes_share_the_same_frame_zero() {
        let mut mixer = BufferedMixer::new(true, 100);
        mixer.enqueue(&[0.1, 0.2], 2, &[0.3], 3);
        let mut mixed = Vec::new();
        loop { let block = mixer.drain_ready(1); if block.is_empty() { break; } mixed.extend(block); }
        mixed.extend(mixer.finish());
        assert_eq!(mixed, vec![0.0, 0.0, 0.1, 0.5]);
    }

    #[test]
    fn large_common_silence_is_sparse_and_drains_in_bounded_blocks() {
        let mut mixer = BufferedMixer::new(true, 14_400);
        mixer.enqueue(&[], 1_000_000_000, &[], 1_000_000_000);
        assert_eq!(mixer.mic.blocks.len(), 1);
        assert_eq!(mixer.sys.blocks.len(), 1);
        assert_eq!(mixer.drain_ready(4_800), vec![0.0; 4_800]);
        assert_eq!(mixer.mic.len, 1_000_000_000 - 4_800);
    }

    #[test]
    fn measured_late_source_prefix_does_not_replay_previous_silence() {
        let mut mixer = BufferedMixer::new(true, 14_400);
        assert_eq!(mixer.push(&vec![0.1; 48_000], &[]).len(), 48_000);
        mixer.enqueue(&[0.1; 4_800], 0, &[0.4; 4_800], 48_000);
        assert_eq!(mixer.drain_ready(4_800), vec![0.5; 4_800]);
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn asynchronous_equal_streams_preserve_every_sample_without_growth() {
        let mut mixer = BufferedMixer::new(true, 14_400);
        let mut actual = Vec::new();
        let mut mic_all = Vec::new();
        let mut sys_all = Vec::new();
        for tick in 0..1_200 {
            let mic: Vec<_> = (0..4_800).map(|i| ((mic_all.len() + i) % 97) as f32 / 1_000.0).collect();
            let sys_len = match tick % 10 { 0 => 4_320, 1 => 5_280, _ => 4_800 };
            let sys: Vec<_> = (0..sys_len).map(|i| ((sys_all.len() + i) % 89) as f32 / 1_000.0).collect();
            actual.extend(mixer.push(&mic, &sys));
            mic_all.extend(mic);
            sys_all.extend(sys);
        }
        actual.extend(mixer.finish());
        assert_eq!(actual.len(), 120 * 48_000);
        assert_eq!(actual, mix(&mic_all, &sys_all, 1.0, 1.0));
    }

    #[test]
    fn finishing_flushes_the_unmatched_tail_once() {
        let mut mixer = BufferedMixer::new(true, 100);
        assert_eq!(mixer.push(&[0.1, 0.2, 0.3], &[0.4]), vec![0.5]);
        assert_eq!(mixer.finish(), vec![0.2, 0.3]);
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn microphone_only_does_not_wait_for_system_audio() {
        let mut mixer = BufferedMixer::new(false, 100);
        assert_eq!(mixer.push(&[0.1, 0.2], &[]), vec![0.1, 0.2]);
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn missing_system_source_has_bounded_backlog_and_keeps_microphone_samples() {
        let mut mixer = BufferedMixer::new(true, 30);
        let mut actual = Vec::new();
        for _ in 0..1_000 {
            actual.extend(mixer.push(&[0.25; 10], &[]));
            assert!(mixer.mic.len() <= 30);
            assert!(mixer.sys.is_empty());
        }
        actual.extend(mixer.finish());
        assert_eq!(actual, vec![0.25; 10_000]);
    }

    #[test]
    fn interval_boundary_does_not_mix_old_remainder_with_resumed_audio() {
        let mut mixer = BufferedMixer::new(true, 100);
        assert!(mixer.push(&[0.1], &[]).is_empty());
        assert_eq!(mixer.finish(), vec![0.1]);
        assert_eq!(mixer.push(&[0.2], &[0.3]), vec![0.5]);
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn delayed_bulk_catch_up_cannot_replay_time_already_substituted_with_silence() {
        let mut mixer = BufferedMixer::new(true, 14_400);
        let first = mixer.push(&vec![0.1; 48_000], &[]);
        assert_eq!(first.len(), 48_000);
        assert!(mixer.is_degraded());
        assert!(mixer.push(&[], &vec![0.5; 48_000]).is_empty());
        assert!(mixer.finish().is_empty());
        assert!(!mixer.is_degraded());
        assert_eq!(mixer.push(&[0.25], &[0.25]), vec![0.5]);
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn fully_late_catch_up_does_not_leak_into_a_future_output_window() {
        let mut mixer = BufferedMixer::new(true, 30);
        assert_eq!(mixer.push(&[], &[0.25; 100]), vec![0.25; 100]);
        assert_eq!(mixer.push(&[], &[0.25; 100]), vec![0.25; 100]);
        assert!(mixer.push(&[0.1; 200], &[]).is_empty());
        assert_eq!(mixer.push(&[], &[0.5; 10]), vec![0.5; 10]);
    }

    #[test]
    fn stale_catch_up_and_new_leading_audio_in_one_batch_do_not_overlap() {
        let mut mixer = BufferedMixer::new(true, 30);
        assert_eq!(mixer.push(&[], &[0.25; 200]), vec![0.25; 200]);
        assert_eq!(mixer.push(&[0.1; 200], &[0.5; 10]), vec![0.5; 10]);
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn system_audio_continues_when_microphone_stalls_without_growth_on_mic_catch_up() {
        let mut mixer = BufferedMixer::new(true, 30);
        assert_eq!(mixer.push(&[], &[0.25; 100]), vec![0.25; 100]);
        assert_eq!(mixer.push(&[], &[0.5; 100]), vec![0.5; 100]);
        assert!(mixer.push(&[0.1; 200], &[]).is_empty());
        assert!(mixer.finish().is_empty());
    }

    #[test]
    fn resumed_sound_is_not_muted_by_the_duration_of_previous_silence() {
        let mut mixer = BufferedMixer::new(true, 30);
        let mut mixed = mixer.push(&[0.0; 1_000], &[]);
        // The first system packet's measured origin follows 1,000 frames of
        // actual silence; the packet is current audio, not stale catch-up.
        mixer.enqueue(&[0.0; 10], 0, &[0.5; 10], 1_000);
        mixed.extend(mixer.drain_ready(usize::MAX));
        mixed.extend(mixer.finish());
        assert_eq!(mixed.len(), 1_010);
        assert_eq!(&mixed[1_000..], &[0.5; 10]);
    }

    #[test]
    fn initial_system_delay_keeps_newest_sound_in_the_current_output_window() {
        let mut mixer = BufferedMixer::new(true, 30);
        let mut mixed = mixer.push(&[0.1; 400], &[]);
        let mut late_system = vec![0.2; 400];
        late_system.extend_from_slice(&[0.4; 100]);
        mixed.extend(mixer.push(&[0.1; 100], &late_system));
        mixed.extend(mixer.finish());
        assert_eq!(mixed.len(), 500);
        assert_eq!(&mixed[400..], &[0.5; 100]);
    }
}

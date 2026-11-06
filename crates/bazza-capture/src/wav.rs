//! Crash-tolerant 16-bit PCM WAV writer: flushes to disk about once per second
//! of audio so a crash loses at most that much.

use std::fs::File;
use std::io::{BufWriter, Read};
use std::path::Path;

use crate::dsp::{rms_db, to_mono, Resampler};

/// Read mono PCM at `sample_rate`, with bounded decoding/resampler workspace.
/// The final 64 native silence samples match the post-processing loader's tail.
pub fn read_mono_resampled(path: &Path, sample_rate: u32) -> hound::Result<Vec<f32>> {
    read_mono_resampled_from(std::io::BufReader::new(File::open(path)?), sample_rate)
}

/// Decode audio from an already opened and authorized media handle.
pub fn read_mono_resampled_from(input: impl Read, sample_rate: u32) -> hound::Result<Vec<f32>> {
    let reader = hound::WavReader::new(input)?;
    let mut output = Vec::new();
    resample_reader(reader, sample_rate, |block| output.extend_from_slice(block))?;
    Ok(output)
}

/// RMS dBFS for consecutive resampled hops, including the final partial hop.
/// Only one hop and bounded decode blocks are held while scanning the WAV.
pub fn rms_resampled(path: &Path, sample_rate: u32, hop_samples: usize) -> hound::Result<Vec<f32>> {
    if hop_samples == 0 { return Err(hound::Error::FormatError("RMS hop must be nonzero")); }
    let reader = hound::WavReader::open(path)?;
    let mut output = Vec::new();
    let mut hop = Vec::with_capacity(hop_samples);
    resample_reader(reader, sample_rate, |mut block| {
        while !block.is_empty() {
            let count = (hop_samples - hop.len()).min(block.len());
            hop.extend_from_slice(&block[..count]);
            block = &block[count..];
            if hop.len() == hop_samples {
                output.push(rms_db(&hop));
                hop.clear();
            }
        }
    })?;
    if !hop.is_empty() { output.push(rms_db(&hop)); }
    Ok(output)
}

fn resample_reader<R: std::io::Read>(
    reader: hound::WavReader<R>, sample_rate: u32, consume: impl FnMut(&[f32]),
) -> hound::Result<()> {
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 || sample_rate == 0 {
        return Err(hound::Error::FormatError("sample rates and channel count must be nonzero"));
    }
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = ((1u64 << (spec.bits_per_sample - 1)) - 1) as f32;
            resample_samples(reader.into_samples::<i32>().map(|s| s.map(|v| v as f32 / max)),
                spec.channels as usize, spec.sample_rate, sample_rate, consume)
        }
        hound::SampleFormat::Float => resample_samples(reader.into_samples::<f32>(),
            spec.channels as usize, spec.sample_rate, sample_rate, consume),
    }
}

fn resample_samples(
    mut samples: impl Iterator<Item = hound::Result<f32>>, channels: usize,
    from_hz: u32, to_hz: u32, mut consume: impl FnMut(&[f32]),
) -> hound::Result<()> {
    // Align blocks to complete frames; also bound workspace for many channels.
    let block_samples = (8192 / channels).max(1) * channels;
    let mut interleaved = Vec::with_capacity(block_samples);
    let mut resampler = Resampler::new(from_hz, to_hz);
    loop {
        interleaved.clear();
        for sample in samples.by_ref().take(block_samples) { interleaved.push(sample?); }
        if interleaved.is_empty() { break; }
        let mono = to_mono(&interleaved, channels);
        consume(&resampler.process(&mono));
    }
    consume(&resampler.process(&[0.0; 64]));
    Ok(())
}

pub struct WavWriter {
    inner: Option<hound::WavWriter<BufWriter<File>>>,
    sample_rate: u32,
    since_flush: usize,
}

impl WavWriter {
    pub fn create(path: &Path, sample_rate: u32, channels: u16) -> hound::Result<WavWriter> {
        let spec = hound::WavSpec { channels, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        Ok(WavWriter { inner: Some(hound::WavWriter::create(path, spec)?), sample_rate, since_flush: 0 })
    }

    /// Write f32 samples in [-1, 1] (interleaved if channels > 1).
    pub fn write(&mut self, samples: &[f32]) -> hound::Result<()> {
        let w = self.inner.as_mut().expect("writer finished");
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        self.since_flush += samples.len();
        if self.since_flush >= self.sample_rate as usize {
            w.flush()?;
            self.since_flush = 0;
        }
        Ok(())
    }

    /// Finalize the header. Safe to call once; dropping without it still
    /// leaves a readable file because `flush` updates the header.
    pub fn finish(mut self) -> hound::Result<()> {
        if let Some(w) = self.inner.take() {
            w.finalize()?;
        }
        Ok(())
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        if let Some(w) = self.inner.take() {
            let _ = w.finalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_resampling_delivers_bounded_blocks_before_reading_the_whole_recording() {
        use std::cell::Cell;

        // Measure work between deliveries, not elapsed time or allocator noise.
        // The generated source itself never allocates a recording-sized buffer.
        const INPUT_BUDGET: usize = 16_384;
        for total_frames in [32_768, 131_072] {
            for channels in [1, 2] {
                for (from_hz, to_hz) in [(16_000, 16_000), (48_000, 16_000), (16_000, 48_000)] {
                    let read = Cell::new(0usize);
                    let delivered_at = Cell::new(0usize);
                    let deliveries = Cell::new(0usize);
                    let output_samples = Cell::new(0usize);
                    let samples = (0..total_frames * channels).map(|index| {
                        read.set(read.get() + 1);
                        Ok((index % 31) as f32 / 31.0)
                    });
                    let output_budget = INPUT_BUDGET * (to_hz as usize).div_ceil(from_hz as usize) + 128;
                    resample_samples(samples, channels, from_hz, to_hz, |block| {
                        assert!(read.get() - delivered_at.get() <= INPUT_BUDGET,
                            "decoded too much audio before delivering a block: {total_frames} frames, {channels} channels, {from_hz}->{to_hz}");
                        assert!(block.len() <= output_budget, "resampled output accumulated into a recording-sized block");
                        delivered_at.set(read.get());
                        deliveries.set(deliveries.get() + 1);
                        output_samples.set(output_samples.get() + block.len());
                    }).unwrap();
                    assert_eq!(read.get(), total_frames * channels);
                    assert!(deliveries.get() > 2, "long inputs must be streamed to the consumer");
                    assert!(output_samples.get() >= total_frames * to_hz as usize / from_hz as usize,
                        "bounded processing must still deliver the complete recording");
                }
            }
        }
    }

    fn signal_wav(path: &Path, rate: u32, channels: u16, float: bool, frames: usize) {
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: if float { 32 } else { 16 },
            sample_format: if float { hound::SampleFormat::Float } else { hound::SampleFormat::Int } };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            for channel in 0..channels {
                let value = (frame as f32 * 0.063 + channel as f32 * 0.41).sin() * 0.7;
                if float { writer.write_sample(value).unwrap(); }
                else { writer.write_sample((value * i16::MAX as f32) as i16).unwrap(); }
            }
        }
        writer.finalize().unwrap();
    }

    // The former post-processing loader supplies the output/tail oracle.
    fn reference_resampled(path: &Path, output_rate: u32) -> Vec<f32> {
        let reader = hound::WavReader::open(path).unwrap();
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let max = ((1u64 << (spec.bits_per_sample - 1)) - 1) as f32;
                reader.into_samples::<i32>().map(|sample| sample.unwrap() as f32 / max).collect()
            }
            hound::SampleFormat::Float => reader.into_samples::<f32>().map(|sample| sample.unwrap()).collect(),
        };
        let mono = to_mono(&samples, spec.channels as usize);
        let mut resampler = Resampler::new(spec.sample_rate, output_rate);
        let mut result = resampler.process(&mono);
        result.extend(resampler.process(&[0.0; 64]));
        result
    }

    #[test]
    fn streamed_pcm_preserves_rates_channels_formats_and_the_flush_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("signal.wav");
        for rate in [8_000, 16_000, 22_050, 44_100, 48_000, 96_000] {
            for channels in [1, 2] {
                for float in [false, true] {
                    signal_wav(&path, rate, channels, float, 25_013);
                    let reference = reference_resampled(&path, 16_000);
                    let actual = read_mono_resampled(&path, 16_000).unwrap();
                    assert_eq!(actual.len(), reference.len(), "output length changed for {rate} Hz/{channels} channels");
                    let max_error = actual.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                    assert!(max_error < 0.0001, "sample error {max_error} for {rate} Hz/{channels} channels");
                    if rate == 16_000 || rate == 48_000 {
                        assert!(actual.iter().zip(&reference).all(|(a, b)| a.to_bits() == b.to_bits()));
                    }
                }
            }
        }
    }

    #[test]
    fn streamed_rms_matches_full_pcm_hops_including_the_final_partial_hop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("levels.wav");
        for rate in [16_000, 44_100, 48_000] {
            signal_wav(&path, rate, 2, false, 30_017);
            let reference: Vec<f32> = reference_resampled(&path, 16_000).chunks(4000).map(rms_db).collect();
            let actual = rms_resampled(&path, 16_000, 4000).unwrap();
            assert_eq!(actual.len(), reference.len());
            assert!(actual.iter().zip(reference).all(|(a, b)| (a - b).abs() < 0.001));
        }
        assert!(rms_resampled(&path, 16_000, 0).is_err());
    }

    #[test]
    fn short_and_empty_inputs_retain_existing_tail_samples() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("short.wav");
        for rate in [16_000, 48_000] {
            for frames in [0, 1, 7] {
                signal_wav(&path, rate, 1, false, frames);
                assert_eq!(read_mono_resampled(&path, 16_000).unwrap(), reference_resampled(&path, 16_000));
            }
        }
    }

    #[test]
    fn wav_writer_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let mut w = WavWriter::create(&p, 48000, 1).unwrap();
        w.write(&[0.25; 4800]).unwrap();
        w.finish().unwrap();
        let r = hound::WavReader::open(&p).unwrap();
        assert_eq!(r.spec().sample_rate, 48000);
        assert_eq!(r.spec().channels, 1);
        assert_eq!(r.len(), 4800);
        let first = r.into_samples::<i16>().next().unwrap().unwrap();
        assert!((first as f32 / i16::MAX as f32 - 0.25).abs() < 0.001);
    }

    #[test]
    fn dropped_writer_is_still_readable() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.wav");
        {
            let mut w = WavWriter::create(&p, 16000, 1).unwrap();
            w.write(&[0.1; 20000]).unwrap();
        }
        assert_eq!(hound::WavReader::open(&p).unwrap().len(), 20000);
    }
}

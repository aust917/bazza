use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use hound::{SampleFormat, WavSpec, WavWriter};
use serde::{Deserialize, Serialize};

use crate::{
    ingress::{ClockSnapshot, Packet, PacketTimeline},
    SAMPLE_RATE,
};

pub const TRACK_DIRECTORY: &str = "attendee";
pub const MANIFEST_FILE: &str = "tracks.json";
const SAMPLES_PER_MS: u64 = SAMPLE_RATE as u64 / 1000;
pub(crate) const MAX_PARTICIPANTS: usize = 64;
const MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TRACK_SAMPLES: u64 = 8 * 60 * 60 * SAMPLE_RATE as u64;
const JITTER_MS: u64 = 25;
const MAX_REPORTED_GAPS: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub tracks: Vec<TrackInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<Timing>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Timing {
    /// Wall time corresponding to the capture origin, for log correlation only.
    pub origin_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_unix_micros: Option<u64>,
    /// Elapsed time on the host monotonic clock, with recording pauses removed.
    pub local_elapsed_ms: u64,
    pub excluded_pause_ms: u64,
    pub queue_dropped_packets: u64,
    /// Unexpected provider timestamp jumps. Starting and resuming do not count.
    pub provider_clock_reanchors: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackInfo {
    pub participant_id: String,
    pub display_name: Option<String>,
    pub file: String,
    pub received_samples: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<TrackCoverage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackCoverage {
    /// Actual arrival times for packets that contributed samples, on the local clock.
    pub first_observed_ms: u64,
    pub last_observed_ms: u64,
    /// Positions in the saved WAV estimated from provider timestamps and local arrival.
    pub first_pcm_sample: u64,
    pub last_pcm_end_sample: u64,
    /// Internal padding, which could represent silence or missing audio.
    pub internal_gap_samples: u64,
    pub internal_gap_count: u64,
    /// First few internal gaps; the totals include gaps omitted here.
    pub internal_gaps: Vec<SampleInterval>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SampleInterval {
    pub start_sample: u64,
    pub end_sample: u64,
}

pub(crate) struct Tracks {
    directory: PathBuf,
    slots: Vec<Track>,
    indexes: HashMap<String, usize>,
    timeline: PacketTimeline,
    end_sample: u64,
}

struct Track {
    id: String,
    file: String,
    writer: WavWriter<BufWriter<File>>,
    written: u64,
    received: u64,
    coverage: Option<TrackCoverage>,
}

#[derive(Clone, Copy)]
pub(crate) enum TrackError {
    Disk,
    Limit,
}

impl Tracks {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            slots: Vec::new(),
            indexes: HashMap::new(),
            timeline: PacketTimeline::default(),
            end_sample: 0,
        }
    }

    pub fn write(&mut self, packet: Packet) -> Result<(), TrackError> {
        let Some(position_ms) = self.timeline.position_ms(&packet) else {
            return Ok(());
        };
        let mut offset = position_ms.saturating_mul(SAMPLES_PER_MS);
        let index = self.indexes.get(&packet.participant_id).copied();
        if let Some(index) = index {
            let written = self.slots[index].written;
            if offset.abs_diff(written) <= JITTER_MS * SAMPLES_PER_MS {
                offset = written;
            }
        }
        let count = self.slots.len() + usize::from(index.is_none());
        let end = offset.saturating_add((packet.pcm.len() / 2) as u64);
        if count > MAX_PARTICIPANTS || end.max(self.end_sample) > max_samples(count) {
            return Err(TrackError::Limit);
        }
        let index = match index {
            Some(index) => index,
            None => {
                let index = self.slots.len();
                let track = Track::create(&self.directory, index, packet.participant_id)
                    .map_err(|_| TrackError::Disk)?;
                self.indexes.insert(track.id.clone(), index);
                self.slots.push(track);
                index
            }
        };
        let track = &mut self.slots[index];
        track
            .write(offset, &packet.pcm, packet.observation.elapsed_ms)
            .map_err(|_| TrackError::Disk)?;
        self.end_sample = self.end_sample.max(track.written);
        Ok(())
    }

    pub fn finish(
        self,
        names: &HashMap<String, String>,
        clock: ClockSnapshot,
        queue_dropped_packets: u64,
    ) -> Result<usize, TrackError> {
        let end = clock
            .elapsed_ms
            .saturating_mul(SAMPLES_PER_MS)
            .max(self.end_sample);
        let target = end.min(max_samples(self.slots.len()));
        let mut error = (target < end).then_some(TrackError::Limit);
        let mut manifest = Manifest {
            schema_version: 1,
            sample_rate: SAMPLE_RATE,
            duration_ms: target.div_ceil(SAMPLES_PER_MS),
            tracks: Vec::with_capacity(self.slots.len()),
            timing: Some(Timing {
                origin_unix_ms: clock.origin_unix_micros.map(|time| time / 1_000),
                origin_unix_micros: clock.origin_unix_micros,
                local_elapsed_ms: clock.elapsed_ms,
                excluded_pause_ms: clock.excluded_pause_ms,
                queue_dropped_packets,
                provider_clock_reanchors: self.timeline.reanchors,
            }),
        };
        for mut track in self.slots {
            let padded = track.pad_to(target);
            let finalized = track.writer.finalize();
            if padded.is_err() || finalized.is_err() {
                error = Some(TrackError::Disk);
            }
            if finalized.is_ok() && track.received > 0 {
                manifest.tracks.push(TrackInfo {
                    display_name: names.get(&track.id).cloned(),
                    participant_id: track.id,
                    file: track.file,
                    received_samples: track.received,
                    coverage: track.coverage,
                });
            }
        }
        let partial = self.directory.join("tracks.json.partial");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|_| TrackError::Disk)?;
        serde_json::to_writer_pretty(&mut file, &manifest).map_err(|_| TrackError::Disk)?;
        file.flush().map_err(|_| TrackError::Disk)?;
        drop(file);
        fs::rename(partial, self.directory.join(MANIFEST_FILE)).map_err(|_| TrackError::Disk)?;
        match error {
            Some(error) => Err(error),
            None => Ok(manifest.tracks.len()),
        }
    }
}

fn max_samples(count: usize) -> u64 {
    MAX_TRACK_SAMPLES.min(MAX_OUTPUT_BYTES / (2 * count.max(1) as u64))
}

impl Track {
    fn create(directory: &Path, index: usize, id: String) -> hound::Result<Self> {
        let file = format!("participant-{:03}.wav", index + 1);
        let path = directory.join(&file);
        let output = OpenOptions::new().write(true).create_new(true).open(path)?;
        let writer = WavWriter::new(
            BufWriter::new(output),
            WavSpec {
                channels: 1,
                sample_rate: SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            },
        )?;
        Ok(Self {
            id,
            file,
            writer,
            written: 0,
            received: 0,
            coverage: None,
        })
    }

    fn pad_to(&mut self, target: u64) -> hound::Result<()> {
        while self.written < target {
            let count = (target - self.written).min(4096) as u32;
            let mut writer = self.writer.get_i16_writer(count);
            for _ in 0..count {
                writer.write_sample(0);
            }
            writer.flush()?;
            self.written += u64::from(count);
        }
        Ok(())
    }

    fn write(&mut self, offset: u64, pcm: &[u8], observed_ms: u64) -> hound::Result<()> {
        let pcm_samples = (pcm.len() / 2) as u64;
        let overlap = self.written.saturating_sub(offset).min(pcm_samples);
        let count = pcm_samples - overlap;
        if count == 0 {
            return Ok(());
        }
        self.pad_to(offset)?;
        let pcm_start = self.written;
        let mut writer = self.writer.get_i16_writer(count as u32);
        for pair in pcm[(overlap * 2) as usize..].as_chunks::<2>().0 {
            writer.write_sample(i16::from_le_bytes(*pair));
        }
        writer.flush()?;
        self.written += count;
        self.received += count;
        if let Some(coverage) = &mut self.coverage {
            let previous_end = coverage.last_pcm_end_sample;
            if previous_end < pcm_start {
                coverage.internal_gap_samples += pcm_start - previous_end;
                coverage.internal_gap_count += 1;
                if coverage.internal_gaps.len() < MAX_REPORTED_GAPS {
                    coverage.internal_gaps.push(SampleInterval {
                        start_sample: previous_end,
                        end_sample: pcm_start,
                    });
                }
            }
            coverage.last_pcm_end_sample = self.written;
            coverage.first_observed_ms = coverage.first_observed_ms.min(observed_ms);
            coverage.last_observed_ms = coverage.last_observed_ms.max(observed_ms);
        } else {
            self.coverage = Some(TrackCoverage {
                first_observed_ms: observed_ms,
                last_observed_ms: observed_ms,
                first_pcm_sample: pcm_start,
                last_pcm_end_sample: self.written,
                internal_gap_samples: 0,
                internal_gap_count: 0,
                internal_gaps: Vec::new(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::Observation;

    fn frame(id: &str, source_ms: i64, elapsed_ms: u64, epoch: u64, value: i16) -> Packet {
        Packet {
            bot_id: "bot_test".into(),
            participant_id: id.into(),
            provider_ms: source_ms,
            observation: Observation { elapsed_ms, epoch },
            pcm: value.to_le_bytes().repeat(160),
        }
    }

    #[test]
    fn preserves_named_participants_overlap_silence_and_pause(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut tracks = Tracks::new(directory.path().into());
        let first_id = "a/opaque";
        let second_id = "b/opaque";
        assert!(tracks.write(frame(first_id, 100, 50, 0, 1)).is_ok());
        assert!(tracks.write(frame(second_id, 100, 51, 0, 2)).is_ok());
        assert!(tracks.write(frame(first_id, 1_000, 150, 1, 3)).is_ok());
        assert!(tracks.write(frame(first_id, 101, 52, 0, 4)).is_ok());
        let names = HashMap::from([
            (first_id.into(), "Same".into()),
            (second_id.into(), "Same".into()),
        ]);
        let clock = ClockSnapshot {
            elapsed_ms: 180,
            origin_unix_micros: Some(1_793_750_400_000_000),
            excluded_pause_ms: 850,
        };
        assert_eq!(tracks.finish(&names, clock, 3).ok(), Some(2));
        let manifest: Manifest =
            serde_json::from_reader(File::open(directory.path().join(MANIFEST_FILE))?)?;
        assert_eq!(manifest.duration_ms, 180);
        assert_eq!(manifest.tracks[0].participant_id, first_id);
        assert_eq!(manifest.tracks[1].participant_id, second_id);
        assert_eq!(manifest.tracks[0].display_name.as_deref(), Some("Same"));
        assert_eq!(manifest.tracks[1].display_name.as_deref(), Some("Same"));
        let timing = manifest.timing.as_ref().unwrap();
        assert_eq!(timing.origin_unix_ms, Some(1_793_750_400_000));
        assert_eq!(timing.origin_unix_micros, Some(1_793_750_400_000_000));
        assert_eq!(timing.local_elapsed_ms, 180);
        assert_eq!(timing.excluded_pause_ms, 850);
        assert_eq!(timing.queue_dropped_packets, 3);
        assert_eq!(timing.provider_clock_reanchors, 0);
        let first_coverage = manifest.tracks[0].coverage.as_ref().unwrap();
        assert_eq!(first_coverage.first_observed_ms, 50);
        assert_eq!(first_coverage.last_observed_ms, 150);
        assert_eq!(first_coverage.first_pcm_sample, 800);
        assert_eq!(first_coverage.last_pcm_end_sample, 2_560);
        assert_eq!(first_coverage.internal_gap_samples, 1_440);
        assert_eq!(first_coverage.internal_gap_count, 1);
        assert_eq!(first_coverage.internal_gaps[0].start_sample, 960);
        assert_eq!(first_coverage.internal_gaps[0].end_sample, 2_400);
        let second_coverage = manifest.tracks[1].coverage.as_ref().unwrap();
        assert_eq!(second_coverage.first_observed_ms, 51);
        assert_eq!(second_coverage.first_pcm_sample, 800);
        assert_eq!(second_coverage.last_pcm_end_sample, 960);
        assert_eq!(second_coverage.internal_gap_count, 0);
        let samples = |index: usize| -> Result<Vec<i16>, hound::Error> {
            hound::WavReader::open(directory.path().join(&manifest.tracks[index].file))?
                .into_samples()
                .collect()
        };
        let first = samples(0)?;
        let second = samples(1)?;
        assert_eq!(first.len(), second.len());
        assert_eq!(first[800..960], [1; 160]);
        assert_eq!(second[800..960], [2; 160]);
        assert_eq!(first[960..2400], [0; 1440]);
        assert_eq!(first[2400..2560], [3; 160]);
        Ok(())
    }

    #[test]
    fn historical_manifest_does_not_require_coverage() -> serde_json::Result<()> {
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "sample_rate": 16000,
            "duration_ms": 20,
            "tracks": [{
                "participant_id": "participant",
                "display_name": "Person",
                "file": "participant-001.wav",
                "received_samples": 320
            }]
        }))?;
        assert!(manifest.timing.is_none());
        assert!(manifest.tracks[0].coverage.is_none());
        Ok(())
    }
}

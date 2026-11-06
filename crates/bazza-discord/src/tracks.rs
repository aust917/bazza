use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
};

use anyhow::{ensure, Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub const SAMPLE_RATE: u32 = 48_000;
pub const TICK_MS: u64 = 20;
pub const SAMPLES_PER_TICK: usize = 960;
pub const MAX_TRACKS: usize = 16;

pub struct Tick {
    pub index: u64,
    pub frames: Vec<(u64, Vec<i16>)>,
}

#[derive(Deserialize, Serialize)]
pub struct Manifest {
    pub schema_version: u8,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub finish_reason: String,
    pub dropped_ticks: u64,
    pub unmapped_frames: u64,
    pub invalid_frames: u64,
    pub tracks: Vec<TrackInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<CaptureTimeline>,
}

/// Wall-clock anchor and receiver-side monotonic observations for the WAV timeline.
#[derive(Debug, Deserialize, Serialize)]
pub struct CaptureTimeline {
    pub attempt_started_unix_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_epoch_unix_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_epoch_elapsed_us: Option<i64>,
    pub capture_closed_elapsed_us: u64,
    pub pause_seen: bool,
    pub spans: Vec<CaptureSpan>,
    pub writer_drops: Vec<TickRange>,
    pub truncated: bool,
}

/// One run of voice callbacks, with WAV tick bounds and callback times.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CaptureSpan {
    pub start_tick: u64,
    pub end_tick: u64,
    pub first_elapsed_us: u64,
    pub last_elapsed_us: u64,
}

/// Half-open interval of 20 ms WAV ticks.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct TickRange {
    pub start_tick: u64,
    pub end_tick: u64,
}

#[derive(Deserialize, Serialize)]
pub struct TrackInfo {
    pub user_id: String,
    pub display_name: String,
    pub file: String,
    pub received_frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_received_tick: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_received_tick: Option<u64>,
}

struct Track {
    writer: WavWriter<BufWriter<File>>,
    next_tick: u64,
    received_frames: u64,
    first_received_tick: u64,
}

impl Track {
    fn pad_to(&mut self, tick: u64) -> Result<()> {
        ensure!(
            tick >= self.next_tick,
            "received a repeated or out-of-order tick"
        );
        for _ in self.next_tick..tick {
            for _ in 0..SAMPLES_PER_TICK {
                self.writer.write_sample(0i16)?;
            }
        }
        self.next_tick = tick;
        Ok(())
    }
}

pub struct Tracks {
    dir: PathBuf,
    by_user: BTreeMap<u64, Track>,
}

impl Tracks {
    pub fn collect(dir: PathBuf, mut rx: mpsc::Receiver<Tick>) -> Result<Self> {
        let mut output = Self {
            dir,
            by_user: BTreeMap::new(),
        };
        while let Some(tick) = rx.blocking_recv() {
            for (id, frame) in tick.frames {
                ensure!(
                    frame.len() == SAMPLES_PER_TICK,
                    "incorrect mono frame length"
                );
                if !output.by_user.contains_key(&id) {
                    ensure!(
                        output.by_user.len() < MAX_TRACKS,
                        "too many speakers for one Discord recording"
                    );
                    let path = output.dir.join(format!("{id}.wav"));
                    let file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .with_context(|| format!("create {}", path.display()))?;
                    let writer = WavWriter::new(
                        BufWriter::new(file),
                        WavSpec {
                            channels: 1,
                            sample_rate: SAMPLE_RATE,
                            bits_per_sample: 16,
                            sample_format: SampleFormat::Int,
                        },
                    )?;
                    output.by_user.insert(
                        id,
                        Track {
                            writer,
                            next_tick: 0,
                            received_frames: 0,
                            first_received_tick: tick.index,
                        },
                    );
                }
                let track = output
                    .by_user
                    .get_mut(&id)
                    .expect("track was created above");
                track.pad_to(tick.index)?;
                for sample in frame {
                    track.writer.write_sample(sample)?;
                }
                track.next_tick += 1;
                track.received_frames += 1;
            }
        }
        Ok(output)
    }

    pub fn finish(self, end_tick: u64) -> Result<Vec<TrackInfo>> {
        self.by_user
            .into_iter()
            .map(|(id, mut track)| {
                let last_received_tick = track.next_tick.checked_sub(1);
                track.pad_to(end_tick)?;
                track.writer.finalize()?;
                Ok(TrackInfo {
                    user_id: id.to_string(),
                    display_name: format!("Discord {id}"),
                    file: format!("{id}.wav"),
                    received_frames: track.received_frames,
                    first_received_tick: Some(track.first_received_tick),
                    last_received_tick,
                })
            })
            .collect()
    }
}

pub fn save_manifest(dir: &std::path::Path, manifest: &Manifest) -> Result<()> {
    let temp = dir.join("tracks.json.part");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, dir.join("tracks.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_frames_keep_identity_and_shared_timeline() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (tx, rx) = mpsc::channel(3);
        let frame = |value| vec![value; SAMPLES_PER_TICK];
        tx.try_send(Tick {
            index: 1,
            frames: vec![(42, frame(100))],
        })?;
        tx.try_send(Tick {
            index: 2,
            frames: vec![(42, frame(200)), (43, frame(-200))],
        })?;
        tx.try_send(Tick {
            index: 4,
            frames: vec![(43, frame(-400))],
        })?;
        drop(tx);

        let tracks = Tracks::collect(dir.path().to_owned(), rx)?.finish(6)?;
        assert_eq!(tracks.len(), 2);
        for (track, expected) in tracks
            .iter()
            .zip([[0, 100, 200, 0, 0, 0], [0, 0, -200, 0, -400, 0]])
        {
            assert_eq!(track.received_frames, 2);
            let pcm = hound::WavReader::open(dir.path().join(&track.file))?
                .into_samples::<i16>()
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(pcm.len(), 6 * SAMPLES_PER_TICK);
            for (block, value) in pcm.chunks_exact(SAMPLES_PER_TICK).zip(expected) {
                assert_eq!(block, frame(value));
            }
        }
        Ok(())
    }
}

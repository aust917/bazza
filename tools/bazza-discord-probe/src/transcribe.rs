use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, ErrorKind, Read, Write};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use bazza_asr::whisper::TranscribeOpts;
use bazza_asr::WhisperEngine;
use bazza_capture::dsp::Resampler;
use serde::Serialize;
use tempfile::NamedTempFile;

use crate::tracks::{Manifest, MAX_TRACKS, SAMPLES_PER_TICK, SAMPLE_RATE, TICK_MS};

const ASR_HZ: u32 = 16_000;
const MAX_DURATION_MS: u64 = 600_000;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Serialize)]
struct NamedSegment {
    user_id: String,
    display_name: String,
    start_ms: i64,
    end_ms: i64,
    text: String,
}

#[derive(Serialize)]
struct Transcript {
    schema_version: u32,
    segments: Vec<NamedSegment>,
}

pub fn run(dir: &Path, model: &Path, lang: Option<&str>) -> Result<()> {
    let dir = dir.canonicalize().context("open the recording directory")?;
    let output = dir.join("transcript.json");
    match fs::symlink_metadata(&output) {
        Ok(_) => bail!("transcript.json already exists"),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("check transcript.json"),
    }

    let mut manifest = Vec::new();
    open_file(&dir.join("tracks.json"))?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut manifest)
        .context("read tracks.json")?;
    ensure!(
        manifest.len() as u64 <= MAX_MANIFEST_BYTES,
        "tracks.json is too large"
    );
    let manifest: Manifest = serde_json::from_slice(&manifest).context("parse tracks.json")?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported tracks.json version"
    );
    ensure!(
        manifest.sample_rate == SAMPLE_RATE,
        "unexpected track sample rate"
    );
    ensure!(
        (TICK_MS..=MAX_DURATION_MS).contains(&manifest.duration_ms)
            && manifest.duration_ms.is_multiple_of(TICK_MS),
        "invalid recording duration"
    );
    ensure!(
        (1..=MAX_TRACKS).contains(&manifest.tracks.len()),
        "invalid number of tracks in tracks.json"
    );
    let duration_ms = manifest.duration_ms as i64;
    let expected_frames = (manifest.duration_ms / TICK_MS) as u32 * SAMPLES_PER_TICK as u32;

    let mut user_ids = HashSet::new();
    for track in &manifest.tracks {
        let id: u64 = track.user_id.parse().context("invalid Discord user ID")?;
        ensure!(
            id != 0 && track.user_id == id.to_string(),
            "invalid Discord user ID"
        );
        ensure!(
            user_ids.insert(id),
            "duplicate Discord user ID in tracks.json"
        );
        ensure!(
            track.file == format!("{id}.wav"),
            "invalid WAV filename in tracks.json"
        );
    }

    let engine = WhisperEngine::load(model, false).context("load local Whisper model")?;
    let opts = TranscribeOpts {
        lang,
        is_final: true,
        drop_no_speech_above: Some(0.6),
        ..Default::default()
    };
    let mut segments = Vec::new();
    for track in manifest.tracks {
        let pcm = read_track(&dir.join(&track.file), expected_frames)
            .with_context(|| format!("read track {}", track.file))?;
        let result = engine
            .transcribe_opts(&pcm, &opts)
            .with_context(|| format!("transcribe track {}", track.file))?;
        for caption in result.captions {
            let start_ms = caption.start_ms.clamp(0, duration_ms);
            let end_ms = caption.end_ms.clamp(0, duration_ms);
            if end_ms > start_ms {
                segments.push(NamedSegment {
                    user_id: track.user_id.clone(),
                    display_name: track.display_name.clone(),
                    start_ms,
                    end_ms,
                    text: caption.text,
                });
            }
        }
    }
    segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));

    let mut temp = NamedTempFile::new_in(&dir).context("create temporary transcript")?;
    let transcript = Transcript {
        schema_version: 1,
        segments,
    };
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, &transcript).context("write transcript")?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&output)
        .context("save transcript.json")?;
    println!(
        "Saved {} named segments to {}",
        transcript.segments.len(),
        output.display()
    );
    Ok(())
}

fn open_file(path: &Path) -> Result<File> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "expected a regular file: {}",
        path.display()
    );
    File::open(path).with_context(|| format!("open {}", path.display()))
}

fn read_track(path: &Path, expected_frames: u32) -> Result<Vec<f32>> {
    let file = open_file(path)?;
    let bytes = file.metadata()?.len();
    ensure!(
        bytes <= u64::from(expected_frames) * 2 + 65_536,
        "WAV file is too large"
    );
    let mut reader = hound::WavReader::new(std::io::BufReader::new(file))?;
    let spec = reader.spec();
    ensure!(
        spec == hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
        "expected 48 kHz mono PCM16 WAV"
    );
    ensure!(
        reader.duration() == expected_frames,
        "WAV duration differs from tracks.json"
    );
    let asr_frames = (expected_frames as usize).div_ceil((SAMPLE_RATE / ASR_HZ) as usize);
    let mut pcm = Vec::with_capacity(asr_frames);
    let mut input = Vec::with_capacity(8192);
    let mut resampler = Resampler::new(SAMPLE_RATE, ASR_HZ);
    let mut samples = reader.samples::<i16>();
    loop {
        input.clear();
        for sample in samples.by_ref().take(8192) {
            input.push(f32::from(sample?) / 32_768.0);
        }
        if input.is_empty() {
            break;
        }
        pcm.extend(resampler.process(&input));
    }
    // Drain the filter and trim the padding so each track stays on its original clock.
    pcm.extend(resampler.process(&[0.0; 64]));
    pcm.truncate(asr_frames);
    Ok(pcm)
}

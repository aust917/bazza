//! `bazza-diarize --seg <onnx> --emb <onnx> --wav <wav>` (legacy enrollment)
//! or `--backend nemotron --nemotron <gguf> --nemotron-runtime <dir> --emb <onnx> --wav <wav>`.
//! Optional: `[--max-embed-secs 1..120] [--start-ms A --end-ms B] [--threads T]`.
//! Turn timestamps are relative to `--start-ms` when a window is given.
//! Reads a WAV (any rate/channels), diarizes, computes one voice embedding per
//! cluster, prints `DiarizeOutput` JSON on stdout. Progress lines `progress <0..1>`
//! go to stderr. Exit code 0 even on engine errors (error is in the JSON).

mod engine;
mod nemotron;
mod voiceprints;

use anyhow::{anyhow, Context, Result};
use bazza_diarize::{DiarizeBackend, DiarizeOutput, DiarizeRequest, Turn};
use bazza_diarize::voice_evidence::{
    plan_coherence, SampleInterval, SourceInterval, VoiceEvidence,
    EVIDENCE_VERSION,
};
use bazza_diarize::voice_policy::{planner_config, POLICY_ID};
use std::path::Path;

fn parse_args() -> Result<DiarizeRequest> {
    parse_options(std::env::args().skip(1))
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<DiarizeRequest> {
    let mut backend = DiarizeBackend::Pyannote;
    let (mut nemotron_model, mut nemotron_runtime) = (None, None);
    let mut seg = None;
    let mut emb = None;
    let mut wav = None;
    let mut max_embed_secs = 60usize;
    let (mut start_ms, mut end_ms, mut threads) = (None, None, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--backend" => backend = match it.next().as_deref() {
                Some("pyannote") => DiarizeBackend::Pyannote,
                Some("nemotron") => DiarizeBackend::Nemotron,
                _ => return Err(anyhow!("--backend must be pyannote or nemotron")),
            },
            "--nemotron" => nemotron_model = it.next(),
            "--nemotron-runtime" => nemotron_runtime = it.next(),
            "--seg" => seg = it.next(),
            "--emb" => emb = it.next(),
            "--wav" => wav = it.next(),
            "--max-embed-secs" => {
                max_embed_secs = it.next().and_then(|v| v.parse().ok())
                    .filter(|v| (1..=120).contains(v))
                    .ok_or_else(|| anyhow!("--max-embed-secs must be between 1 and 120"))?
            }
            "--start-ms" => start_ms = it.next().and_then(|v| v.parse().ok()),
            "--end-ms" => end_ms = it.next().and_then(|v| v.parse().ok()),
            "--threads" => threads = it.next().and_then(|v| v.parse().ok()),
            other => return Err(anyhow!("unknown argument {other}")),
        }
    }
    match backend {
        DiarizeBackend::Pyannote if seg.is_none() => return Err(anyhow!("--seg required")),
        DiarizeBackend::Nemotron if nemotron_model.is_none() || nemotron_runtime.is_none() =>
            return Err(anyhow!("--nemotron and --nemotron-runtime required")),
        _ => {}
    }
    Ok(DiarizeRequest {
        backend,
        nemotron_model,
        nemotron_runtime,
        seg_model: seg.unwrap_or_default(),
        emb_model: emb.ok_or_else(|| anyhow!("--emb required"))?,
        wav: wav.ok_or_else(|| anyhow!("--wav required"))?,
        max_embed_secs,
        start_ms,
        end_ms,
        threads,
    })
}

/// Read only the native frames needed by a 16 kHz window. Interpolation stays on
/// the full recording's sample grid, so shard boundaries match a full-file read.
fn read_wav_16k(path: &Path, start_ms: Option<i64>, end_ms: Option<i64>) -> Result<Vec<f32>> {
    let reader =
        hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    read_wav_16k_window(reader, start_ms, end_ms)
}

fn read_wav_16k_window<R: std::io::Read + std::io::Seek>(
    reader: hound::WavReader<R>, start_ms: Option<i64>, end_ms: Option<i64>,
) -> Result<Vec<f32>> {
    let (start, end) = wav_sample_range(&reader, start_ms, end_ms)?;
    read_wav_16k_frames(reader, start, end)
}

fn wav_sample_range<R: std::io::Read>(
    reader: &hound::WavReader<R>, start_ms: Option<i64>, end_ms: Option<i64>,
) -> Result<(usize, usize)> {
    let spec = reader.spec();
    let ch = spec.channels as usize;
    if ch == 0 || spec.sample_rate == 0 {
        return Err(anyhow!("invalid WAV channel count or sample rate"));
    }
    let ratio = spec.sample_rate as f64 / 16_000.0;
    let native_frames = (reader.len() as usize).div_ceil(ch);
    let output_frames = if spec.sample_rate == 16_000 { native_frames } else { (native_frames as f64 / ratio) as usize };
    let frame_at = |ms: i64| (ms.max(0) as u64).saturating_mul(16).min(output_frames as u64) as usize;
    let start = frame_at(start_ms.unwrap_or(0));
    let end = end_ms.map(frame_at).unwrap_or(output_frames).max(start);
    Ok((start, end))
}

fn read_wav_16k_frames<R: std::io::Read + std::io::Seek>(
    mut reader: hound::WavReader<R>, start: usize, end: usize,
) -> Result<Vec<f32>> {
    let spec = reader.spec();
    let ch = spec.channels as usize;
    let ratio = spec.sample_rate as f64 / 16_000.0;
    let native_frames = (reader.len() as usize).div_ceil(ch);
    if start == end { return Ok(Vec::new()); }
    let native_start = (start as f64 * ratio).floor() as usize;
    if spec.bits_per_sample == 24 {
        // Hound's seek uses bits_per_sample / 8, but supported 24-bit WAVs
        // may use four-byte containers. Stream past the prefix in this case;
        // decoding remains bounded in memory and retains padded-WAV support.
        for sample in reader.samples::<i32>().take(native_start * ch) { sample?; }
    } else {
        reader.seek(native_start as u32)?;
    }
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = ((1u64 << (spec.bits_per_sample - 1)) - 1) as f32;
            interpolate_window(reader.into_samples::<i32>().map(|s| s.map(|v| v as f32 / max)), ch, native_frames,
                native_start, start..end, ratio, spec.sample_rate == 16_000)
        }
        hound::SampleFormat::Float => interpolate_window(reader.into_samples::<f32>(), ch, native_frames,
            native_start, start..end, ratio, spec.sample_rate == 16_000),
    }
}

fn interpolate_window(
    mut samples: impl Iterator<Item = hound::Result<f32>>, channels: usize, native_frames: usize,
    native_start: usize, window: std::ops::Range<usize>, ratio: f64, identity: bool,
) -> Result<Vec<f32>> {
    let (start, end) = (window.start, window.end);
    let mut decoded_frames = native_start;
    let mut next_frame = || -> Result<Option<f32>> {
        let mut sum = 0.0;
        let mut present = false;
        for _ in 0..channels {
            match samples.next() {
                Some(value) => { sum += value?; present = true; }
                None => break,
            }
        }
        if !present && decoded_frames < native_frames {
            return Err(anyhow!("WAV audio ends before its declared frame count"));
        }
        if present { decoded_frames += 1; }
        Ok(present.then_some(sum / channels as f32))
    };
    let mut out = Vec::with_capacity(end - start);
    if identity {
        for _ in start..end {
            if let Some(value) = next_frame()? { out.push(value); } else { break; }
        }
        return Ok(out);
    }
    let mut native_index = native_start;
    let mut a = next_frame()?.unwrap_or(0.0);
    let mut b = next_frame()?.unwrap_or(a);
    for i in start..end {
        let pos = i as f64 * ratio;
        let j = pos.floor() as usize;
        while native_index < j {
            a = b;
            b = next_frame()?.unwrap_or(a);
            native_index += 1;
        }
        let frac = (pos - j as f64) as f32;
        out.push(a + (b - a) * frac);
    }
    Ok(out)
}

fn cluster_audio(pcm: &[f32], turns: &[Turn], cluster: i32, max_secs: usize) -> Vec<f32> {
    let cap = max_secs * 16_000;
    let mut out = Vec::new();
    for t in turns.iter().filter(|t| t.cluster == cluster) {
        let a = ((t.start_ms.max(0) as usize) * 16).min(pcm.len());
        let b = ((t.end_ms.max(0) as usize) * 16).min(pcm.len());
        if b > a {
            out.extend_from_slice(&pcm[a..b]);
        }
        if out.len() >= cap {
            out.truncate(cap);
            break;
        }
    }
    out
}

/// A model can return NaN/Inf on degraded input. Those values serialize as null,
/// breaking the typed sidecar contract. Omit only its voiceprint, not its turns.
fn checked_normalized_embedding(embedding: &[f32], expected_dim: usize) -> Option<Vec<f32>> {
    if expected_dim == 0
        || embedding.len() != expected_dim
        || !embedding.iter().all(|value| value.is_finite())
    {
        return None;
    }
    // Accumulate in f64 so finite scaled model output cannot overflow/underflow
    // the f32 squared norm and turn normalization into zeros or nonfinite values.
    let squared_norm = embedding
        .iter()
        .map(|value| (*value as f64).powi(2))
        .sum::<f64>();
    if !squared_norm.is_finite() || squared_norm <= 0.0 {
        return None;
    }
    let norm = squared_norm.sqrt();
    let normalized: Vec<f32> = embedding
        .iter()
        .map(|value| (*value as f64 / norm) as f32)
        .collect();
    let normalized_squared_norm = normalized
        .iter()
        .map(|value| (*value as f64).powi(2))
        .sum::<f64>();
    if normalized.len() != expected_dim
        || !normalized.iter().all(|value| value.is_finite())
        || !normalized_squared_norm.is_finite()
        || normalized_squared_norm <= 0.0
    {
        return None;
    }
    Some(normalized)
}

fn run(req: &DiarizeRequest) -> Result<DiarizeOutput> {
    if req.backend == DiarizeBackend::Nemotron {
        return run_nemotron(req);
    }
    let pcm = read_wav_16k(Path::new(&req.wav), req.start_ms, req.end_ms)?;
    let t0 = std::time::Instant::now();
    let threads = req.threads.unwrap_or_else(engine::default_threads);
    let mut eng = engine::DiarizeEngine::load_with(
        Path::new(&req.seg_model),
        Path::new(&req.emb_model),
        threads,
        "cpu",
    )?;
    let turns = eng.diarize(&pcm, |p| eprintln!("progress {p:.3}"))?;
    eprintln!(
        "timing: diarize {:.1}s ({} threads, {} turns)",
        t0.elapsed().as_secs_f32(),
        eng.threads,
        turns.len()
    );
    let t1 = std::time::Instant::now();
    let mut out = DiarizeOutput {
        turns: turns.clone(),
        embedding_dim: eng.embedding_dim,
        ..Default::default()
    };
    let mut clusters: Vec<i32> = turns.iter().map(|t| t.cluster).collect();
    clusters.sort_unstable();
    clusters.dedup();
    for c in clusters {
        let audio = cluster_audio(&pcm, &turns, c, req.max_embed_secs);
        if audio.len() < 16_000 {
            continue;
        }
        match eng.embed(&audio) {
            Ok(e) => {
                match checked_normalized_embedding(&e, eng.embedding_dim) {
                    Some(embedding) => { out.embeddings.insert(c, embedding); }
                    None => eprintln!("embed cluster {c}: invalid voice embedding (expected {} finite nonzero dimensions); keeping turns without a voiceprint", eng.embedding_dim),
                }
            }
            Err(e) => eprintln!("embed cluster {c}: {e}"),
        }
    }
    eprintln!(
        "timing: embeddings {:.1}s ({} clusters)",
        t1.elapsed().as_secs_f32(),
        out.embeddings.len()
    );
    Ok(out)
}

/// Keep one native stream for the whole recording; independent shards would
/// discard Nemotron's cross-window speaker memory. Only 15 seconds of PCM and
/// up to max_embed_secs of one cluster's TitaNet input are resident at a time.
fn run_nemotron(req: &DiarizeRequest) -> Result<DiarizeOutput> {
    let wav = Path::new(&req.wav);
    let reader = hound::WavReader::open(wav)?;
    let (start, end) = wav_sample_range(&reader, req.start_ms, req.end_ms)?;
    drop(reader);
    let mut cursor = start;
    let mut engine = nemotron::NemotronEngine::load(
        Path::new(req.nemotron_runtime.as_deref().context("Nemotron runtime required")?),
        Path::new(req.nemotron_model.as_deref().context("Nemotron model required")?),
    )?;
    let started = std::time::Instant::now();
    let turns = engine.diarize_chunks(end - start, || {
        if cursor == end { return Ok(Vec::new()); }
        let chunk_end = cursor.saturating_add(15 * 16_000).min(end);
        let reader = hound::WavReader::open(wav)
            .map_err(|e| engine::DiarizeError::Engine(format!("open WAV: {e}")))?;
        let pcm = read_wav_16k_frames(reader, cursor, chunk_end)
            .map_err(|e| engine::DiarizeError::Engine(format!("read WAV: {e}")))?;
        cursor = chunk_end;
        Ok(pcm)
    }, |p| eprintln!("progress {p:.3}"))?;
    eprintln!("timing: Nemotron diarize {:.1}s ({} turns, {} committed frames)", started.elapsed().as_secs_f32(), turns.len(), engine.consumed_frame_count());
    // Release the large diarization model before loading the unchanged TitaNet.
    drop(engine);
    let mut out = DiarizeOutput { turns, ..Default::default() };
    let mut voice = match engine::VoiceprintEngine::load(
        Path::new(&req.emb_model), req.threads.unwrap_or_else(engine::default_threads),
    ) {
        Ok(voice) => voice,
        Err(error) => {
            eprintln!("voiceprint initialization failed: {error}; keeping anonymous Nemotron turns");
            return Ok(out);
        }
    };
    out.embedding_dim = voice.embedding_dim;
    let catalog = bazza_models::Catalog::builtin();
    let spec = catalog.get("titanet-small").context("TitaNet catalog entry required")?;
    let mut evidence = VoiceEvidence {
        version: EVIDENCE_VERSION,
        model_id: spec.id.clone(),
        model_sha256: spec.sha256.clone(),
        policy_id: if req.max_embed_secs == 60 { POLICY_ID.to_string() } else { format!("{POLICY_ID}/primary{}s", req.max_embed_secs) },
        clusters: Default::default(),
    };
    let clusters: std::collections::BTreeSet<_> = out.turns.iter().map(|t| t.cluster).collect();
    let started = std::time::Instant::now();
    for cluster in clusters {
        let source = cluster_sources(&out.turns, cluster, (end - start) as u64);
        let plan = match plan_coherence(&source, (end - start) as u64, planner_config()) {
            Ok(plan) => plan,
            Err(error) => { eprintln!("voice evidence cluster {cluster}: {error}; keeping anonymous turns"); continue; }
        };
        let pool = match cluster_audio_from_wav(wav, (start, end), &out.turns, cluster, req.max_embed_secs) {
            Ok(pool) => pool,
            Err(error) => { eprintln!("voiceprint audio cluster {cluster}: {error}; keeping anonymous turns"); continue; }
        };
        let extracted = voiceprints::extract_cluster(
            &pool.samples, &pool.sources, &plan, (end - start) as u64, voice.embedding_dim,
            |span| {
                let a = start.checked_add(usize::try_from(span.start_sample).map_err(|e| e.to_string())?).ok_or("window start overflow")?;
                let b = start.checked_add(usize::try_from(span.end_sample).map_err(|e| e.to_string())?).ok_or("window end overflow")?;
                read_wav_16k_frames(hound::WavReader::open(wav).map_err(|e| e.to_string())?, a, b).map_err(|e| e.to_string())
            },
            |audio| voice.embed(audio).map_err(|e| e.to_string()),
        );
        eprintln!("voice evidence cluster {cluster}: query {:?}, {} scheduled windows", extracted.evidence.recovery.query_source, extracted.evidence.coherence_windows.len());
        if let Some(embedding) = extracted.embedding { out.embeddings.insert(cluster, embedding); }
        evidence.clusters.insert(cluster, extracted.evidence);
    }
    out.voice_evidence = Some(evidence);
    eprintln!("timing: embeddings {:.1}s ({} clusters)", started.elapsed().as_secs_f32(), out.embeddings.len());
    Ok(out)
}

struct SpeechPool {
    samples: Vec<f32>,
    /// Chronological source coordinates relative to the processed WAV window.
    sources: Vec<SampleInterval>,
}

fn cluster_sources(turns: &[Turn], cluster: i32, audio_samples: u64) -> Vec<SourceInterval> {
    let at = |ms: i64| (ms.max(0) as u64).saturating_mul(16).min(audio_samples);
    turns.iter().enumerate().filter(|(_, t)| t.cluster == cluster)
        .filter_map(|(index, turn)| {
            let (a, b) = (at(turn.start_ms), at(turn.end_ms));
            (b > a).then_some(SourceInterval {
                turn_index: index as u32,
                span: SampleInterval { start_sample: a, end_sample: b },
            })
        }).collect()
}

fn cluster_audio_from_wav(
    wav: &Path, range: (usize, usize), turns: &[Turn], cluster: i32, max_secs: usize,
) -> Result<SpeechPool> {
    let mut sources = Vec::new();
    let samples = cluster_audio_from_windows(range, turns, cluster, max_secs, |a, b| {
        let audio = read_wav_16k_frames(hound::WavReader::open(wav)?, a, b)?;
        if audio.len() != b - a { return Err(anyhow!("incomplete voiceprint audio")); }
        sources.push(SampleInterval { start_sample: (a - range.0) as u64, end_sample: (b - range.0) as u64 });
        Ok(audio)
    })?;
    Ok(SpeechPool { samples, sources })
}

fn cluster_audio_from_windows(
    range: (usize, usize), turns: &[Turn], cluster: i32, max_secs: usize,
    mut read: impl FnMut(usize, usize) -> Result<Vec<f32>>,
) -> Result<Vec<f32>> {
    let cap = max_secs.checked_mul(16_000).context("embedding audio cap overflow")?;
    let mut audio = Vec::new();
    for turn in turns.iter().filter(|turn| turn.cluster == cluster) {
        if audio.len() == cap { break; }
        let at = |ms: i64| range.0.saturating_add((ms.max(0) as u64)
            .saturating_mul(16).min(usize::MAX as u64) as usize).min(range.1);
        let a = at(turn.start_ms);
        let b = at(turn.end_ms).min(a.saturating_add(cap - audio.len()));
        if b > a {
            audio.extend(read(a, b)?);
        }
    }
    Ok(audio)
}

fn main() {
    let req = match parse_args() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let out = match run(&req) {
        Ok(o) => o,
        Err(e) => DiarizeOutput {
            error: Some(e.to_string()),
            ..Default::default()
        },
    };
    println!("{}", serde_json::to_string(&out).expect("json"));
}

#[cfg(test)]
mod wav_window_tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, SeekFrom};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    fn fixture(rate: u32, channels: u16, format: hound::SampleFormat, bits: u16, padded: bool, frames: usize) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: bits, sample_format: format };
        {
            let bytes_per_sample = if padded { 4 } else { bits / 8 };
            let mut writer = hound::WavWriter::new_with_spec_ex(&mut cursor, hound::WavSpecEx { spec, bytes_per_sample }).unwrap();
            for frame in 0..frames {
                for channel in 0..channels {
                    let value = ((frame as f32 * 0.071 + channel as f32 * 0.37).sin() * 0.7)
                        + ((frame % 17) as f32 / 200.0);
                    if format == hound::SampleFormat::Float { writer.write_sample(value).unwrap(); }
                    else { writer.write_sample((value * ((1u64 << (bits - 1)) - 1) as f32) as i32).unwrap(); }
                }
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    // Preserve the pre-window-reader algorithm as the compatibility oracle.
    fn full_reference(bytes: &[u8]) -> Vec<f32> {
        let reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let max = ((1u64 << (spec.bits_per_sample - 1)) - 1) as f32;
                reader.into_samples::<i32>().map(|s| s.unwrap() as f32 / max).collect()
            }
            hound::SampleFormat::Float => reader.into_samples::<f32>().map(|s| s.unwrap()).collect(),
        };
        let mono: Vec<f32> = samples.chunks(spec.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / spec.channels as f32).collect();
        if spec.sample_rate == 16_000 { return mono; }
        let ratio = spec.sample_rate as f64 / 16_000.0;
        (0..(mono.len() as f64 / ratio) as usize).map(|i| {
            let pos = i as f64 * ratio;
            let j = pos.floor() as usize;
            let frac = (pos - j as f64) as f32;
            let a = mono.get(j).copied().unwrap_or(0.0);
            let b = mono.get(j + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        }).collect()
    }

    #[test]
    fn ranges_match_full_file_reference_on_the_absolute_sample_grid() {
        let ranges = [(None, None), (Some(0), Some(0)), (Some(1), Some(9)),
            (Some(11), Some(47)), (Some(219), Some(9999)), (Some(-17), Some(20)),
            (Some(12_000), None), (Some(200), Some(30))];
        for rate in [8_000, 16_000, 22_050, 44_100, 48_000, 96_000] {
            for channels in [1, 2] {
                for (format, bits, padded) in [(hound::SampleFormat::Int, 8, false), (hound::SampleFormat::Int, 16, false),
                    (hound::SampleFormat::Int, 24, false), (hound::SampleFormat::Int, 24, true),
                    (hound::SampleFormat::Int, 32, false),
                    (hound::SampleFormat::Float, 32, false)] {
                    let bytes = fixture(rate, channels, format, bits, padded, rate as usize / 3 + 17);
                    let full = full_reference(&bytes);
                    let at_ms = |ms: i64| (ms.max(0) as usize * 16).min(full.len());
                    for (start_ms, end_ms) in ranges {
                        let start = at_ms(start_ms.unwrap_or(0));
                        let end = end_ms.map(at_ms).unwrap_or(full.len()).max(start);
                        let actual = read_wav_16k_window(hound::WavReader::new(Cursor::new(&bytes)).unwrap(), start_ms, end_ms).unwrap();
                        let expected = &full[start..end];
                        assert!(actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, b)| a.to_bits() == b.to_bits()),
                            "range mismatch: {rate} Hz, {channels} channels, {bits} bits, padded={padded}, {start_ms:?}..{end_ms:?}");
                    }
                }
            }
        }
    }

    struct CountReads { inner: Cursor<Vec<u8>>, bytes_read: Arc<AtomicUsize> }
    impl Read for CountReads {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = self.inner.read(buffer)?;
            self.bytes_read.fetch_add(count, Ordering::Relaxed);
            Ok(count)
        }
    }
    impl Seek for CountReads {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> { self.inner.seek(position) }
    }

    #[test]
    fn short_late_window_does_not_decode_the_rest_of_a_long_wav() {
        let bytes = fixture(48_000, 1, hound::SampleFormat::Int, 16, false, 60 * 48_000);
        let bytes_read = Arc::new(AtomicUsize::new(0));
        let reader = hound::WavReader::new(CountReads { inner: Cursor::new(bytes), bytes_read: bytes_read.clone() }).unwrap();
        let output = read_wav_16k_window(reader, Some(59_000), Some(59_010)).unwrap();
        assert_eq!(output.len(), 160);
        assert!(bytes_read.load(Ordering::Relaxed) < 4096, "read beyond the selected range");
    }

    #[test]
    fn streaming_chunks_keep_resampler_grid_and_submillisecond_tail() {
        for rate in [8_000, 16_000, 44_100, 48_000] {
            let bytes = fixture(rate, 2, hound::SampleFormat::Int, 16, false, rate as usize * 16 + 37);
            let full = full_reference(&bytes);
            let start = 123 * 16;
            let mut streamed = Vec::new();
            for a in (start..full.len()).step_by(15 * 16_000) {
                let b = (a + 15 * 16_000).min(full.len());
                streamed.extend(read_wav_16k_frames(hound::WavReader::new(Cursor::new(&bytes)).unwrap(), a, b).unwrap());
            }
            assert_eq!(streamed, full[start..], "{rate} Hz chunk boundary or final samples changed");
        }
    }

    #[test]
    fn truncated_wav_cannot_fabricate_a_complete_stream_by_repeating_its_tail() {
        for rate in [8_000, 16_000, 48_000] {
            let mut bytes = fixture(rate, 1, hound::SampleFormat::Int, 16, false, rate as usize);
            bytes.truncate(bytes.len() - 1000);
            let reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
            assert!(read_wav_16k_window(reader, None, None).is_err(), "truncated {rate} Hz WAV reported success");
        }
    }

    #[test]
    fn huge_time_ranges_clamp_without_overflow() {
        let bytes = fixture(16_000, 1, hound::SampleFormat::Int, 16, false, 100);
        let read = |start, end| read_wav_16k_window(hound::WavReader::new(Cursor::new(&bytes)).unwrap(), start, end).unwrap();
        assert!(read(Some(i64::MAX), None).is_empty());
        assert_eq!(read(None, Some(i64::MAX)).len(), 100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_remains_default_and_nemotron_requires_explicit_assets() {
        let parse = |args: &[&str]| parse_options(args.iter().map(|v| v.to_string()));
        assert_eq!(parse(&["--seg", "seg", "--emb", "tita", "--wav", "sample"]).unwrap().backend, DiarizeBackend::Pyannote);
        assert!(parse(&["--backend", "nemotron", "--emb", "tita", "--wav", "sample"]).is_err());
        let req = parse(&["--backend", "nemotron", "--nemotron", "model", "--nemotron-runtime", "runtime", "--emb", "tita", "--wav", "sample"]).unwrap();
        assert_eq!(req.backend, DiarizeBackend::Nemotron);
        for seconds in ["0", "121", "18446744073709551615", "bad"] {
            assert!(parse(&["--seg", "seg", "--emb", "tita", "--wav", "sample", "--max-embed-secs", seconds]).is_err());
        }
    }

    #[test]
    fn streamed_voiceprint_audio_matches_legacy_pool_without_reading_long_turns() {
        let pcm: Vec<_> = (0..200 * 16_000).map(|i| i as f32).collect();
        let range = (123 * 16, pcm.len() - 16 * 23);
        let turns = vec![
            Turn { start_ms: 0, end_ms: 500, cluster: 1 },
            Turn { start_ms: 700, end_ms: 1200, cluster: 2 },
            Turn { start_ms: 1700, end_ms: 199_999, cluster: 1 },
        ];
        let mut reads = Vec::new();
        let got = cluster_audio_from_windows(range, &turns, 1, 2, |a, b| {
            reads.push((a, b)); Ok(pcm[a..b].to_vec())
        }).unwrap();
        assert_eq!(got, cluster_audio(&pcm[range.0..range.1], &turns, 1, 2));
        assert_eq!(reads.iter().map(|(a,b)| b-a).sum::<usize>(), 32_000);
        assert!(reads.iter().all(|(a,b)| a >= &range.0 && b <= &range.1));
    }

    #[test]
    fn invalid_model_embeddings_are_not_normalized_or_serialized() {
        for embedding in [
            vec![f32::NAN, 1.0],
            vec![f32::INFINITY, 1.0],
            vec![f32::NEG_INFINITY, 1.0],
            vec![0.0, 0.0],
            vec![1.0],
            vec![],
        ] {
            assert!(checked_normalized_embedding(&embedding, 2).is_none());
        }
        assert!(checked_normalized_embedding(&[], 0).is_none());
        assert!(checked_normalized_embedding(&[1.0, 0.0], 0).is_none());
    }

    #[test]
    fn valid_scaled_embeddings_remain_finite_and_normalized() {
        let embedding = checked_normalized_embedding(&[3.0, 4.0], 2).unwrap();
        assert!((embedding[0] - 0.6).abs() < 1e-6);
        assert!((embedding[1] - 0.8).abs() < 1e-6);
        for input in [[f32::MAX, f32::MAX], [f32::MIN_POSITIVE, f32::MIN_POSITIVE]] {
            let embedding = checked_normalized_embedding(&input, 2).unwrap();
            assert!(embedding.iter().all(|value| value.is_finite()));
            let norm = embedding
                .iter()
                .map(|value| (*value as f64).powi(2))
                .sum::<f64>()
                .sqrt();
            assert!((norm - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn typed_output_roundtrip_retains_turns_without_invalid_voiceprints() {
        let raw = [
            vec![3.0, 4.0],
            vec![f32::NAN, 1.0],
            vec![0.0, 0.0],
            vec![f32::INFINITY, 1.0],
            vec![1.0],
        ];
        let turns: Vec<_> = raw
            .iter()
            .enumerate()
            .map(|(cluster, _)| Turn {
                start_ms: cluster as i64 * 2000,
                end_ms: cluster as i64 * 2000 + 1500,
                cluster: cluster as i32,
            })
            .collect();
        let mut output = DiarizeOutput {
            turns: turns.clone(),
            embedding_dim: 2,
            ..Default::default()
        };
        for (cluster, embedding) in raw.iter().enumerate() {
            if let Some(embedding) = checked_normalized_embedding(embedding, output.embedding_dim) {
                output.embeddings.insert(cluster as i32, embedding);
            }
        }
        let restored: DiarizeOutput =
            serde_json::from_str(&serde_json::to_string(&output).unwrap()).unwrap();
        assert_eq!(restored.turns, turns);
        assert_eq!(restored.embeddings.len(), 1);
        assert!(restored.embeddings.contains_key(&0));
        assert_eq!(restored, output);
    }
}

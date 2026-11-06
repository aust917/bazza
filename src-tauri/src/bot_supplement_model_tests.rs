//! Opt-in offline model comparison. Fixtures, expected text and output stay local.
//!
//! Required: BAZZA_TEST_MODEL, BAZZA_REFERENCE_WAV, BAZZA_MEDIA_WAV.
//! Expected text is read from sibling synthetic-corpus JSON (`text`), or from
//! BAZZA_MEDIA_EXPECTED_TEXT / BAZZA_REFERENCE_EXPECTED_TEXT plain-text paths.
//! BAZZA_SUPPLEMENT_TEST_OUTPUT must be below this checkout's .git/local-tests.
//! Optional: BAZZA_OBSERVED_REFERENCE_WAV tests a transformed playback copy;
//! BAZZA_MEDIA_SNR_DB defaults to -16, BAZZA_TEST_LANGUAGE defaults to ru.
//! No expected words or prompts are passed to the recognizer.

use bazza_asr::{whisper::TranscribeOpts, WhisperEngine};
use bazza_capture::{reference_residual::remove_reference, wav::read_mono_resampled, WavWriter};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    time::Instant,
};

const HZ: u32 = 16_000;

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name}"))
}

fn expected_text(wav: &Path, variable: &str) -> String {
    if let Some(path) = std::env::var_os(variable) {
        return fs::read_to_string(path).expect("read external expected text");
    }
    let metadata: Value = serde_json::from_slice(
        &fs::read(wav.with_extension("json"))
            .expect("supply a synthetic-corpus JSON or an explicit expected-text path"),
    )
    .expect("synthetic corpus JSON");
    metadata["text"]
        .as_str()
        .expect("synthetic corpus text")
        .to_owned()
}

fn sha256(path: &Path) -> String {
    let mut input = File::open(path).expect("open hash input");
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut bytes).expect("hash input read");
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    format!("{:x}", digest.finalize())
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('ё', "е")
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn wer(expected: &[String], actual: &[String]) -> f64 {
    let mut previous: Vec<_> = (0..=actual.len()).collect();
    let mut current = vec![0; actual.len() + 1];
    for (i, expected_word) in expected.iter().enumerate() {
        current[0] = i + 1;
        for (j, actual_word) in actual.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(expected_word != actual_word));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[actual.len()] as f64 / expected.len().max(1) as f64
}

fn recall(expected: &HashSet<String>, actual: &[String]) -> f64 {
    let actual: HashSet<_> = actual.iter().collect();
    expected.iter().filter(|word| actual.contains(word)).count() as f64
        / expected.len().max(1) as f64
}

fn rms(pcm: &[f32]) -> f64 {
    (pcm.iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / pcm.len().max(1) as f64)
        .sqrt()
}

fn mse(actual: &[f32], desired: &[f32]) -> f64 {
    actual
        .iter()
        .zip(desired)
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum::<f64>()
        / actual.len() as f64
}

fn save_wav(path: &Path, pcm: &[f32]) {
    let mut writer = WavWriter::create(path, HZ, 1).expect("create local comparison WAV");
    writer.write(pcm).expect("write local comparison WAV");
    writer.finish().expect("finish local comparison WAV");
}

fn output_directory() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("checkout root")
        .to_owned();
    let local_path = root.join(".git/local-tests");
    fs::create_dir_all(&local_path).expect("local test root");
    let local = local_path.canonicalize().expect("canonical test root");
    let output = std::env::var_os("BAZZA_SUPPLEMENT_TEST_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| local.join("20261106-bot-supplement-model/clean"));
    // Validate before creating any output directories. Refuse traversal and any
    // existing ancestor that redirects outside the local test tree.
    let relative = output
        .strip_prefix(&local_path)
        .or_else(|_| output.strip_prefix(&local))
        .expect("output must stay below .git/local-tests");
    assert!(
        relative.components().next().is_some(),
        "use a dedicated run directory"
    );
    assert!(relative
        .components()
        .all(|part| matches!(part, Component::Normal(_))));
    let mut existing = output.as_path();
    while !existing.exists() {
        existing = existing.parent().expect("existing output ancestor");
    }
    assert!(existing
        .canonicalize()
        .expect("canonical output ancestor")
        .starts_with(&local));
    fs::create_dir_all(&output).expect("create local output directory");
    assert!(output
        .canonicalize()
        .expect("canonical output")
        .starts_with(&local));
    output
}

#[test]
#[ignore = "requires a local Whisper model and saved synthetic speech fixtures; no network"]
fn offline_reference_residual_preserves_unique_media_speech() {
    let model = required_path("BAZZA_TEST_MODEL");
    let reference_path = required_path("BAZZA_REFERENCE_WAV");
    let media_path = required_path("BAZZA_MEDIA_WAV");
    let observed_path = std::env::var_os("BAZZA_OBSERVED_REFERENCE_WAV")
        .map(PathBuf::from)
        .unwrap_or_else(|| reference_path.clone());
    let transformed = observed_path != reference_path;
    let output = output_directory();
    let expected_media = expected_text(&media_path, "BAZZA_MEDIA_EXPECTED_TEXT");
    let expected_reference = expected_text(&reference_path, "BAZZA_REFERENCE_EXPECTED_TEXT");
    let language = std::env::var("BAZZA_TEST_LANGUAGE").unwrap_or_else(|_| "ru".to_owned());
    let media_snr = std::env::var("BAZZA_MEDIA_SNR_DB")
        .unwrap_or_else(|_| "-16".to_owned())
        .parse::<f64>()
        .expect("numeric media/reference RMS ratio in dB");
    assert!(
        (-30.0..=-6.0).contains(&media_snr),
        "use a quiet overlapping media control"
    );

    let mut reference = read_mono_resampled(&reference_path, HZ).expect("reference WAV");
    let mut played_reference = read_mono_resampled(&observed_path, HZ).expect("playback-copy WAV");
    let mut desired = read_mono_resampled(&media_path, HZ).expect("media WAV");
    let length = reference
        .len()
        .max(played_reference.len())
        .max(desired.len());
    assert!(
        (HZ as usize..=HZ as usize * 60).contains(&length),
        "use 1--60 second controls"
    );
    let reference_gain = 0.6
        / played_reference
            .iter()
            .map(|sample| f64::from(sample.abs()))
            .fold(0.0, f64::max)
            .max(1e-12);
    let media_gain = rms(&played_reference) * reference_gain * 10.0f64.powf(media_snr / 20.0)
        / rms(&desired).max(1e-12);
    for sample in &mut reference {
        *sample *= reference_gain as f32;
    }
    for sample in &mut played_reference {
        *sample *= reference_gain as f32;
    }
    for sample in &mut desired {
        *sample *= media_gain as f32;
    }
    reference.resize(length, 0.0);
    played_reference.resize(length, 0.0);
    desired.resize(length, 0.0);
    let original: Vec<_> = played_reference
        .iter()
        .zip(&desired)
        .map(|(a, b)| a + b)
        .collect();
    assert!(
        original.iter().all(|sample| sample.abs() < 0.999),
        "control mix must not clip"
    );

    let mut residual = original.clone();
    let mut evidence = Vec::new();
    let mut removed_windows = 0;
    let mut unchanged_windows = 0;
    for from in (0..length).step_by(HZ as usize * 5) {
        let to = (from + HZ as usize * 5).min(length);
        let result = remove_reference(&mut residual[from..to], &reference[from..to], HZ);
        if result.removed() {
            removed_windows += 1;
        } else {
            unchanged_windows += 1;
            assert_eq!(
                residual[from..to],
                original[from..to],
                "uncertain windows must remain untouched"
            );
        }
        evidence.push(
            json!({ "start_ms": from * 1000 / HZ as usize, "end_ms": to * 1000 / HZ as usize,
            "evidence": format!("{result:?}") }),
        );
    }
    let original_mse = mse(&original, &desired);
    let residual_mse = mse(&residual, &desired);
    let desired_power = rms(&desired).powi(2);
    save_wav(&output.join("original-mix.wav"), &original);
    save_wav(&output.join("suppression-residual.wav"), &residual);
    save_wav(&output.join("media-alone.wav"), &desired);

    let reference_words: HashSet<_> = words(&expected_reference).into_iter().collect();
    let media_words = words(&expected_media);
    let unique_media: HashSet<_> = media_words
        .iter()
        .filter(|word| word.chars().count() >= 4 && !reference_words.contains(*word))
        .cloned()
        .collect();
    assert!(
        unique_media.len() >= 8,
        "controls need eight distinct external-groundtruth media words"
    );
    let load_start = Instant::now();
    let engine = WhisperEngine::load(&model, true).expect("load local model");
    let model_load_ms = load_start.elapsed().as_millis();
    let mut recognitions = Vec::new();
    let mut scores = Vec::new();
    let expected_backend = std::env::var("BAZZA_TEST_EXPECT_BACKEND").ok();
    for (name, pcm) in [
        ("original_mix", &original),
        ("residual", &residual),
        ("media_alone", &desired),
    ] {
        let start = Instant::now();
        let mut job = engine.job();
        let result = job
            .transcribe_opts(
                pcm,
                &TranscribeOpts {
                    lang: Some(&language),
                    is_final: true,
                    ..Default::default()
                },
                None,
            )
            .expect("offline recognition");
        let text = result
            .captions
            .iter()
            .map(|caption| caption.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let actual = words(&text);
        if let Some(expected) = &expected_backend {
            assert_eq!(
                job.backend().as_str(),
                expected,
                "requested backend must actually execute inference"
            );
        }
        let word_error_rate = wer(&media_words, &actual);
        let unique_recall = recall(&unique_media, &actual);
        scores.push((word_error_rate, unique_recall));
        recognitions.push(
            json!({ "name": name, "elapsed_ms": start.elapsed().as_millis(),
            "backend": job.backend().as_str(), "backend_reason": job.backend_reason(),
            "media_wer": word_error_rate, "unique_media_word_recall": unique_recall,
            "text": text, "captions": result.captions }),
        );
    }
    let report = json!({ "schema": 1, "test": "known-reference residual with quiet overlapping synthetic media",
        "limitation": "bounded constant-gain aligned waveform experiment; not real Meet/Discord acceptance",
        "model": { "path": model, "sha256": sha256(&model), "load_ms": model_load_ms },
        "inputs": { "reference": reference_path, "reference_sha256": sha256(&reference_path),
            "observed_reference": observed_path, "observed_reference_sha256": sha256(&observed_path),
            "media": media_path, "media_sha256": sha256(&media_path), "transformed_reference": transformed },
        "mix": { "sample_rate": HZ, "duration_ms": length * 1000 / HZ as usize,
            "media_snr_db": media_snr, "reference_gain": reference_gain, "media_gain": media_gain },
        "waveform": { "original_mse_to_desired": original_mse, "residual_mse_to_desired": residual_mse,
            "desired_power": desired_power, "removed_windows": removed_windows, "unchanged_windows": unchanged_windows,
            "windows": evidence },
        "expected_media": expected_media, "expected_reference": expected_reference,
        "unique_media_words": unique_media, "recognitions": recognitions });
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .expect("save ignored local model report");
    eprintln!("offline residual comparison: removed={removed_windows}, unchanged={unchanged_windows}, mse {original_mse:.8}->{residual_mse:.8}; media WER mix/residual/alone={:.3}/{:.3}/{:.3}; unique media recall={:.3}/{:.3}/{:.3}",
        scores[0].0, scores[1].0, scores[2].0, scores[0].1, scores[1].1, scores[2].1);

    assert!(
        scores[2].1 >= 0.75 && scores[2].0 <= 0.35,
        "media-alone control must be intelligible to this model"
    );
    assert!(
        residual_mse <= original_mse * 1.01,
        "suppression must not worsen desired-waveform error"
    );
    assert!(
        scores[1].1 + 0.10 >= scores[0].1,
        "suppression must preserve unique media recognized in the mix"
    );
    if !transformed {
        assert!(
            removed_windows > 0 && residual_mse < original_mse * 0.20,
            "clean aligned reference must give useful waveform cancellation"
        );
        assert!(
            scores[1].1 >= 0.75 && scores[1].1 + 0.10 >= scores[2].1,
            "unique media speech must survive close to its isolated control"
        );
        assert!(
            scores[1].0 <= scores[2].0 + 0.15,
            "residual recognition must stay close to media-alone groundtruth WER"
        );
        if scores[0].1 < 0.75 {
            assert!(
                scores[1].1 > scores[0].1 + 0.10,
                "quiet-media recognition must improve over the original mix"
            );
        }
    }
}

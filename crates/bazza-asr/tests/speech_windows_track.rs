//! Diagnostic: decode a saved 16 kHz mono participant track in speech windows,
//! with the options the post-call combined transcript uses, and print raw and
//! trimmed captions. Run (release is much faster):
//! BAZZA_TEST_MODEL=<ggml-*.bin> BAZZA_TRACK_WAV=<16k mono wav> [BAZZA_TRACK_LANG=ru]
//! cargo test --release -p bazza-asr --test speech_windows_track -- --ignored --nocapture
use std::{path::PathBuf, time::Duration};

use bazza_asr::{chunker::trim_to_signal, whisper::TranscribeOpts, SpeechWindows, WhisperEngine};

#[test]
#[ignore = "needs a whisper model and a saved track; set BAZZA_TEST_MODEL and BAZZA_TRACK_WAV"]
fn prints_speech_window_captions_for_a_saved_track() {
    let model = PathBuf::from(std::env::var("BAZZA_TEST_MODEL").expect("BAZZA_TEST_MODEL"));
    let wav = PathBuf::from(std::env::var("BAZZA_TRACK_WAV").expect("BAZZA_TRACK_WAV"));
    let language = std::env::var("BAZZA_TRACK_LANG").ok();
    let reader = hound::WavReader::open(&wav).unwrap();
    assert_eq!((reader.spec().sample_rate, reader.spec().channels), (16_000, 1));
    let pcm: Vec<f32> = reader.into_samples::<i16>().map(|s| f32::from(s.unwrap()) / 32768.0).collect();
    let engine = WhisperEngine::load(&model, true).unwrap();
    let mut splitter = SpeechWindows::new(16_000, Duration::from_secs(30), Duration::from_secs(4), Duration::from_millis(1500));
    let mut windows = splitter.push(&pcm);
    windows.extend(splitter.flush());
    for window in windows {
        let end = window.start_ms + window.samples.len() as i64 / 16;
        println!("window {}..{} continued={} continues={}", window.start_ms, end, window.continued, window.continues);
        let result = engine.transcribe_opts(&window.samples, &TranscribeOpts {
            offset_ms: window.start_ms, lang: language.as_deref(), is_final: true, max_len: 80,
            drop_no_speech_above: Some(0.6), ..Default::default()
        }).unwrap();
        for caption in result.captions {
            let clamped = (caption.start_ms.clamp(window.start_ms, end), caption.end_ms.clamp(window.start_ms, end));
            println!("  raw {}..{} trimmed {:?} {}", caption.start_ms, caption.end_ms,
                trim_to_signal(&window.samples, window.start_ms, 16_000, clamped), caption.text);
        }
    }
}

//! Requires BAZZA_TEST_MODEL=<path to ggml-*.bin>. Run: cargo test -p bazza-asr -- --ignored
use std::path::PathBuf;

fn read_wav_16k_mono(p: &std::path::Path) -> Vec<f32> {
    let r = hound::WavReader::open(p).unwrap();
    let spec = r.spec();
    assert_eq!(spec.sample_rate, 16000);
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r.into_samples::<i16>().map(|s| s.unwrap() as f32 / i16::MAX as f32).collect(),
        hound::SampleFormat::Float => r.into_samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    samples.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
}

#[test]
#[ignore = "needs a whisper model; set BAZZA_TEST_MODEL"]
fn transcribes_jfk_fixture() {
    let model = std::env::var("BAZZA_TEST_MODEL").map(PathBuf::from).expect("BAZZA_TEST_MODEL");
    let engine = bazza_asr::WhisperEngine::load(&model, true).unwrap();
    let pcm = read_wav_16k_mono(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jfk.wav"));
    let caps = engine.transcribe(&pcm, 0, Some("en"), true).unwrap();
    let text: String = caps.iter().map(|c| c.text.to_lowercase()).collect::<Vec<_>>().join(" ");
    println!("gpu={} captions={caps:#?}", engine.gpu);
    assert!(text.contains("country"), "got: {text}");
    assert!(caps[0].start_ms >= 0 && caps.last().unwrap().end_ms > 5000);
}

#[test]
#[ignore = "needs a whisper model; set BAZZA_TEST_MODEL"]
fn digital_silence_has_no_captions_or_detected_language() {
    let model = std::env::var("BAZZA_TEST_MODEL").map(PathBuf::from).expect("BAZZA_TEST_MODEL");
    let engine = bazza_asr::WhisperEngine::load(&model, false).unwrap();
    for len in [0, 1600, 16_000, 16_000 * 12] {
        let pcm: Vec<f32> = (0..len).map(|i| if i % 2 == 0 { 0.0 } else { -0.0 }).collect();
        let caps = engine.transcribe_with(&pcm, 5000, Some("en"), true, 80).unwrap();
        assert!(caps.is_empty(), "len={len}: {caps:?}");
        let result = engine
            .transcribe_opts(
                &pcm,
                &bazza_asr::whisper::TranscribeOpts {
                    offset_ms: 5000,
                    is_final: true,
                    prompt: Some("The team discussed the project and the next meeting."),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(result.captions.is_empty(), "prompted silence len={len}: {:?}", result.captions);
        assert_eq!(result.lang, None, "silence must not lock language detection");
    }
}

#[test]
#[ignore = "needs a whisper model; set BAZZA_TEST_MODEL"]
fn transcribes_quiet_speech_surrounded_by_silence() {
    let model = std::env::var("BAZZA_TEST_MODEL").map(PathBuf::from).expect("BAZZA_TEST_MODEL");
    let engine = bazza_asr::WhisperEngine::load(&model, false).unwrap();
    let speech = read_wav_16k_mono(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jfk.wav"));
    let mut pcm = vec![0.0; 16_000 * 2];
    pcm.extend(speech.iter().map(|sample| sample * 0.01));
    pcm.extend(vec![0.0; 16_000 * 2]);
    let offset_ms = 5000;
    let caps = engine.transcribe_with(&pcm, offset_ms, Some("en"), true, 80).unwrap();
    let text = caps.iter().map(|c| c.text.to_lowercase()).collect::<Vec<_>>().join(" ");
    assert!(text.contains("country"), "quiet speech was lost: {text}");
    let audio_end_ms = offset_ms + pcm.len() as i64 * 1000 / 16_000;
    assert!(caps.iter().all(|c| c.is_final && c.start_ms >= offset_ms && c.end_ms >= c.start_ms && c.end_ms <= audio_end_ms), "invalid captions: {caps:?}");
}

#[test]
#[ignore = "needs a multilingual model and synthetic Russian and Spanish WAV fixtures"]
fn live_auto_switches_between_russian_english_and_spanish_utterances() {
    use bazza_asr::live::LiveState;
    use std::sync::atomic::AtomicBool;

    let model = std::env::var("BAZZA_TEST_MODEL").map(PathBuf::from).expect("BAZZA_TEST_MODEL");
    let russian = std::env::var("BAZZA_TEST_RU_WAV").map(PathBuf::from).expect("BAZZA_TEST_RU_WAV");
    let spanish = std::env::var("BAZZA_TEST_ES_WAV").map(PathBuf::from).expect("BAZZA_TEST_ES_WAV");
    let english = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jfk.wav");
    let engine = bazza_asr::WhisperEngine::load(&model, true).unwrap();
    let mut job = engine.job();
    let mut live = LiveState::new(None);
    let cancel = AtomicBool::new(false);

    for (wav, anchors) in [
        (russian, ["русской речи", "синяя книга"]),
        (english, ["country", "americans"]),
        (spanish, ["español", "libro azul"]),
    ] {
        live.buf.extend(read_wav_16k_mono(&wav));
        let first_ms = (live.base * 1000 / bazza_asr::live::HZ) as i64;
        let rows = live.transcribe(&mut job, true, &cancel);
        let text = rows.iter().map(|row| row.text.to_lowercase()).collect::<Vec<_>>().join(" ");
        assert!(anchors.iter().all(|anchor| text.contains(anchor)), "{wav:?}: {text}");
        assert!(rows.iter().all(|row| row.is_final && row.start_ms >= first_ms));
    }
}

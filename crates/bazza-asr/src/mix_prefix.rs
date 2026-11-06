//! Final transcription of the ordinary mono mix.

use crate::{whisper::TranscribeOpts, AsrError, Caption, WhisperEngine};
use std::sync::atomic::AtomicBool;

const SAMPLES_PER_MS: usize = 16;
const MINIMUM_PREFIX_MS: usize = 5_000;
const PREROLL_MS: usize = 2_000;

fn trim_offset_ms(pcm16k: &[f32]) -> usize {
    let Some(first_nonzero) = pcm16k.iter().position(|&sample| sample != 0.0) else {
        return 0;
    };
    let prefix_ms = first_nonzero / SAMPLES_PER_MS;
    if prefix_ms >= MINIMUM_PREFIX_MS {
        prefix_ms - PREROLL_MS
    } else {
        0
    }
}

/// Skip only the verified zero prefix, retaining preroll and original timeline.
pub fn transcribe_with_progress(
    engine: &WhisperEngine,
    pcm16k: &[f32],
    language: Option<&str>,
    progress: impl FnMut(u8),
) -> Result<Vec<Caption>, AsrError> {
    transcribe_with_control(engine, pcm16k, language, None, progress)
}

pub fn transcribe_with_control(
    engine: &WhisperEngine,
    pcm16k: &[f32],
    language: Option<&str>,
    cancel: Option<&AtomicBool>,
    progress: impl FnMut(u8),
) -> Result<Vec<Caption>, AsrError> {
    let offset_ms = trim_offset_ms(pcm16k);
    let mut captions = engine
        .transcribe_opts_with_control(
            &pcm16k[offset_ms * SAMPLES_PER_MS..],
            &TranscribeOpts {
                offset_ms: offset_ms as i64,
                lang: language,
                is_final: true,
                max_len: 80,
                ..Default::default()
            },
            cancel,
            progress,
        )?
        .captions;
    if offset_ms != 0 {
        let duration_ms = (pcm16k.len() / SAMPLES_PER_MS) as i64;
        captions.retain_mut(|caption| {
            caption.start_ms = caption.start_ms.max(offset_ms as i64);
            caption.end_ms = caption.end_ms.min(duration_ms);
            caption.start_ms < caption.end_ms
        });
    }
    Ok(captions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_only_an_exact_zero_prefix_with_preroll() {
        let mut pcm = vec![-0.0; 5_001 * SAMPLES_PER_MS + 7];
        pcm.extend_from_slice(&[0.00001, 0.0, -0.25]);
        let offset_ms = trim_offset_ms(&pcm);
        assert_eq!(offset_ms, 3_001);
        assert!(pcm[..offset_ms * SAMPLES_PER_MS].iter().all(|&s| s == 0.0));
        assert_eq!(
            &pcm[(offset_ms + PREROLL_MS) * SAMPLES_PER_MS..],
            &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.00001, 0.0, -0.25]
        );
    }

    #[test]
    fn short_prefix_full_silence_and_quiet_nonzero_input_use_existing_path() {
        let mut pcm = vec![0.0; 6_000 * SAMPLES_PER_MS];
        assert_eq!(trim_offset_ms(&pcm), 0);
        pcm[4_999 * SAMPLES_PER_MS] = 0.00001;
        assert_eq!(trim_offset_ms(&pcm), 0);
        pcm[0] = f32::NAN;
        assert_eq!(trim_offset_ms(&pcm), 0);
    }
}

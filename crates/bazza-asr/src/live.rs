//! Live captions: a growing buffer of the current utterance is re-transcribed
//! every couple of seconds (the line refines in place), then committed when
//! the speaker pauses or the buffer gets long. Near-silent audio is skipped.
//! With an explicit language, committed text provides context for the next
//! pass. Auto detects language afresh from each window.

use crate::whisper::{AsrError, Caption, TranscribeOpts, Transcribed, WhisperEngine, WhisperJob};
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::JoinHandle;

pub const HZ: usize = 16_000;
/// Re-transcribe after this much new audio.
pub const STEP_S: f32 = 2.0;
/// Commit no matter what once the buffer is this long.
pub const MAX_S: f32 = 15.0;
/// A pause this long at the end of the buffer ends the utterance.
pub const PAUSE_S: f32 = 0.8;
/// Below this (dBFS) a window is treated as silence.
pub const GATE_DB: f32 = -50.0;
/// Committed text carried into the next window as the prompt.
pub const PROMPT_CHARS: usize = 220;
/// Segments whisper itself thinks are probably not speech.
pub const NO_SPEECH_MAX: f32 = 0.6;

pub struct LiveTranscriber {
    handle: Option<JoinHandle<()>>,
}

impl LiveTranscriber {
    /// Consumes `rx` until it is disconnected, sending captions on `tx`.
    /// The final partial window is flushed when the input closes.
    pub fn start(engine: Arc<WhisperEngine>, rx: Receiver<Vec<f32>>, tx: Sender<Caption>, lang: Option<String>) -> Self {
        let handle = std::thread::Builder::new()
            .name("bazza-live-asr".into())
            .spawn(move || {
                let mut st = LiveState::new(lang);
                let mut since_step = 0usize;
                let step = (STEP_S * HZ as f32) as usize;
                for pcm in rx.iter() {
                    // Drain everything queued so we never fall further behind than one step.
                    let mut merged = pcm;
                    while let Ok(more) = rx.try_recv() {
                        merged.extend(more);
                    }
                    since_step += merged.len();
                    st.buf.extend(merged);
                    if since_step < step && st.buf.len() < (MAX_S * HZ as f32) as usize {
                        continue;
                    }
                    since_step = 0;
                    for caption in run_pass(&mut st, false, |pcm, opts| engine.transcribe_opts(pcm, opts)) {
                        if tx.send(caption).is_err() { return; }
                    }
                }
                for caption in run_pass(&mut st, true, |pcm, opts| engine.transcribe_opts(pcm, opts)) {
                    if tx.send(caption).is_err() { break; }
                }
            })
            .expect("spawn live asr thread");
        LiveTranscriber { handle: Some(handle) }
    }

    /// Wait for the thread to finish (drop the input sender first).
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Everything the loop carries between passes. Pure apart from the engine call.
pub struct LiveState {
    pub buf: Vec<f32>,
    /// Absolute position (samples) of `buf[0]`.
    pub base: usize,
    pub prompt: String,
    /// Explicit language, if configured.
    pub lang: Option<String>,
}

impl LiveState {
    pub fn new(lang: Option<String>) -> Self {
        LiveState { buf: Vec::with_capacity(HZ * 20), base: 0, prompt: String::new(), lang }
    }

    fn drop_buffer(&mut self) {
        self.base += self.buf.len();
        self.buf.clear();
    }

    /// Reuse a decoder across independent speakers while keeping each speaker's
    /// buffer, language and prompt in their own state.
    pub fn transcribe(&mut self, job: &mut WhisperJob<'_>, flush: bool, cancel: &AtomicBool) -> Vec<Caption> {
        run_pass(self, flush, |pcm, opts| job.transcribe_opts(pcm, opts, Some(cancel)))
    }
}

/// What to do with the current buffer, decided from its audio alone.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing worth sending to the model.
    Silence,
    /// Transcribe and show as provisional.
    Partial,
    /// Transcribe and commit (pause detected or buffer full).
    Commit,
}

pub fn plan(buf: &[f32], flush: bool) -> Plan {
    if buf.len() < HZ / 2 {
        return Plan::Silence;
    }
    if rms_db(buf) < GATE_DB {
        return Plan::Silence;
    }
    if flush || buf.len() >= (MAX_S * HZ as f32) as usize {
        return Plan::Commit;
    }
    let tail = (PAUSE_S * HZ as f32) as usize;
    if buf.len() > tail + HZ && rms_db(&buf[buf.len() - tail..]) < GATE_DB {
        return Plan::Commit;
    }
    Plan::Partial
}

fn run_pass(
    st: &mut LiveState,
    flush: bool,
    decode: impl FnOnce(&[f32], &TranscribeOpts<'_>) -> Result<Transcribed, AsrError>,
) -> Vec<Caption> {
    match plan(&st.buf, flush) {
        Plan::Silence => {
            // Long stretches of nothing are discarded so the next utterance starts fresh.
            if st.buf.len() >= (3.0 * HZ as f32) as usize || flush {
                st.drop_buffer();
            }
            Vec::new()
        }
        p => {
            let commit = p == Plan::Commit;
            let offset_ms = (st.base as i64) * 1000 / HZ as i64;
            let opts = TranscribeOpts {
                offset_ms,
                lang: st.lang.as_deref(),
                is_final: commit,
                max_len: 0,
                prompt: st.lang.as_ref().map(|_| st.prompt.as_str()),
                drop_no_speech_above: Some(NO_SPEECH_MAX),
            };
            let t = match decode(&st.buf, &opts) {
                Ok(t) => t,
                Err(AsrError::Cancelled) => return Vec::new(),
                Err(e) => {
                    tracing::warn!("live asr: {e}");
                    if commit {
                        st.drop_buffer();
                    }
                    return Vec::new();
                }
            };
            let mut caps: Vec<Caption> = t.captions.into_iter().filter(|c| {
                !is_hallucination(&c.text) && !caption_on_digital_silence(c, offset_ms, &st.buf)
            }).collect();
            // With a prompt, whisper sometimes just repeats it when the audio is unclear.
            if echoes_prompt(&caps, &st.prompt) {
                caps.clear();
            }
            let end_ms = offset_ms + (st.buf.len() as i64) * 1000 / HZ as i64;
            if commit {
                if st.lang.is_some() {
                    let text: Vec<&str> = caps.iter().map(|c| c.text.as_str()).collect();
                    st.prompt = tail_chars(&format!("{} {}", st.prompt, text.join(" ")), PROMPT_CHARS);
                }
                st.drop_buffer();
                caps
            } else if !caps.is_empty() {
                // One provisional line for the whole utterance so the UI can replace it in place.
                let text: Vec<&str> = caps.iter().map(|c| c.text.as_str()).collect();
                vec![Caption { start_ms: caps[0].start_ms, end_ms, text: text.join(" "), is_final: false }]
            } else {
                Vec::new()
            }
        }
    }
}

fn caption_on_digital_silence(caption: &Caption, offset_ms: i64, pcm: &[f32]) -> bool {
    let start_ms = caption.start_ms.saturating_sub(offset_ms);
    let end_ms = caption.end_ms.saturating_sub(offset_ms);
    let duration_ms = (pcm.len() / (HZ / 1_000)) as i64;
    if start_ms < 0 || end_ms <= start_ms || end_ms > duration_ms {
        return false;
    }
    // Keep uncertain boundaries and quiet signals. Reject only when the
    // timestamped span and its immediate neighborhood are entirely silent.
    let start = start_ms.saturating_sub(100).max(0) as usize * (HZ / 1_000);
    let end = end_ms.saturating_add(100).min(duration_ms) as usize * (HZ / 1_000);
    pcm[start..end].iter().all(|sample| *sample == 0.0)
}

/// True when the new text is nothing but the end of the prompt again.
pub fn echoes_prompt(caps: &[Caption], prompt: &str) -> bool {
    if caps.is_empty() || prompt.trim().is_empty() {
        return false;
    }
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let text = norm(&caps.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join(" "));
    let p = norm(prompt);
    text.len() >= 12 && p.ends_with(&text)
}

fn rms_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let mean_sq = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    if mean_sq <= 1e-10 {
        return -100.0;
    }
    (20.0 * mean_sq.sqrt().log10()).max(-100.0)
}

fn tail_chars(s: &str, n: usize) -> String {
    let s = s.trim();
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let mut chars = s.chars().skip(count - n - 1);
    let before = chars.next().unwrap_or(' ');
    let cut: String = chars.collect();
    // Start at a word boundary: if the cut landed mid-word, drop that partial word.
    if before.is_whitespace() {
        return cut;
    }
    match cut.find(' ') {
        Some(i) => cut[i + 1..].to_string(),
        None => cut,
    }
}

fn normalized(text: &str) -> String {
    // Trim any punctuation, including "…" and «», around the phrase.
    text.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase().replace('ё', "е")
}

/// Subtitle credits and channel phrases Whisper emits on silence or on short
/// non-speech sounds (a notification or join sound). Real speech practically
/// never matches, so this is safe for post-call participant and system text.
pub fn is_subtitle_credit(text: &str) -> bool {
    let t = normalized(text);
    const KNOWN_RU: &[&str] = &[
        "продолжение следует",
        "спасибо за просмотр",
        "подписывайтесь на канал",
        "subtitles by the amara.org community",
    ];
    if KNOWN_RU.contains(&t.as_str()) {
        return true;
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    // "Субтитры сделал DimaTorzok": a verb of authorship and a short name.
    let author = matches!(words.as_slice(),
        ["субтитры", "сделал" | "создавал" | "подготовил" | "делал", name @ ..] if (1..=3).contains(&name.len()));
    // "Редактор субтитров А.Семкин Корректор А.Егорова"
    let editor = words.starts_with(&["редактор", "субтитров"]) && words.contains(&"корректор") && words.len() <= 8;
    author || editor
}

/// Phrases whisper produces from silence, music or applause rather than speech.
pub fn is_hallucination(text: &str) -> bool {
    let t = normalized(text);
    if t.is_empty() || is_subtitle_credit(text) {
        return true;
    }
    const KNOWN: &[&str] = &[
        "thank you",
        "thanks",
        "thank you for watching",
        "thanks for watching",
        "thank you so much for watching",
        "please subscribe",
        "subscribe to my channel",
        "like and subscribe",
        "see you in the next video",
        "see you next time",
        "bye",
        "you",
        "the end",
        "subtitles by the amara.org community",
        "subtitles by",
        "transcribed by",
        "copyright",
        "www.mooji.org",
    ];
    if KNOWN.contains(&t.as_str()) {
        return true;
    }
    // "you you you you" style repetition.
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.len() >= 4 && words.iter().all(|w| *w == words[0]) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * HZ as f32) as usize).map(|i| (i as f32 * 0.05).sin() * amp).collect()
    }

    #[test]
    fn silence_is_never_transcribed() {
        assert_eq!(plan(&vec![0.0; HZ * 4], false), Plan::Silence);
        assert_eq!(plan(&tone(4.0, 0.0005), false), Plan::Silence); // -66 dBFS
        assert_eq!(plan(&tone(0.2, 0.5), false), Plan::Silence); // too short
    }

    #[test]
    fn speech_is_partial_until_a_pause_or_the_cap() {
        assert_eq!(plan(&tone(4.0, 0.3), false), Plan::Partial);
        let mut paused = tone(4.0, 0.3);
        paused.extend(vec![0.0; HZ]); // 1 s of silence at the end
        assert_eq!(plan(&paused, false), Plan::Commit);
        assert_eq!(plan(&tone(MAX_S + 0.5, 0.3), false), Plan::Commit);
        assert_eq!(plan(&tone(2.0, 0.3), true), Plan::Commit); // flush at stop
    }

    #[test]
    fn auto_detects_again_after_a_preview_and_between_utterances() {
        let mut state = LiveState::new(None);
        for (flush, language, text) in [
            (false, "de", "Ein kurzer Satz"),
            (true, "ru", "Русская реплика"),
            (true, "es", "Una frase en español"),
        ] {
            state.buf = tone(2.0, 0.3);
            let result = run_pass(&mut state, flush, |_, opts| {
                assert_eq!(opts.lang, None);
                assert_eq!(opts.prompt, None);
                Ok(Transcribed {
                    captions: vec![Caption {
                        start_ms: opts.offset_ms,
                        end_ms: opts.offset_ms + 1_000,
                        text: text.into(),
                        is_final: opts.is_final,
                    }],
                    lang: Some(language.into()),
                })
            });
            assert_eq!(result[0].text, text);
        }
    }

    #[test]
    fn live_discards_a_caption_inside_silence_without_losing_quiet_speech() {
        let mut live = LiveState::new(None);
        live.base = HZ;
        live.buf = tone(2.0, 0.02);
        live.buf.resize(HZ * 4, 0.0);
        let rows = run_pass(&mut live, true, |_, opts| {
            let caption = |start_ms, end_ms, text: &str| Caption {
                start_ms: opts.offset_ms + start_ms,
                end_ms: opts.offset_ms + end_ms,
                text: text.into(),
                is_final: opts.is_final,
            };
            Ok(Transcribed {
                lang: Some("ru".into()),
                captions: vec![
                    caption(500, 1700, "Тихая речь"),
                    caption(2600, 3500, "Фраза на цифровой тишине"),
                ],
            })
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "Тихая речь");
    }

    #[test]
    fn hallucinations_are_dropped() {
        assert!(is_hallucination("Thank you."));
        assert!(is_hallucination(" thanks for watching! "));
        assert!(is_hallucination("you you you you you"));
        assert!(!is_hallucination("Thank you for the update on the budget."));
        assert!(!is_hallucination("we ship on the ninth"));
        assert!(is_hallucination("Продолжение следует…"));
        assert!(is_hallucination(" Субтитры сделал DimaTorzok "));
        assert!(!is_hallucination("Продолжение следует после обеда, начнём с бюджета."));
        assert!(is_hallucination("Редактор субтитров А.Семкин Корректор А.Егорова"));
        assert!(is_hallucination("Субтитры подготовил Иван Петров"));
        assert!(!is_hallucination("Субтитры нужно включить в настройках плеера."));
        assert!(!is_subtitle_credit("Субтитры делали мы сами, без подрядчика."));
        assert!(!is_subtitle_credit("Редактор субтитров у нас Иван."));
        // Post-call text keeps real short replies that live ASR filters.
        assert!(!is_subtitle_credit("Thank you.") && !is_subtitle_credit("Bye!"));
    }

    #[test]
    fn prompt_echo_is_dropped() {
        let cap = |t: &str| Caption { start_ms: 0, end_ms: 1, text: t.into(), is_final: true };
        assert!(echoes_prompt(&[cap("we ship on the ninth.")], "okay so we ship on the ninth"));
        assert!(!echoes_prompt(&[cap("and the budget is fine")], "okay so we ship on the ninth"));
        assert!(!echoes_prompt(&[cap("ok")], "ok"));
    }

    #[test]
    fn prompt_keeps_the_tail_on_a_word_boundary() {
        let s = "alpha beta gamma delta epsilon";
        assert_eq!(tail_chars(s, 100), s);
        assert_eq!(tail_chars(s, 13), "delta epsilon");
        assert_eq!(tail_chars(s, 15), "delta epsilon");
    }
}

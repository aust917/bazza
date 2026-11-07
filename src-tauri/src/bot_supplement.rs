//! Offline, source-aware recognition. Never changes capture or archive audio.
use crate::{
    bot_transcript::{self, Source},
    commands::{err, CmdResult},
    state::RuntimeState,
    transcript_versions::{Phase, VersionJob},
    ui_message::UiMessage,
};
use bazza_capture::{dsp::{to_mono, Resampler}, reference_residual::remove_reference,
    speech_alignment::{WaveformMatch, WaveformMatcher}, ASR_HZ};
use bazza_domain::transcript_versions::{
    NewTranscriptVersion, NewTranscriptVersionSegment, TranscriptParticipantSource,
    TranscriptSegmentProvenance, TranscriptSegmentSource, TranscriptVersionSource,
};
use bazza_media_gate::MediaReadGuard;
use serde::Deserialize;
use std::{collections::HashMap, fs::File, io::{BufReader, Read}, path::Path};
use tauri::AppHandle;
use uuid::Uuid;

const WINDOW_MS: i64 = 30_000;
const OVERLAP_MS: i64 = 4_000;
const MAX_ROWS: usize = bazza_domain::local_asr::MAX_LOCAL_ASR_CAPTIONS;
/// Participant rows within this distance of a caption describe the same moment.
const SAME_MOMENT_MS: i64 = 1_500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LivePolicy {
    schema: u32, source: String, provider: TranscriptParticipantSource,
    #[serde(default)] capture_system: Option<bool>,
}

fn unavailable() -> UiMessage {
    UiMessage::app("native.botSupplementUnavailable", "Could not prepare the complete transcript. The original subtitles were kept.")
}

pub(crate) fn selected(guard: &MediaReadGuard) -> CmdResult<Option<Source>> {
    Ok(policy(guard)?.map(|policy| match policy.provider { TranscriptParticipantSource::Meet => Source::Meet, TranscriptParticipantSource::Discord => Source::Discord }))
}

fn policy(guard: &MediaReadGuard) -> CmdResult<Option<LivePolicy>> {
    let file = match guard.open_bot_live_policy() {
        Ok(file) => file,
        Err(bazza_media_gate::MediaGateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() > 1024 { return Err(unavailable()); }
    let policy: LivePolicy = serde_json::from_slice(&bytes).map_err(err)?;
    if policy.schema != 1 || policy.source != "participant_only" { return Err(unavailable()); }
    Ok(Some(policy))
}

/// Open and seek bounded windows instead of retaining hours of PCM in memory.
struct Wave { reader: hound::WavReader<BufReader<File>>, duration_ms: i64 }

impl Wave {
    fn open(file: File) -> CmdResult<Self> {
        let reader = hound::WavReader::new(BufReader::new(file)).map_err(err)?;
        let spec = reader.spec();
        if !matches!(spec.channels, 1 | 2) || !matches!(spec.sample_rate, 16_000 | 44_100 | 48_000)
            || spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
            return Err(unavailable());
        }
        let duration_ms = i64::from(reader.duration()) * 1000 / i64::from(spec.sample_rate);
        if duration_ms > 8 * 60 * 60 * 1000 { return Err(unavailable()); }
        Ok(Self { reader, duration_ms })
    }

    fn window(&mut self, start_ms: i64, end_ms: i64) -> CmdResult<Vec<f32>> {
        if start_ms < 0 || end_ms < start_ms || end_ms - start_ms > WINDOW_MS + OVERLAP_MS {
            return Err(unavailable());
        }
        let spec = self.reader.spec();
        let rate = u64::from(spec.sample_rate);
        let from = start_ms as u64 * rate / 1000;
        let to = (end_ms as u64 * rate / 1000).min(u64::from(self.reader.duration()));
        let expected = ((end_ms - start_ms) as usize) * ASR_HZ as usize / 1000;
        if from >= to { return Ok(vec![0.0; expected]); }
        // Include resampler context, then crop to the requested source coordinates.
        // Use the same absolute output grid for each independently read window.
        // A multiple of 441 native samples is an integer 160 samples at 16kHz;
        // 48kHz similarly uses a multiple of three.
        let grid = match spec.sample_rate { 44_100 => 441, 48_000 => 3, _ => 1 };
        let read_from = from.saturating_sub(64) / grid * grid;
        self.reader.seek(u32::try_from(read_from).map_err(err)?).map_err(err)?;
        let count = usize::try_from(to - read_from).map_err(err)? * usize::from(spec.channels);
        let native = self.reader.samples::<i16>().take(count).map(|value| value.map(|v| f32::from(v) / 32768.0))
            .collect::<Result<Vec<_>, _>>().map_err(err)?;
        if native.len() != count { return Err(unavailable()); }
        let mut mono = to_mono(&native, usize::from(spec.channels));
        mono.extend_from_slice(&[0.0; 64]);
        let mut resampler = Resampler::new(spec.sample_rate, ASR_HZ);
        let output = resampler.process_offline(&mono);
        let skip = ((from - read_from) * u64::from(ASR_HZ) / rate) as usize;
        let mut output = output.get(skip..).unwrap_or(&[]).to_vec();
        output.resize(expected, 0.0);
        output.truncate(expected);
        Ok(output)
    }
}

struct Token { start: usize, end: usize, word: String }

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut begin = None;
    let mut push = |from: usize, to: usize| out.push(Token { start: from, end: to, word: text[from..to].to_lowercase().replace('ё', "е") });
    for (index, character) in text.char_indices() {
        if character.is_alphanumeric() {
            begin.get_or_insert(index);
        } else if let Some(from) = begin.take() {
            push(from, index);
        }
    }
    if let Some(from) = begin { push(from, text.len()); }
    out
}

fn words(text: &str) -> Vec<String> {
    tokens(text).into_iter().map(|token| token.word).collect()
}

/// Short function words ("что", "как", "для") carry little evidence.
fn is_content(word: &str) -> bool {
    word.chars().count() >= 4
}

/// The same word, allowing a different inflection of a longer word.
fn same_word(a: &str, b: &str) -> bool {
    a == b || {
        let (mut x, mut y) = (a.chars(), b.chars());
        a.chars().count() >= 5 && b.chars().count() >= 5 && (0..5).all(|_| x.next() == y.next())
    }
}

/// Caption words that occur in order in the heard words (one LCS alignment).
fn ordered_mask(caption: &[String], heard: &[String]) -> Vec<bool> {
    let (n, m) = (caption.len(), heard.len());
    let mut table = vec![vec![0_u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if same_word(&caption[i], &heard[j]) { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let mut mask = vec![false; n];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if same_word(&caption[i], &heard[j]) && table[i][j] == table[i + 1][j + 1] + 1 {
            mask[i] = true;
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    mask
}

/// The speech a supplement caption adds to participant rows heard at the same moment.
///
/// Discord or Meet playback puts every participant into the system track as
/// well. Bot rows exist only where that participant's own track had signal and
/// are placed on the recording timeline, so the same words, in order, at the
/// same moment are one acoustic event heard twice. Word order is checked per
/// participant, since simultaneous speakers interleave in time. Similar text at
/// another moment is new speech. When a repeat shares one caption with other
/// speech (a video starting as a participant finishes), new leading or trailing
/// words are kept with a proportional share of the caption time.
/// Returns nothing for a repeat, else the whole caption or its new parts.
fn new_speech(text: &str, start: i64, end: i64, known: &[NewTranscriptVersionSegment], subtracted: bool) -> Vec<(i64, i64, String)> {
    let caption = tokens(text);
    let whole = vec![(start, end, text.to_owned())];
    if caption.is_empty() { return whole; }
    let said: Vec<_> = caption.iter().map(|token| token.word.clone()).collect();
    let mut context: Vec<_> = known.iter()
        .filter(|row| row.segment.end_ms + SAME_MOMENT_MS >= start && row.segment.start_ms - SAME_MOMENT_MS <= end)
        .collect();
    context.sort_by_key(|row| (row.segment.start_ms, row.segment.end_ms));
    let mut speakers: Vec<(Option<&str>, Vec<String>)> = Vec::new();
    for row in context {
        let speaker = row.provenance.as_ref().and_then(|origin| origin.participant_id.as_deref());
        let heard = words(&row.segment.text);
        match speakers.iter_mut().find(|(id, _)| *id == speaker) {
            Some((_, list)) => list.extend(heard),
            None => speakers.push((speaker, heard)),
        }
    }
    let mut matched = vec![false; said.len()];
    for (_, heard) in &speakers {
        for (slot, hit) in matched.iter_mut().zip(ordered_mask(&said, heard)) { *slot |= hit; }
    }
    let content: Vec<_> = (0..said.len()).filter(|&i| is_content(&said[i])).collect();
    let content = if content.is_empty() { (0..said.len()).collect() } else { content };
    let hits = content.iter().filter(|&&i| matched[i]).count();
    let repeat = if subtracted {
        // After a verified subtraction the remaining audio is mostly echo of
        // those words; a third of them already identifies the residue.
        hits > 0 && hits * 3 >= content.len()
    } else {
        hits * 2 >= content.len() && (hits >= 2 || content.len() <= 2)
    };
    if !repeat { return whole; }
    let lead = matched.iter().take_while(|hit| !**hit).count();
    let tail = matched.iter().rev().take_while(|hit| !**hit).count();
    let duration = (end - start) as f64;
    let at = |byte: usize| start + (duration * byte as f64 / text.len() as f64) as i64;
    let mut parts = Vec::new();
    for range in [0..lead, said.len() - tail..said.len()] {
        let new_words = range.clone().filter(|&i| is_content(&said[i])).count();
        if range.is_empty() || new_words < 2 || range.len() == said.len() { continue; }
        let (first, last) = (&caption[range.start], &caption[range.end - 1]);
        let to = if range.end == said.len() { text.len() } else { last.end };
        let (a, b) = (at(first.start), at(to));
        parts.push((a, b.max(a + 1), text[first.start..to].trim().to_owned()));
    }
    parts
}

/// Rows without a verified waveform link keep their participant text. Place
/// them with the lag of the nearest verified row of the same track, or of any
/// track: all tracks share one source timeline. Their seek stays disabled
/// (`playback` is `None`). Returns false when a row cannot be placed inside
/// the recording.
fn place_on_recording(rows: &mut [NewTranscriptVersionSegment], duration_ms: i64) -> bool {
    let linked: Vec<(Option<String>, i64, i64)> = rows.iter().filter_map(|row| {
        let origin = row.provenance.as_ref()?;
        let timing = row.playback?;
        Some((origin.participant_id.clone(), (origin.source_start_ms + origin.source_end_ms) / 2, timing.end_ms - origin.source_end_ms))
    }).collect();
    let mut placed = true;
    for row in rows.iter_mut() {
        if let Some(timing) = row.playback {
            row.segment.start_ms = timing.start_ms;
            row.segment.end_ms = timing.end_ms;
            continue;
        }
        let Some(origin) = row.provenance.as_mut().filter(|origin| origin.source == TranscriptSegmentSource::Bot) else { continue };
        let middle = (origin.source_start_ms + origin.source_end_ms) / 2;
        let nearest = |same_track: bool| linked.iter()
            .filter(|(id, ..)| !same_track || *id == origin.participant_id)
            .min_by_key(|(_, at, _)| at.abs_diff(middle))
            .map(|(.., lag)| *lag);
        let span = nearest(true).or_else(|| nearest(false))
            .map(|lag| ((origin.source_start_ms + lag).max(0), (origin.source_end_ms + lag).min(duration_ms)));
        match span {
            Some((start, end)) if start < end => {
                row.segment.start_ms = start;
                row.segment.end_ms = end;
                origin.uncertain = false;
            }
            _ => placed = false,
        }
    }
    placed
}

/// Remove only verified copies in <=5s pieces. No entire speech interval is muted.
fn suppress(
    pcm: &mut [f32], window_start: i64, rows: &[NewTranscriptVersionSegment],
    references: &mut HashMap<String, Wave>, job: &VersionJob,
) -> CmdResult<Vec<(i64, i64)>> {
    let window_end = window_start + pcm.len() as i64 * 1000 / i64::from(ASR_HZ);
    let mut removed = Vec::new();
    let mut matcher = WaveformMatcher::default();
    let mut plans = Vec::<(String, i64, i64, i64)>::new();
    for row in rows {
        job.checkpoint()?;
        let Some(provenance) = row.provenance.as_ref().filter(|p| p.source == TranscriptSegmentSource::Bot && !p.uncertain) else { continue; };
        let Some(id) = provenance.participant_id.as_ref() else { continue; };
        let Some(reference) = references.get(id) else { continue; };
        // The verified end retains the full source duration; the visible start may
        // have trimmed a leading pause, so deriving the lag from start is unsafe.
        let delta = row.segment.end_ms - provenance.source_end_ms;
        let from = (provenance.source_start_ms + delta - 100).max(window_start).max(delta).max(0);
        let to = (row.segment.end_ms + 100).min(window_end).min(reference.duration_ms + delta);
        if from < to { plans.push((id.clone(), delta, from, to)); }
    }
    plans.sort_by(|a,b| (&a.0,a.1,a.2).cmp(&(&b.0,b.1,b.2)));
    let mut merged = Vec::<(String,i64,i64,i64)>::new();
    for plan in plans {
        if let Some(last) = merged.last_mut().filter(|last| last.0 == plan.0 && last.1 == plan.1 && plan.2 <= last.3) {
            last.3 = last.3.max(plan.3);
        } else { merged.push(plan); }
    }
    for (id,delta,from,to) in merged {
        let reference = references.get_mut(&id).expect("validated reference");
        let mut start = from;
        while start < to {
            let end = (start + 4_000).min(to);
            if end - start < 600 { break; }
            let reference_pcm = reference.window(start - delta, end - delta)?;
            let expected = ((start - window_start) * 16) as usize;
            let search_start = expected.saturating_sub(1_280);
            let search_end = (expected + reference_pcm.len() + 1_280).min(pcm.len());
            let downsample = |samples: &[f32]| {
                let mut input = samples.to_vec(); input.extend_from_slice(&[0.0;64]);
                let mut output = Resampler::new(ASR_HZ, 8_000).process_offline(&input);
                output.truncate(samples.len() / 2); output
            };
            // Per-caption playback matching permits about 20ms disagreement.
            // Re-establish a unique local lag before sample-level subtraction.
            if let WaveformMatch::Located(offset) = matcher.search(&downsample(&reference_pcm),
                &downsample(&pcm[search_start..search_end]), 8_000) {
                let a = search_start + offset * 2;
                let b = a + reference_pcm.len();
                if b <= pcm.len() && remove_reference(&mut pcm[a..b], &reference_pcm, ASR_HZ).removed() {
                    let actual = window_start + a as i64 / 16;
                    removed.push((actual, actual + (end-start)));
                }
            }
            start = end;
        }
    }
    Ok(removed)
}

fn covered(start: i64, end: i64, removed: &[(i64, i64)]) -> bool {
    let mut intervals = removed.iter().copied().filter(|(a,b)| *b > start && *a < end).collect::<Vec<_>>();
    intervals.sort_unstable();
    let mut position = start;
    for (a,b) in intervals {
        if a > position + 20 { return false; }
        position = position.max(b);
    }
    position >= end - 20
}

pub(crate) fn compute(
    app: &AppHandle, runtime: &RuntimeState, guard: &MediaReadGuard,
    meeting_id: &str, model: &str, language: Option<&str>, job: &VersionJob,
) -> CmdResult<NewTranscriptVersion> {
    let source = match selected(guard)? {
        Some(source) => source,
        None => bot_transcript::participant_source(guard, None)?.ok_or_else(unavailable)?,
    };
    let _busy = runtime.busy_guard();
    let tracks = bot_transcript::compute_for_supplement(app, runtime, guard, meeting_id, source, model, language, job)?;
    let mut rows = tracks.rows;
    let capture_policy = policy(guard)?;
    let mut partial = tracks.partial || capture_policy.as_ref().is_none_or(|policy| policy.capture_system.is_none());
    let mix_duration = Wave::open(guard.open_playback_file(bazza_media_gate::PlaybackMediaKind::Audio).map_err(|_| unavailable())?)?.duration_ms;
    partial |= !place_on_recording(&mut rows, mix_duration);
    let mut references = tracks.references.into_iter().map(|(id,file)| Wave::open(file).map(|wave| (id,wave)))
        .collect::<CmdResult<HashMap<_,_>>>()?;
    let engine = runtime.whisper(model).ok_or_else(unavailable)?;
    let mut whisper = engine.job();
    let locale = *runtime.ui_locale.read();
    // Each source is recognized independently. The system lane excludes the
    // microphone; mic is a fallback and must not duplicate system or bot speech.
    for microphone in [false, true] {
        let file = match guard.open_source_audio(microphone) {
            Ok(file) => file,
            Err(bazza_media_gate::MediaGateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                partial |= microphone || capture_policy.as_ref().and_then(|policy| policy.capture_system) != Some(false);
                continue;
            }
            Err(_) => return Err(unavailable()),
        };
        let mut wave = Wave::open(file)?;
        partial |= wave.duration_ms.abs_diff(mix_duration) > 100;
        let known = rows.clone();
        let (mut added, mut repeated) = (0_usize, 0_usize);
        let label = if microphone { locale.text("Микрофон", "Microphone") } else { locale.text("Системный звук", "System audio") };
        let source = if microphone { TranscriptSegmentSource::Microphone } else { TranscriptSegmentSource::System };
        // Speech windows: digital silence is not decoded, so captions keep
        // their real times and no phrase is lost at a fixed window seam.
        // Continuous sound (music, noise floor) falls back to 30 s windows.
        let mut windows = bot_transcript::speech_windows();
        let mut removed = Vec::new();
        let mut start = 0;
        while start < wave.duration_ms {
            job.checkpoint()?;
            job.update(app, meeting_id, Phase::Recognizing, Some((50 + 45 * start / wave.duration_ms.max(1)) as u8));
            let end = (start + WINDOW_MS).min(wave.duration_ms);
            // Re-read a short context so a copy crossing the chunk seam is
            // subtracted in full; only the new part enters the speech windows.
            let context = start.min(OVERLAP_MS);
            let mut pcm = wave.window(start - context, end)?;
            // Removal residue below two PCM16 steps reads as silence afterwards.
            removed.extend(suppress(&mut pcm, start - context, &known, &mut references, job)?);
            let skip = (context * i64::from(ASR_HZ) / 1000) as usize;
            let mut ready = windows.push(&pcm[skip.min(pcm.len())..]);
            if end == wave.duration_ms { ready.extend(windows.flush()); }
            for window in ready {
                let window_end = bot_transcript::window_end_ms(&window).min(wave.duration_ms);
                let owner = bot_transcript::window_owner(&window, window_end);
                bot_transcript::decode_window(&mut whisper, &job.cancel, language, &window, window_end, |a, b, text| {
                    if !owner.contains(&(a + (b - a) / 2)) { return Ok(()); }
                    let parts = new_speech(&text, a, b, &known, covered(a, b, &removed));
                    if parts.first().is_none_or(|part| part.2 != text) { repeated += 1; }
                    for (a, b, text) in parts {
                        rows.push(NewTranscriptVersionSegment {
                            segment: bazza_domain::NewSegment { start_ms: a, end_ms: b, speaker: Some(label.into()), text },
                            playback: None,
                            provenance: Some(TranscriptSegmentProvenance { source, provider: None, participant_id: None,
                                source_start_ms: a, source_end_ms: b, uncertain: false }),
                        });
                        added += 1;
                        if rows.len() > MAX_ROWS { return Err(unavailable()); }
                    }
                    Ok(())
                })?;
            }
            start = end;
        }
        log::info!("combined transcript: lane={} added={added} repeated_participants={repeated}", if microphone { "microphone" } else { "system" });
    }
    rows.sort_by_key(|row| (row.segment.start_ms, row.segment.end_ms));
    job.checkpoint()?;
    if rows.is_empty() { return Err(crate::transcript_versions::no_speech()); }
    Ok(NewTranscriptVersion { id:Uuid::new_v4().to_string(), source:TranscriptVersionSource::Combined,
        partial, model:model.to_owned(), language:language.map(str::to_owned), segments:rows })
}

/// A completed version is the durable success marker, so retry after a crash
/// does not depend on an in-memory task flag or a mutable audio sidecar.
pub(crate) fn has_result(versions: &bazza_domain::transcript_versions::TranscriptVersions) -> bool {
    versions.versions.iter().any(|version| version.source == TranscriptVersionSource::Combined)
}

pub(crate) fn published_result(versions: &bazza_domain::transcript_versions::TranscriptVersions) -> bool {
    versions.versions.iter().any(|version| versions.effective_id.as_deref() == Some(version.id.as_str())
        && version.source == TranscriptVersionSource::Combined && !version.partial)
}

pub(crate) fn marker_exists(directory: &Path) -> bool {
    directory.join("bot-live-policy.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_requires_the_entire_event_and_tolerates_only_rounding() {
        assert!(covered(1000,2000,&[(900,1500),(1500,2100)]));
        assert!(!covered(1000,2000,&[(900,1200),(1600,2100)]));
        assert!(!covered(1000,2000,&[]));
    }
    fn bot(start: i64, end: i64, text: &str, linked: Option<i64>) -> NewTranscriptVersionSegment {
        NewTranscriptVersionSegment {
            segment: bazza_domain::NewSegment { start_ms: start, end_ms: end, speaker: Some("Alex".into()), text: text.into() },
            playback: linked.map(|lag| bazza_domain::transcript_versions::TranscriptPlaybackTiming { start_ms: start + lag, end_ms: end + lag }),
            provenance: Some(TranscriptSegmentProvenance { source: TranscriptSegmentSource::Bot,
                provider: Some(TranscriptParticipantSource::Discord), participant_id: Some("1".into()),
                source_start_ms: start, source_end_ms: end, uncertain: linked.is_none() }),
        }
    }

    fn other(start: i64, end: i64, id: &str, text: &str) -> NewTranscriptVersionSegment {
        let mut row = bot(start, end, text, Some(0));
        row.provenance.as_mut().unwrap().participant_id = Some(id.into());
        row
    }

    #[test]
    fn participant_speech_heard_again_in_system_audio_is_one_event() {
        let known = [bot(10_000, 14_000, "Главный вход остаётся со стороны сквера, а доставку оборудования проведём через двор.", None)];
        let repeat = |text: &str, start, end, subtracted| new_speech(text, start, end, &known, subtracted).is_empty();
        let kept = |text: &str, start, end| new_speech(text, start, end, &known, false) == vec![(start, end, text.to_owned())];
        // Same words at the same moment, with a different inflection and a split caption.
        assert!(repeat("Главный вход остается со стороны сквера, а", 10_200, 12_000, false));
        assert!(repeat("доставку оборудование проведем через двор.", 12_000, 14_300, false));
        // A video playing under the participant keeps its own words.
        assert!(kept("Для каждого месяца выберу отдельную палитру", 11_000, 13_000));
        assert!(kept("и короткое наблюдение о сквере", 11_000, 13_000));
        // The same sentence at another moment is a new event.
        assert!(kept("Главный вход остаётся со стороны сквера", 40_000, 42_000));
        // A verified subtraction leaves echo; a partly recognized remainder is residue.
        assert!(repeat("Через двор, говорю я", 13_000, 14_000, true));
        assert!(new_speech("Совсем другая фраза ролика", 13_000, 14_000, &known, true).len() == 1);
        // A short misheard echo of two words is not new speech.
        let window = [bot(95_000, 99_700, "Предлагаю начать с 50 участников в каждом временном окне.", None)];
        assert!(new_speech("...в мином окне.", 95_000, 99_720, &window, false).is_empty());
    }

    #[test]
    fn new_words_sharing_a_caption_with_a_repeat_are_kept() {
        let known = [bot(50_000, 56_000, "Продолжительность фильма ровно семь минут, без рекламных вставок.", None)];
        let parts = new_speech("минут, без рекламных вставок. У меня появилась", 54_000, 59_000, &known, false);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].2, "У меня появилась");
        assert_eq!(new_speech("минут, без рекламных вставок. У меня появилась", 54_000, 59_000, &known, true), parts,
            "a verified subtraction does not erase new words sharing the caption");
        assert!(parts[0].0 > 54_000 && parts[0].1 == 59_000);
        // One leftover word is not enough evidence of new speech.
        assert!(new_speech("комнаты. Продолжительность фильма ровно семь минут", 54_000, 57_000, &known, false).is_empty());
    }

    #[test]
    fn word_order_is_checked_per_participant_for_simultaneous_speakers() {
        let known = [
            other(284_150, 287_320, "9", "Возле сцены убрали лишние стулья, оставили проход"),
            other(284_410, 286_090, "6", "где можно обсудить впечатления."),
        ];
        assert!(new_speech("где можно найти лишние стулья.", 284_560, 286_540, &known, false).is_empty());
    }

    #[test]
    fn unlinked_participant_rows_move_to_the_recording_timeline_without_a_seek() {
        let mut rows = vec![bot(1_000, 3_000, "раз", Some(3_600)), bot(20_000, 22_000, "два", None), bot(50_000, 51_000, "три", Some(3_700))];
        assert!(place_on_recording(&mut rows, 600_000));
        let placed: Vec<_> = rows.iter().map(|row| (row.segment.start_ms, row.playback.is_some(), row.provenance.as_ref().unwrap().uncertain)).collect();
        assert_eq!(placed, vec![(4_600, true, false), (23_600, false, false), (53_700, true, false)]);
        assert_eq!(rows[1].provenance.as_ref().unwrap().source_start_ms, 20_000, "source coordinates are kept");
        let mut lonely = vec![bot(20_000, 22_000, "два", None)];
        assert!(!place_on_recording(&mut lonely, 600_000), "no verified lag means the result stays partial");
        let mut late = vec![bot(1_000, 3_000, "раз", Some(3_600)), bot(70_000, 72_000, "после записи", None)];
        assert!(!place_on_recording(&mut late, 60_000), "a row past the end of the recording is not placed");
    }
}

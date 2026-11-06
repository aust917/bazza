//! Offline, source-aware recognition. Never changes capture or archive audio.
use crate::{
    bot_transcript::{self, Source},
    commands::{err, CmdResult},
    state::RuntimeState,
    transcript_versions::{Phase, VersionJob},
    ui_message::UiMessage,
};
use bazza_asr::whisper::TranscribeOpts;
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

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase).collect()
}

fn shared_time(a: &NewTranscriptVersionSegment, start: i64, end: i64) -> bool {
    let overlap = a.segment.end_ms.min(end) - a.segment.start_ms.max(start);
    overlap > 0 && overlap * 3 >= (a.segment.end_ms - a.segment.start_ms).min(end - start)
}

/// Text can only flag ambiguity; it never authorizes deletion of an event.
fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() { return false; }
    let matches = a.iter().filter(|word| b.contains(word)).count();
    matches * 2 >= a.len().min(b.len()) && matches > 0
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

fn only_removal_residue(pcm: &[f32], start_ms: i64, removed: &[(i64,i64)]) -> bool {
    pcm.iter().enumerate().all(|(index,sample)| {
        let ms = start_ms + index as i64 / 16;
        if removed.iter().any(|(start,end)| (*start..*end).contains(&ms)) {
            sample.abs() <= 2.0 / 32768.0
        } else { *sample == 0.0 }
    })
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
    for row in &mut rows {
        if let Some(playback) = row.playback {
            row.segment.start_ms = playback.start_ms;
            row.segment.end_ms = playback.end_ms;
        } else {
            partial = true;
        }
    }
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
        let mut start = 0;
        while start < wave.duration_ms {
            job.checkpoint()?;
            job.update(app, meeting_id, Phase::Recognizing, Some((50 + 45 * start / wave.duration_ms.max(1)) as u8));
            let end = (start + WINDOW_MS).min(wave.duration_ms);
            let mut pcm = wave.window(start, end)?;
            let removed = suppress(&mut pcm, start, &known, &mut references, job)?;
            // Numerical residue below two PCM16 steps in fully explained audio
            // is not a new event. Do not apply a general loudness speech gate.
            let numerical_only = only_removal_residue(&pcm, start, &removed);
            if !numerical_only {
                let result = whisper.transcribe_opts(&pcm, &TranscribeOpts {
                    offset_ms: start, lang: language, is_final: true, max_len: 80,
                    drop_no_speech_above: Some(0.6), ..Default::default()
                }, Some(&job.cancel)).map_err(err)?;
                let owner_start = if start == 0 { 0 } else { start + OVERLAP_MS / 2 };
                let owner_end = if end == wave.duration_ms { end } else { end - OVERLAP_MS / 2 };
                for caption in result.captions {
                    let a = caption.start_ms.max(start);
                    let b = caption.end_ms.min(end);
                    let middle = a + (b - a) / 2;
                    if a >= b || !(owner_start..owner_end).contains(&middle) || caption.text.trim().is_empty() { continue; }
                    // Remaining overlap after a rejected fit is visible but cannot
                    // be claimed as new media. Likewise a same-text residual may
                    // be a second event or an imperfectly removed copy.
                    let overlaps = known.iter().filter(|row| shared_time(row,a,b)).collect::<Vec<_>>();
                    let uncertain = overlaps.iter().any(|row| similar(&row.segment.text, &caption.text))
                        || (!covered(a,b,&removed) && !overlaps.is_empty());
                    partial |= uncertain;
                    rows.push(NewTranscriptVersionSegment {
                        segment: bazza_domain::NewSegment { start_ms:a, end_ms:b,
                            speaker: Some(if microphone { locale.text("Микрофон", "Microphone") } else { locale.text("Системный звук", "System audio") }.into()), text:caption.text },
                        playback: None,
                        provenance: Some(TranscriptSegmentProvenance { source: if microphone { TranscriptSegmentSource::Microphone } else { TranscriptSegmentSource::System },
                            provider:None, participant_id:None, source_start_ms:a, source_end_ms:b, uncertain }),
                    });
                    if rows.len() > MAX_ROWS { return Err(unavailable()); }
                }
            }
            if end == wave.duration_ms { break; }
            start += WINDOW_MS - OVERLAP_MS;
        }
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
    #[test]
    fn text_similarity_only_flags_ambiguity_including_short_replies() {
        assert!(similar("Да", "да!"));
        assert!(similar("Откройте второй проект", "Откройте, пожалуйста, проект"));
        assert!(!similar("Первый проект", "Реклама закончилась"));
    }
    #[test]
    fn short_cancelled_speech_surrounded_by_silence_is_not_a_new_event() {
        let mut pcm = vec![0.0; 30 * 16_000];
        pcm[16_000..32_000].fill(1.0/32768.0);
        assert!(only_removal_residue(&pcm,0,&[(1000,2000)]));
        pcm[160_000] = 0.00000001;
        assert!(!only_removal_residue(&pcm,0,&[(1000,2000)]), "never erase quiet untouched speech");
    }
}

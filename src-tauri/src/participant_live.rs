//! Live captions exclusively from the bot's participant streams.
//! Capture only offers PCM to bounded queues; inference runs on one thread.

use crate::{
    live_feed::LiveFeed,
    settings::Settings,
    state::{AppState, RuntimeState},
};
use bazza_asr::{
    live::{LiveState, HZ, MAX_S, PAUSE_S, STEP_S},
    Caption,
};
use bazza_capture::dsp::Resampler;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

const MAX_SPEAKERS: usize = 64;
const MAX_FRAME_SAMPLES: usize = 4096;
const BUFFER_LIMIT: usize = HZ * (MAX_S as usize + STEP_S as usize);
const STEP_SAMPLES: usize = HZ * STEP_S as usize;
const UTTERANCE_GAP_MS: u64 = (PAUSE_S * 1000.0) as u64;
const STOP_FLUSH_BUDGET: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Control {
    // Even epochs accept frames. Every pause and resume fences the previous queue.
    epoch: AtomicU64,
    closing: AtomicBool,
    stopped: AtomicBool,
}

struct Frame {
    epoch: u64,
    speaker: String,
    name: Option<Arc<str>>,
    start_ms: u64,
    rate: u32,
    samples: Vec<i16>,
    received: Instant,
}

#[derive(Clone)]
pub(crate) struct ParticipantAudioSender {
    control: Arc<Control>,
    tx: Sender<Frame>,
    names: Sender<Frame>,
}

impl ParticipantAudioSender {
    /// An empty frame can supply a display name after the speaker stopped.
    pub(crate) fn try_send(
        &self,
        speaker_id: String,
        speaker_name: Option<Arc<str>>,
        start_ms: u64,
        sample_rate: u32,
        samples: Vec<i16>,
    ) {
        let epoch = self.control.epoch.load(Ordering::Acquire);
        if self.control.stopped.load(Ordering::Acquire)
            || self.control.closing.load(Ordering::Acquire)
            || epoch % 2 != 0
            || !matches!(sample_rate, 16_000 | 48_000)
            || samples.len() > MAX_FRAME_SAMPLES
        {
            return;
        }
        // A late roster result for an ended utterance must not compete with PCM.
        let queue = if samples.is_empty() {
            if speaker_name.is_none() {
                return;
            }
            &self.names
        } else {
            &self.tx
        };
        let _ = queue.try_send(Frame {
            epoch,
            speaker: speaker_id,
            name: speaker_name,
            start_ms,
            rate: sample_rate,
            samples,
            received: Instant::now(),
        });
    }
}

pub(crate) struct ParticipantSession {
    app: AppHandle,
    meeting_id: String,
    sender: ParticipantAudioSender,
    input: Option<(Receiver<Frame>, Receiver<Frame>)>,
    finished: Option<Receiver<()>>,
}

impl ParticipantSession {
    pub(crate) fn new(app: &AppHandle, meeting_id: &str) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(2048);
        let (names, name_updates) = crossbeam_channel::bounded(MAX_SPEAKERS * 2);
        Self {
            app: app.clone(),
            meeting_id: meeting_id.into(),
            sender: ParticipantAudioSender {
                control: Arc::new(Control::default()),
                tx,
                names,
            },
            input: Some((rx, name_updates)),
            finished: None,
        }
    }

    pub(crate) fn start(&mut self, settings: &Settings) {
        let Some((rx, names)) = self.input.take() else {
            return;
        };
        let (app, id, settings) = (self.app.clone(), self.meeting_id.clone(), settings.clone());
        let control = self.sender.control.clone();
        let (done, finished) = crossbeam_channel::bounded(1);
        match std::thread::Builder::new()
            .name("bazza-participant-live".into())
            .spawn(move || {
                run(&app, &id, &settings, &control, rx, names);
                let _ = done.try_send(());
            })
        {
            Ok(_) => self.finished = Some(finished),
            Err(error) => log::warn!("Could not start participant captions: {error}"),
        }
    }

    pub(crate) fn finish(self) {
        self.sender.control.closing.store(true, Ordering::Release);
        if let Some(finished) = &self.finished {
            // Drop aborts any outstanding decoding if the short final pass overruns.
            let _ = finished.recv_timeout(STOP_FLUSH_BUDGET);
        }
    }

    pub(crate) fn sender(&self) -> ParticipantAudioSender {
        self.sender.clone()
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        let changed = self
            .sender
            .control
            .epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                ((epoch % 2 != 0) != paused).then(|| epoch.wrapping_add(1))
            })
            .is_ok();
        if changed && paused {
            if let Some(state) = runtime(&self.app) {
                LiveFeed::clear_named_partials(&self.app, state, &self.meeting_id);
            }
        }
    }
}

impl Drop for ParticipantSession {
    fn drop(&mut self) {
        self.sender.control.stopped.store(true, Ordering::Release);
    }
}

fn runtime(app: &AppHandle) -> Option<&RuntimeState> {
    app.try_state::<RuntimeState>()
        .map(|state| state.inner())
        .or_else(|| app.try_state::<AppState>().map(|state| &**state.inner()))
}

struct Track {
    name: Option<Arc<str>>,
    name_at: Option<Instant>,
    audio: LiveState,
    resampler: Resampler,
    end_ms: Option<u64>,
    last_audio: Instant,
    last_pass: Instant,
    new_samples: usize,
}

impl Track {
    fn new(lang: Option<String>, rate: u32, now: Instant) -> Self {
        Self {
            name: None,
            name_at: None,
            audio: LiveState::new(lang),
            resampler: Resampler::new(rate, HZ as u32),
            end_ms: None,
            last_audio: now,
            last_pass: now,
            new_samples: 0,
        }
    }

    fn ready_to_decode(&self, closing: bool, queue_caught_up: bool, now: Instant) -> bool {
        !self.audio.buf.is_empty()
            && (self.new_samples >= STEP_SAMPLES || self.quiet(closing, queue_caught_up, now))
    }

    fn quiet(&self, closing: bool, queue_caught_up: bool, now: Instant) -> bool {
        closing || (queue_caught_up
            && now.saturating_duration_since(self.last_audio) >= Duration::from_millis(UTTERANCE_GAP_MS))
    }
}

#[derive(Default)]
struct Streams {
    tracks: HashMap<String, Track>,
    finished: VecDeque<(String, LiveState)>,
}

impl Streams {
    fn ingest(&mut self, frame: Frame, epoch: u64, lang: Option<&str>) {
        if frame.epoch != epoch || epoch % 2 != 0 {
            return;
        }
        if self.tracks.len() >= MAX_SPEAKERS && !self.tracks.contains_key(&frame.speaker) {
            return;
        }
        let track = self
            .tracks
            .entry(frame.speaker.clone())
            .or_insert_with(|| Track::new(lang.map(str::to_owned), frame.rate, frame.received));
        if let Some(name) = frame.name.filter(|name| !name.trim().is_empty()) {
            if track.name_at.is_none_or(|at| at <= frame.received) {
                track.name = Some(name);
                track.name_at = Some(frame.received);
            }
        }
        if frame.samples.is_empty() {
            return;
        }
        if track
            .end_ms
            .is_some_and(|end| frame.start_ms.saturating_sub(end) >= UTTERANCE_GAP_MS)
        {
            let old = std::mem::replace(&mut track.audio, LiveState::new(lang.map(str::to_owned)));
            if !old.buf.is_empty() && self.finished.len() < MAX_SPEAKERS {
                self.finished.push_back((frame.speaker, old));
            }
            track.resampler = Resampler::new(frame.rate, HZ as u32);
            track.new_samples = 0;
        }
        if track.audio.buf.is_empty() {
            track.audio.base = (frame.start_ms as usize).saturating_mul(HZ / 1000);
        } else if let Some(end) = track.end_ms {
            // Ignore small arrival-time variation, but preserve short gaps in the timeline.
            let gap = frame.start_ms.saturating_sub(end);
            if gap >= 40 {
                track
                    .audio
                    .buf
                    .resize(track.audio.buf.len() + gap as usize * (HZ / 1000), 0.0);
            }
        }
        track.end_ms = Some(
            frame
                .start_ms
                .saturating_add(frame.samples.len() as u64 * 1000 / u64::from(frame.rate)),
        );
        track.last_audio = frame.received;
        let pcm: Vec<_> = frame
            .samples
            .into_iter()
            .map(|sample| f32::from(sample) / 32768.0)
            .collect();
        let pcm = track.resampler.process_offline(&pcm);
        track.new_samples += pcm.len();
        track.audio.buf.extend(pcm);
        let excess = track.audio.buf.len().saturating_sub(BUFFER_LIMIT);
        if excess != 0 {
            track.audio.buf.drain(..excess);
            track.audio.base += excess;
            track.new_samples = track.new_samples.min(BUFFER_LIMIT);
        }
    }
}

fn run(
    app: &AppHandle,
    meeting_id: &str,
    settings: &Settings,
    control: &Control,
    rx: Receiver<Frame>,
    names: Receiver<Frame>,
) {
    let Some(state) = runtime(app) else { return };
    let _busy = state.busy_guard();
    let Some(engine) = crate::stt_stage::live_engine(app, state, meeting_id, settings) else {
        return;
    };
    let mut job = engine.job();
    let lang = settings.language.as_deref();
    let mut epoch = 0;
    let mut streams = Streams::default();
    let mut pending = None;
    let unknown_label = state.ui_locale.read().text("Участник", "Participant");
    let mut speaker_labels = SpeakerLabels::new(unknown_label);
    while !control.stopped.load(Ordering::Acquire) {
        let current = control.epoch.load(Ordering::Acquire);
        if current != epoch {
            epoch = current;
            streams = Streams::default();
        }
        if let Some(frame) = pending.take() {
            streams.ingest(frame, epoch, lang);
        }
        for frame in rx.try_iter().take(512) {
            streams.ingest(frame, epoch, lang);
        }
        for frame in names.try_iter().take(MAX_SPEAKERS * 2) {
            streams.ingest(frame, epoch, lang);
        }
        let queue_caught_up = rx.is_empty();
        let closing = control.closing.load(Ordering::Acquire) && queue_caught_up && names.is_empty();
        let mut publish = |id: &str, name: Option<&str>, captions: Vec<Caption>, flush: bool| {
            if control.epoch.load(Ordering::Acquire) != epoch || control.stopped.load(Ordering::Acquire) {
                return;
            }
            let clear = captions.is_empty() && flush;
            let name = speaker_labels.label(id, name);
            for caption in captions.into_iter().chain(clear.then(|| Caption {
                start_ms: 0,
                end_ms: 0,
                text: String::new(),
                is_final: true,
            })) {
                LiveFeed::publish_keyed(app, state, meeting_id, id, Some(name), caption, || {
                    control.epoch.load(Ordering::Acquire) == epoch
                        && !control.stopped.load(Ordering::Acquire)
                });
            }
        };
        let now = Instant::now();
        // Missing roster metadata must not block a participant's audio. The
        // source ID stays stable when a platform name arrives later.
        let decoded = if let Some((id, mut audio)) = streams.finished.pop_front() {
            publish(
                &id,
                streams.tracks[&id].name.as_deref(),
                audio.transcribe(&mut job, true, &control.stopped),
                true,
            );
            true
        } else if let Some((id, track)) = streams
            .tracks
            .iter_mut()
            .filter(|(_, track)| track.ready_to_decode(closing, queue_caught_up, now))
            .min_by_key(|(_, track)| track.last_pass)
        {
            let flush = track.quiet(closing, queue_caught_up, now);
            let captions = track.audio.transcribe(&mut job, flush, &control.stopped);
            publish(id, track.name.as_deref(), captions, flush || track.audio.buf.is_empty());
            track.last_pass = Instant::now();
            track.new_samples = 0;
            true
        } else {
            false
        };
        if decoded {
            continue;
        }
        if closing {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(frame) => pending = Some(frame),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

struct SpeakerLabel {
    platform_name: Option<String>,
    display: String,
}

struct SpeakerLabels {
    by_id: HashMap<String, SpeakerLabel>,
    used: HashSet<String>,
    unknown_label: String,
    next_unknown: usize,
}

impl SpeakerLabels {
    fn new(unknown_label: &str) -> Self {
        Self {
            by_id: HashMap::new(),
            used: HashSet::new(),
            unknown_label: unknown_label.to_owned(),
            next_unknown: 1,
        }
    }

    fn label(&mut self, id: &str, name: Option<&str>) -> &str {
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        if !self.by_id.get(id).is_some_and(|label| label.platform_name.as_deref() == name) {
            if let Some(previous) = self.by_id.remove(id) {
                self.used.remove(&previous.display.to_lowercase());
            }
            let base = match name {
                Some(name) => name.to_owned(),
                None => {
                    let label = format!("{} {}", self.unknown_label, self.next_unknown);
                    self.next_unknown += 1;
                    label
                }
            };
            let mut display = base.clone();
            let mut number = 2;
            while !self.used.insert(display.to_lowercase()) {
                display = format!("{base} ({number})");
                number += 1;
            }
            self.by_id.insert(id.to_owned(), SpeakerLabel {
                platform_name: name.map(str::to_owned),
                display,
            });
        }
        &self.by_id[id].display
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_labels_keep_ids_distinct_and_accept_late_names() {
        let mut labels = SpeakerLabels::new("Участник");
        assert_eq!(labels.label("discord:1", None), "Участник 1");
        assert_eq!(labels.label("discord:2", None), "Участник 2");
        assert_eq!(labels.label("discord:1", None), "Участник 1");
        assert_eq!(labels.label("discord:1", Some("Аня")), "Аня");
        assert_eq!(labels.label("discord:2", Some("Аня")), "Аня (2)");
        assert_eq!(labels.label("discord:1", Some("Анна")), "Анна");
        assert_eq!(labels.label("discord:2", Some("Аня")), "Аня (2)");
    }

    fn frame(
        speaker: &str,
        name: Option<&str>,
        start_ms: u64,
        epoch: u64,
        samples: Vec<i16>,
    ) -> Frame {
        Frame {
            speaker: speaker.into(),
            name: name.map(Arc::from),
            start_ms,
            epoch,
            rate: HZ as u32,
            samples,
            received: Instant::now(),
        }
    }

    #[test]
    fn late_names_keep_separate_speakers_and_the_original_start() {
        let mut streams = Streams::default();
        streams.ingest(frame("one", None, 1_000, 0, vec![12; 320]), 0, None);
        streams.ingest(frame("two", Some("Alex"), 1_020, 0, vec![24; 320]), 0, None);
        streams.ingest(frame("one", Some("Alex"), 2_000, 0, vec![]), 0, None);
        let one = &streams.tracks["one"];
        let two = &streams.tracks["two"];
        assert_eq!(one.name, two.name);
        assert_eq!((one.audio.base, two.audio.base), (16_000, 16_320));
        assert_eq!(one.audio.buf, vec![12.0 / 32768.0; 320]);
        assert_eq!(two.audio.buf, vec![24.0 / 32768.0; 320]);
    }

    #[test]
    fn missing_names_do_not_block_overlapping_audio_or_the_last_stop_window() {
        let mut streams = Streams::default();
        streams.ingest(frame("discord:1", None, 1_000, 0, vec![12; 320]), 0, None);
        streams.ingest(frame("discord:2", Some("Alex"), 1_010, 0, vec![24; 320]), 0, None);
        for track in streams.tracks.values() {
            assert!(!track.ready_to_decode(false, true, track.last_audio));
            assert!(track.ready_to_decode(true, false, track.last_audio), "Stop flushes even a short unnamed tail");
            assert!(track.ready_to_decode(false, true, track.last_audio + Duration::from_millis(UTTERANCE_GAP_MS)));
            assert!(!track.ready_to_decode(false, false, track.last_audio + Duration::from_millis(UTTERANCE_GAP_MS)));
        }
        assert_eq!(streams.tracks["discord:1"].audio.base, 16_000);
        assert_eq!(streams.tracks["discord:2"].audio.base, 16_160);
    }

    #[test]
    fn pause_discards_pending_audio_and_a_long_gap_finishes_the_previous_utterance() {
        let mut streams = Streams::default();
        streams.ingest(frame("one", Some("Alex"), 0, 0, vec![12; 320]), 1, None);
        streams.ingest(frame("one", Some("Alex"), 1_000, 2, vec![12; 320]), 2, None);
        streams.ingest(frame("one", Some("Alex"), 2_000, 2, vec![24; 320]), 2, None);
        assert_eq!(streams.finished.len(), 1);
        assert_eq!(streams.finished[0].1.base, 16_000);
        assert_eq!(streams.tracks["one"].audio.base, 32_000);
    }

    #[test]
    fn a_late_name_can_enter_while_the_pcm_queue_is_full() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (names, roster) = crossbeam_channel::bounded(1);
        let sender = ParticipantAudioSender {
            control: Arc::new(Control::default()),
            tx,
            names,
        };
        sender.try_send("one".into(), None, 0, HZ as u32, vec![12; 320]);
        sender.try_send("one".into(), Some(Arc::from("Alex")), 0, HZ as u32, vec![]);
        assert!(rx.is_full());
        assert_eq!(roster.try_recv().unwrap().name.as_deref(), Some("Alex"));
    }

    #[test]
    fn participant_sender_discards_paused_and_closed_frames() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let (names, roster) = crossbeam_channel::bounded(4);
        let control = Arc::new(Control::default());
        let sender = ParticipantAudioSender { control: control.clone(), tx, names };
        control.epoch.store(1, Ordering::Release);
        sender.try_send("one".into(), None, 0, HZ as u32, vec![12; 320]);
        sender.try_send("one".into(), Some(Arc::from("Alex")), 0, HZ as u32, vec![]);
        assert!(rx.is_empty());
        assert!(roster.is_empty());
        control.epoch.store(2, Ordering::Release);
        sender.try_send("one".into(), None, 1000, HZ as u32, vec![12; 320]);
        let accepted = rx.try_recv().unwrap();
        assert_eq!((accepted.epoch, accepted.start_ms), (2, 1000));
        control.closing.store(true, Ordering::Release);
        sender.try_send("one".into(), None, 1020, HZ as u32, vec![12; 320]);
        assert!(rx.is_empty());
    }
}

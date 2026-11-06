//! What the Meeting page shows while a recording is still running: the
//! captions so far and running notes the language model keeps updating.

use crate::events;
use crate::state::{AppState, RuntimeState};
use bazza_asr::Caption;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Update the notes this soon after a new committed line when something changed...
const SOON: Duration = Duration::from_secs(45);
/// ...and no later than this while new lines keep arriving.
const AT_MOST: Duration = Duration::from_secs(120);
/// A pass slower than this doubles the wait (CPU is busy with captions too).
const SLOW_PASS: Duration = Duration::from_secs(60);

pub(crate) type ConfirmedLiveSink = Arc<dyn Fn(&Caption, Option<&str>) -> bool + Send + Sync>;

#[derive(Default)]
pub struct LiveFeed {
    pub meeting_id: String,
    pub finals: Vec<LiveCaption>,
    pub partial: Option<Caption>,
    named_partials: BTreeMap<String, LiveCaption>,
    pub notes: String,
    pub notes_updated_ms: Option<u64>,
    caption_unavailable: Option<CaptionUnavailableReason>,
    local_model: Option<(String, Option<String>)>,
    confirmed_sink: Option<ConfirmedLiveSink>,
    /// Index into `finals` the notes were last computed from.
    notes_through: usize,
    started: Option<Instant>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionUnavailableReason {
    ModelMissing,
    ModelLoadFailed,
}

#[derive(Serialize, Clone)]
pub struct LiveSnapshot {
    pub meeting_id: String,
    pub finals: Vec<LiveCaption>,
    pub partial: Option<Caption>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub partials: Vec<LiveCaption>,
    pub notes: String,
    pub notes_updated_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption_unavailable: Option<CaptionUnavailableReason>,
}

#[derive(Serialize, Clone)]
pub struct LiveCaption {
    #[serde(flatten)]
    pub(crate) caption: Caption,
    #[serde(skip_serializing_if = "Option::is_none")]
    meeting_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speaker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speaker_name: Option<String>,
}

pub(crate) struct CompletedLiveTranscript {
    pub(crate) model: String,
    pub(crate) language: Option<String>,
    pub(crate) rows: Vec<LiveCaption>,
}

#[derive(Serialize, Clone)]
pub struct LiveNotesPayload {
    pub meeting_id: String,
    pub notes: String,
    pub updated_ms: u64,
}

impl LiveFeed {
    pub fn begin(state: &RuntimeState, meeting_id: &str) {
        Self::begin_inner(state, meeting_id, None);
    }

    pub(crate) fn begin_with_confirmed(state: &RuntimeState, meeting_id: &str, sink: ConfirmedLiveSink) {
        Self::begin_inner(state, meeting_id, Some(sink));
    }

    fn begin_inner(state: &RuntimeState, meeting_id: &str, confirmed_sink: Option<ConfirmedLiveSink>) {
        *state.live_feed.lock() = Some(LiveFeed {
            meeting_id: meeting_id.into(), started: Some(Instant::now()), confirmed_sink, ..Default::default()
        });
    }

    pub fn end(state: &RuntimeState) {
        *state.live_feed.lock() = None;
    }

    pub fn end_for(state: &RuntimeState, meeting_id: &str) {
        let mut feed = state.live_feed.lock();
        if feed.as_ref().is_some_and(|feed| feed.meeting_id == meeting_id) {
            *feed = None;
        }
    }

    pub fn snapshot(state: &RuntimeState) -> Option<LiveSnapshot> {
        state.live_feed.lock().as_ref().map(|f| LiveSnapshot {
            meeting_id: f.meeting_id.clone(),
            finals: f.finals.clone(),
            partial: f.partial.clone(),
            partials: f.named_partials.values().cloned().collect(),
            notes: f.notes.clone(),
            notes_updated_ms: f.notes_updated_ms,
            caption_unavailable: f.caption_unavailable,
        })
    }

    pub(crate) fn set_local_model(state: &RuntimeState, meeting_id: &str, model: &str, language: Option<&str>) {
        let mut current = state.live_feed.lock();
        if let Some(feed) = current.as_mut().filter(|feed| feed.meeting_id == meeting_id) {
            feed.local_model = Some((model.into(), language.map(str::to_owned)));
        }
    }

    pub(crate) fn completed_local(state: &RuntimeState, meeting_id: &str) -> Option<CompletedLiveTranscript> {
        let current = state.live_feed.lock();
        let feed = current.as_ref().filter(|feed| feed.meeting_id == meeting_id)?;
        let (model, language) = feed.local_model.as_ref()?;
        let mut rows: Vec<_> = feed.finals.iter().filter(|row| valid_final(&row.caption)).cloned().collect();
        if rows.is_empty() {
            return None;
        }
        rows.sort_by_key(|row| row.caption.start_ms);
        Some(CompletedLiveTranscript { model: model.clone(), language: language.clone(), rows })
    }

    pub fn set_caption_unavailable_for(
        state: &RuntimeState,
        meeting_id: &str,
        reason: CaptionUnavailableReason,
    ) {
        let mut feed = state.live_feed.lock();
        if let Some(feed) = feed.as_mut().filter(|feed| feed.meeting_id == meeting_id) {
            feed.caption_unavailable = Some(reason);
        }
    }

    /// Record a caption from the live transcriber (called for every emitted caption).
    pub fn push_for(state: &RuntimeState, meeting_id: &str, c: &Caption) -> bool {
        let mut g = state.live_feed.lock();
        let Some(f) = g.as_mut().filter(|feed| feed.meeting_id == meeting_id) else { return false };
        if c.is_final {
            if !c.text.trim().is_empty() && !valid_final(c) {
                return false;
            }
            f.partial = None;
            if valid_final(c) {
                if f.confirmed_sink.as_ref().is_some_and(|sink| !sink(c, None)) {
                    return false;
                }
                f.finals.push(LiveCaption {
                    caption: c.clone(), meeting_id: None, speaker_id: None, speaker_name: None,
                });
            }
        } else {
            f.partial = Some(c.clone());
        }
        true
    }

    pub(crate) fn publish_keyed(
        app: &AppHandle, state: &RuntimeState, meeting_id: &str, speaker_id: &str,
        speaker_name: Option<&str>, caption: Caption, is_current: impl FnOnce() -> bool,
    ) {
        let mut current = state.live_feed.lock();
        let Some(feed) = current.as_mut().filter(|f| f.meeting_id == meeting_id) else { return };
        if !is_current() { return; }
        let mut event = LiveCaption {
            caption, meeting_id: Some(meeting_id.into()),
            speaker_id: Some(speaker_id.into()), speaker_name: speaker_name.map(str::to_owned),
        };
        if event.caption.is_final {
            if !event.caption.text.trim().is_empty() && !valid_final(&event.caption) {
                return;
            }
            let partial = feed.named_partials.remove(speaker_id);
            if valid_final(&event.caption) {
                if feed.confirmed_sink.as_ref().is_some_and(|sink| !sink(&event.caption, speaker_name)) {
                    return;
                }
                feed.finals.push(event.clone());
            } else {
                let Some(partial) = partial else { return };
                event.caption.text.clear();
                event.caption.start_ms = partial.caption.start_ms;
                event.caption.end_ms = partial.caption.end_ms;
            }
        } else {
            feed.named_partials.insert(speaker_id.into(), event.clone());
        }
        let _ = app.emit(events::CAPTION, &event);
    }

    pub(crate) fn clear_named_partials(app: &AppHandle, state: &RuntimeState, meeting_id: &str) {
        let mut current = state.live_feed.lock();
        let Some(feed) = current.as_mut().filter(|f| f.meeting_id == meeting_id) else { return };
        for (_, mut event) in std::mem::take(&mut feed.named_partials) {
            event.caption.text.clear();
            event.caption.is_final = true;
            let _ = app.emit(events::CAPTION, &event);
        }
    }
}

fn valid_final(caption: &Caption) -> bool {
    caption.is_final && caption.start_ms >= 0 && caption.end_ms >= caption.start_ms && !caption.text.trim().is_empty()
}

/// Keep the running notes fresh until the feed ends. Runs on its own thread;
/// yields when there is no language model or nothing new was said.
pub fn spawn_notes_loop(app: AppHandle, meeting_id: String, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let mut last_run: Option<Instant> = None;
        let mut wait = SOON;
        let mut warned = false;
        loop {
            std::thread::sleep(Duration::from_secs(3));
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let (new_text, through, elapsed_ms) = {
                let g = state.live_feed.lock();
                let Some(f) = g.as_ref().filter(|f| f.meeting_id == meeting_id) else { return };
                if f.finals.len() <= f.notes_through {
                    continue;
                }
                let newest_end = f.finals.last().map(|c| c.caption.end_ms).unwrap_or(0);
                let since = last_run.map(|t| t.elapsed()).unwrap_or(Duration::MAX);
                // Fresh lines and a pause since the last pass, or the cap; whichever first.
                let due = since >= AT_MOST || (since >= wait && newest_end > 0);
                if !due {
                    continue;
                }
                let text: Vec<String> = f.finals[f.notes_through..].iter().map(|c| format!("[{}] {}", stamp(c.caption.start_ms), c.caption.text)).collect();
                (text.join("\n"), f.finals.len(), f.started.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0))
            };
            let Some(backend) = state.llm() else {
                if !warned {
                    warned = true;
                    crate::recorder::notice_message(&app, "info", crate::ui_message::app("native.liveNotesModel", "Live notes need a language model; download one in Settings."));
                }
                continue;
            };
            let previous = state.live_feed.lock().as_ref().map(|f| f.notes.clone()).unwrap_or_default();
            let _busy = state.busy_guard();
            let t0 = Instant::now();
            match bazza_llm::live_notes::update(backend.as_ref(), &previous, &new_text) {
                Ok(notes) => {
                    let mut g = state.live_feed.lock();
                    if let Some(f) = g.as_mut().filter(|f| f.meeting_id == meeting_id) {
                        f.notes = notes.clone();
                        f.notes_through = through;
                        f.notes_updated_ms = Some(elapsed_ms);
                    }
                    let _ = app.emit(events::LIVE_NOTES, LiveNotesPayload { meeting_id: meeting_id.clone(), notes, updated_ms: elapsed_ms });
                }
                Err(e) => log::warn!("live notes: {e}"),
            }
            last_run = Some(Instant::now());
            wait = if t0.elapsed() > SLOW_PASS { (wait * 2).min(AT_MOST) } else { SOON };
        }
    });
}

fn stamp(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

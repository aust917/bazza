use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use crate::{
    ingress::{Packet, PacketTimeline},
    tracks::MAX_PARTICIPANTS,
    RecordingClock, SAMPLE_RATE,
};

/// Nonblocking live PCM callback. An empty frame updates the name for an existing ID.
/// The callback must not call back into the recording session.
pub type LiveAudioSink = Arc<dyn Fn(String, Option<Arc<str>>, u64, u32, Vec<i16>) + Send + Sync>;

pub(crate) struct LiveAudio {
    sink: LiveAudioSink,
    clock: RecordingClock,
    timeline: PacketTimeline,
    heard: HashSet<String>,
    names: HashMap<String, Arc<str>>,
    roster_loaded: bool,
}

impl LiveAudio {
    pub fn new(sink: LiveAudioSink, clock: RecordingClock) -> Self {
        Self {
            sink,
            clock,
            timeline: PacketTimeline::default(),
            heard: HashSet::new(),
            names: HashMap::new(),
            roster_loaded: false,
        }
    }

    pub fn needs_names(&self) -> bool {
        (!self.roster_loaded || self.heard.iter().any(|id| !self.names.contains_key(id)))
            && self.clock.0.lock().observe(Instant::now()).is_some()
    }

    pub fn packet(&mut self, packet: &Packet) {
        let id = &packet.participant_id;
        if self.heard.len() >= MAX_PARTICIPANTS && !self.heard.contains(id) {
            return;
        }
        let samples = packet
            .pcm
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
            .collect();
        let clock = self.clock.0.lock();
        if !clock.accepts_epoch(packet.observation.epoch) {
            return;
        }
        let Some(start_ms) = self.timeline.position_ms(packet) else {
            return;
        };
        if !self.heard.contains(id) {
            self.heard.insert(id.clone());
        }
        (self.sink)(
            id.clone(),
            self.names.get(id).cloned(),
            start_ms,
            SAMPLE_RATE,
            samples,
        );
    }

    pub fn update_names(&mut self, names: HashMap<String, String>) {
        let mut names: HashMap<_, Arc<str>> = names
            .into_iter()
            .map(|(id, name)| (id, Arc::from(name)))
            .collect();
        for id in &self.heard {
            let previous = self.names.get(id);
            if let Some(name) = names.get(id) {
                if previous == Some(name) {
                    continue;
                }
                let clock = self.clock.0.lock();
                if let Some(observation) = clock.observe(Instant::now()) {
                    (self.sink)(
                        id.clone(),
                        Some(name.clone()),
                        observation.elapsed_ms,
                        SAMPLE_RATE,
                        Vec::new(),
                    );
                }
            } else if let Some(name) = previous {
                // A partial roster must not discard a name already reported for this ID.
                names.insert(id.clone(), name.clone());
            }
        }
        self.names = names;
        self.roster_loaded = true;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use parking_lot::Mutex;

    use super::*;
    use crate::ingress::Observation;

    fn frame(id: &str, elapsed_ms: u64, provider_ms: i64, epoch: u64) -> Packet {
        Packet {
            bot_id: "bot_test".into(),
            participant_id: id.into(),
            provider_ms,
            observation: Observation { elapsed_ms, epoch },
            pcm: vec![0xfe, 0xff],
        }
    }

    #[test]
    fn roster_resolves_audio_ids_even_after_speech_stops_and_keeps_duplicates_separate() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = events.clone();
        let mut live = LiveAudio::new(
            Arc::new(move |id, name, at, rate, pcm| {
                received.lock().push((id, name, at, rate, pcm))
            }),
            RecordingClock::from_origin(Instant::now(), 0),
        );
        live.update_names(HashMap::from([("known".into(), "Same name".into())]));
        live.packet(&frame("known", 20, 500, 0));
        live.packet(&frame("late", 40, 510, 0));
        assert!(live.needs_names());
        live.update_names(HashMap::from([("late".into(), "Same name".into())]));
        assert!(!live.needs_names());
        live.packet(&frame("known", 65, 520, 0));

        let events = events.lock();
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[0],
            (
                "known".into(),
                Some("Same name".into()),
                20,
                16_000,
                vec![-2]
            )
        );
        assert_eq!(events[1], ("late".into(), None, 30, 16_000, vec![-2]));
        assert_eq!(events[2].0, "late");
        assert_eq!(events[2].1.as_deref(), Some("Same name"));
        assert!(events[2].4.is_empty());
        assert_eq!(
            events[3],
            (
                "known".into(),
                Some("Same name".into()),
                40,
                16_000,
                vec![-2]
            )
        );
    }

    #[test]
    fn live_discards_paused_stopped_and_pre_resume_frames() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let clock = RecordingClock::from_origin(start, 0);
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = events.clone();
        let mut live = LiveAudio::new(
            Arc::new(move |_, _, at, _, _| received.lock().push(at)),
            clock.clone(),
        );
        live.packet(&frame("speaker", 10, 100, 0));
        clock.0.lock().set_paused(true, at(50));
        live.packet(&frame("speaker", 20, 110, 0));
        live.update_names(HashMap::from([("speaker".into(), "Actual Name".into())]));
        clock.0.lock().set_paused(false, at(150));
        live.packet(&frame("speaker", 30, 120, 0));
        live.packet(&frame("speaker", 60, 900, 1));
        clock.0.lock().stop(at(170));
        live.packet(&frame("speaker", 65, 905, 1));

        assert_eq!(*events.lock(), vec![10, 60]);
    }
}

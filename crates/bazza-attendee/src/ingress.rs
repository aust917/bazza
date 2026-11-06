use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::Url;
use serde::Deserialize;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        handshake::server::{Request, Response},
        protocol::WebSocketConfig,
        Message,
    },
};

use crate::{CaptureState, SAMPLE_RATE};

const MAX_CONNECTIONS: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_PCM_BYTES: usize = 8192;
const MAX_CLOCK_DRIFT_MS: u64 = 2_000;

pub(crate) struct Clock {
    started: Instant,
    active_since: Instant,
    origin_unix_micros: Option<u64>,
    paused_at: Option<Instant>,
    paused_total: Duration,
    stopped_at: Option<Instant>,
    epoch: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Observation {
    pub elapsed_ms: u64,
    pub epoch: u64,
}

pub(crate) struct ClockSnapshot {
    pub elapsed_ms: u64,
    pub origin_unix_micros: Option<u64>,
    pub excluded_pause_ms: u64,
}

impl Clock {
    pub fn new(started: Instant, origin_unix_micros: Option<u64>) -> Self {
        Self {
            started,
            active_since: started,
            origin_unix_micros,
            paused_at: None,
            paused_total: Duration::ZERO,
            stopped_at: None,
            epoch: 0,
        }
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        if self.stopped_at.is_some() {
            return;
        }
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(now),
            (false, Some(at)) => {
                self.paused_total += now.saturating_duration_since(at);
                self.paused_at = None;
                self.active_since = now;
                self.epoch += 1;
            }
            _ => {}
        }
    }

    pub fn stop(&mut self, now: Instant) {
        self.stopped_at.get_or_insert(now);
    }

    fn elapsed_ms(&self, at: Instant) -> u64 {
        self.paused_at
            .or(self.stopped_at)
            .unwrap_or(at)
            .saturating_duration_since(self.started)
            .saturating_sub(self.paused_total)
            .as_millis() as u64
    }

    pub fn snapshot(&self) -> ClockSnapshot {
        let end = self.stopped_at.unwrap_or_else(Instant::now);
        let open_pause = self
            .paused_at
            .map_or(Duration::ZERO, |at| end.saturating_duration_since(at));
        ClockSnapshot {
            elapsed_ms: self.elapsed_ms(end),
            origin_unix_micros: self.origin_unix_micros,
            excluded_pause_ms: (self.paused_total + open_pause).as_millis() as u64,
        }
    }

    pub fn observe(&self, at: Instant) -> Option<Observation> {
        (at >= self.active_since && self.paused_at.is_none() && self.stopped_at.is_none()).then(
            || Observation {
                elapsed_ms: self.elapsed_ms(at),
                epoch: self.epoch,
            },
        )
    }

    pub fn accepts_epoch(&self, epoch: u64) -> bool {
        self.epoch == epoch && self.paused_at.is_none() && self.stopped_at.is_none()
    }
}

#[derive(Deserialize)]
struct AudioEvent {
    bot_id: String,
    trigger: String,
    data: AudioData,
}

#[derive(Deserialize)]
struct AudioData {
    participant_uuid: String,
    timestamp_ms: i64,
    sample_rate: u32,
    chunk: String,
}

pub(crate) struct Packet {
    pub bot_id: String,
    pub participant_id: String,
    pub provider_ms: i64,
    pub observation: Observation,
    pub pcm: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct PacketTimeline {
    anchor: Option<Anchor>,
    pub reanchors: u64,
}

#[derive(Clone, Copy)]
struct Anchor {
    epoch: u64,
    provider_ms: i64,
    elapsed_ms: u64,
}

impl PacketTimeline {
    pub fn position_ms(&mut self, packet: &Packet) -> Option<u64> {
        let observation = packet.observation;
        let anchor = Anchor {
            epoch: observation.epoch,
            provider_ms: packet.provider_ms,
            elapsed_ms: observation.elapsed_ms,
        };
        if let Some(previous) = self.anchor {
            if observation.epoch < previous.epoch {
                return None;
            }
            if observation.epoch == previous.epoch {
                let relative = i128::from(packet.provider_ms) - i128::from(previous.provider_ms)
                    + i128::from(previous.elapsed_ms);
                if let Ok(relative) = u64::try_from(relative) {
                    if relative.abs_diff(observation.elapsed_ms) <= MAX_CLOCK_DRIFT_MS {
                        return Some(relative);
                    }
                }
                self.reanchors += 1;
            }
        }
        self.anchor = Some(anchor);
        Some(observation.elapsed_ms)
    }
}

impl Packet {
    fn parse(bytes: &[u8], observation: Observation) -> Option<Self> {
        let event: AudioEvent = serde_json::from_slice(bytes).ok()?;
        if event.trigger != "realtime_audio.per_participant"
            || event.data.sample_rate != SAMPLE_RATE
            || event.data.participant_uuid.is_empty()
            || event.data.participant_uuid.len() > 512
            || event.data.timestamp_ms < 0
        {
            return None;
        }
        let pcm = STANDARD.decode(event.data.chunk).ok()?;
        if pcm.is_empty() || pcm.len() % 2 != 0 || pcm.len() > MAX_PCM_BYTES {
            return None;
        }
        Some(Self {
            bot_id: event.bot_id,
            participant_id: event.data.participant_uuid,
            provider_ms: event.data.timestamp_ms,
            observation,
            pcm,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Ingress {
    pub acceptor: TlsAcceptor,
    pub callback: Url,
    pub capture: Arc<Mutex<CaptureState>>,
    pub packets: mpsc::Sender<Packet>,
    pub dropped: Arc<AtomicU64>,
}

impl Ingress {
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                stream = listener.accept() => {
                    let (stream, _) = stream?;
                    if connections.len() < MAX_CONNECTIONS {
                        let ingress = self.clone();
                        connections.spawn(async move { ingress.receive(stream).await });
                    }
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }

    #[allow(clippy::result_large_err)] // Tungstenite's handshake callback returns an HTTP response.
    async fn receive(self, stream: TcpStream) {
        let handshake = async {
            let stream = self.acceptor.accept(stream).await.ok()?;
            let websocket = WebSocketConfig::default()
                .max_frame_size(Some(MAX_MESSAGE_BYTES))
                .max_message_size(Some(MAX_MESSAGE_BYTES));
            accept_hdr_async_with_config(
                stream,
                |request: &Request, response: Response| {
                    if request.uri().path() == self.callback.path()
                        && request.uri().query() == self.callback.query()
                    {
                        Ok(response)
                    } else {
                        Err(tokio_tungstenite::tungstenite::http::Response::builder()
                            .status(404)
                            .body(None)
                            .expect("valid fixed HTTP status"))
                    }
                },
                Some(websocket),
            )
            .await
            .ok()
        };
        let Ok(Some(mut websocket)) = timeout(HANDSHAKE_TIMEOUT, handshake).await else {
            return;
        };
        while let Some(Ok(message)) = websocket.next().await {
            let received_at = Instant::now();
            let bytes = match &message {
                Message::Text(text) => text.as_bytes(),
                Message::Binary(bytes) => bytes.as_ref(),
                Message::Close(_) => break,
                _ => continue,
            };
            let Some(observation) = self.capture.lock().observe(received_at) else {
                continue;
            };
            if let Some(packet) = Packet::parse(bytes, observation) {
                match self.packets.try_send(packet) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_excludes_pauses_before_audio_and_at_stop() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut clock = Clock::new(start, Some(1_793_750_400_000_000));
        clock.set_paused(true, at(100));
        clock.set_paused(false, at(300));
        clock.set_paused(true, at(900));
        clock.stop(at(1_100));
        clock.set_paused(false, at(1_300));

        let snapshot = clock.snapshot();
        assert_eq!(snapshot.elapsed_ms, 700);
        assert_eq!(snapshot.excluded_pause_ms, 400);
        assert_eq!(snapshot.origin_unix_micros, Some(1_793_750_400_000_000));
    }
}

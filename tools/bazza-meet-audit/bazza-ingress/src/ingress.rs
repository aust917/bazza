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

use crate::{Audit, CaptureState, SAMPLE_RATE};

const MAX_CONNECTIONS: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_PCM_BYTES: usize = 8192;

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
    pub audit_id: Option<u64>,
    pub bot_id: String,
    pub participant_id: String,
    pub provider_ms: i64,
    pub observation: Observation,
    pub pcm: Vec<u8>,
}

impl Packet {
    fn parse(
        bytes: &[u8],
        observation: Observation,
        audit_id: Option<u64>,
    ) -> Result<Self, &'static str> {
        let event: AudioEvent = serde_json::from_slice(bytes).map_err(|_| "rejected_json")?;
        if event.trigger != "realtime_audio.per_participant" {
            return Err("rejected_trigger");
        }
        if event.data.sample_rate != SAMPLE_RATE {
            return Err("rejected_sample_rate");
        }
        if event.data.participant_uuid.is_empty() || event.data.participant_uuid.len() > 512 {
            return Err("rejected_participant");
        }
        if event.data.timestamp_ms < 0 {
            return Err("rejected_timestamp");
        }
        let pcm = STANDARD
            .decode(event.data.chunk)
            .map_err(|_| "rejected_base64")?;
        if pcm.is_empty() || pcm.len() % 2 != 0 || pcm.len() > MAX_PCM_BYTES {
            return Err("rejected_pcm_size");
        }
        Ok(Self {
            audit_id,
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
    pub audit: Option<Audit>,
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
                        if let Some(audit) = &self.audit {
                            audit.mark("tcp_accepted");
                        }
                        let ingress = self.clone();
                        connections.spawn(async move { ingress.receive(stream).await });
                    } else if let Some(audit) = &self.audit {
                        audit.mark("tcp_capacity_rejected");
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
            if let Some(audit) = &self.audit {
                audit.mark("wss_handshake_rejected");
            }
            return;
        };
        if let Some(audit) = &self.audit {
            audit.mark("wss_handshake_accepted");
        }
        while let Some(result) = websocket.next().await {
            let received_at = Instant::now();
            let message = match result {
                Ok(message) => message,
                Err(_) => {
                    if let Some(audit) = &self.audit {
                        audit.mark_at("wss_read_error", received_at);
                    }
                    break;
                }
            };
            let (kind, bytes) = match &message {
                Message::Text(text) => ("raw_text", text.as_bytes()),
                Message::Binary(bytes) => ("raw_binary", bytes.as_ref()),
                Message::Close(_) => {
                    if let Some(audit) = &self.audit {
                        audit.mark_at("wss_peer_close", received_at);
                    }
                    break;
                }
                _ => continue,
            };
            let packet_id = self
                .audit
                .as_ref()
                .map(|audit| audit.raw(kind, received_at, bytes.len()));
            let trace = |event, bytes, provider_ms, elapsed_ms| {
                if let Some((audit, id)) = self.audit.as_ref().zip(packet_id) {
                    audit.packet(event, id, bytes, provider_ms, elapsed_ms);
                }
            };
            let (observation, reason) = {
                let capture = self.capture.lock();
                let reason = if capture.closed {
                    "rejected_session_closed"
                } else if capture.target.is_none() {
                    "rejected_before_adopt"
                } else if capture
                    .target
                    .as_ref()
                    .is_some_and(|t| received_at < t.attached_at)
                {
                    "rejected_before_attach_time"
                } else {
                    "rejected_recording_inactive"
                };
                (capture.observe(received_at), reason)
            };
            let Some(observation) = observation else {
                trace(reason, None, None, None);
                continue;
            };
            let packet = match Packet::parse(bytes, observation, packet_id) {
                Ok(packet) => packet,
                Err(reason) => {
                    trace(reason, None, None, Some(observation.elapsed_ms));
                    continue;
                }
            };
            trace(
                "accepted_pcm",
                Some(packet.pcm.len()),
                Some(packet.provider_ms),
                Some(observation.elapsed_ms),
            );
            match self.packets.try_send(packet) {
                Ok(()) => trace("queued_ingress", None, None, None),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    trace("queue_ingress_full", None, None, None);
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    trace("queue_ingress_closed", None, None, None);
                    break;
                }
            }
        }
        if let Some(audit) = &self.audit {
            audit.mark("wss_end");
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

//! Opt-in Soniox streaming adapter. No network access occurs until `start` is
//! called by an explicitly selected cloud session. Official protocol checked
//! 2026-11-01: https://docs.soniox.com/api-reference/stt/websocket-api
//! Authentication: https://docs.soniox.com/guides/websocket-authentication
use crate::{provider::SttEvent, Caption};
use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

pub const ENDPOINT: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
pub const DEFAULT_MODEL: &str = "stt-rt-v5";
const HZ: u64 = 16_000;
const MAX_BUFFER: usize = 60 * HZ as usize;
const STOPPED: &str =
    "Soniox stopped because permission or credentials changed, cancellation was requested, or the input backlog exceeded its limit; local audio is safe";

/// Deliberately not Debug/Serialize: credentials never enter logs or transcript metadata.
pub struct SonioxConfig {
    pub api_key: String,
    pub model: String,
    pub language: Option<String>,
}

pub struct SonioxWorker {
    handle: Option<JoinHandle<()>>,
    pub cancelled: Arc<AtomicBool>,
}
impl SonioxWorker {
    pub fn start(config: SonioxConfig, rx: Receiver<Vec<f32>>, tx: Sender<SttEvent>, cancelled: Arc<AtomicBool>) -> Self {
        let cancel = cancelled.clone();
        let handle = std::thread::spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(run(config, rx, &tx, cancel)),
                Err(_) => Err("Soniox worker could not start".into()),
            };
            let _ = tx.send(SttEvent::Finished(result));
        });
        Self { handle: Some(handle), cancelled }
    }
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
impl Drop for SonioxWorker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct ReplayBuffer {
    base: u64,
    samples: Vec<f32>,
}
impl ReplayBuffer {
    fn end(&self) -> u64 {
        self.base + self.samples.len() as u64
    }
    fn append(&mut self, samples: Vec<f32>) -> Result<(), String> {
        if self.samples.len().saturating_add(samples.len()) > MAX_BUFFER {
            return Err("Soniox backlog exceeded 60 seconds; local audio is safe".into());
        }
        self.samples.extend(samples);
        Ok(())
    }
    fn acknowledge(&mut self, through: u64) {
        let count = through.saturating_sub(self.base).min(self.samples.len() as u64) as usize;
        self.samples.drain(..count);
        self.base += count as u64;
    }
    fn bytes(&self, start: u64, end: u64) -> Vec<u8> {
        self.samples[(start - self.base) as usize..(end - self.base) as usize]
            .iter()
            .flat_map(|s| {
                let value = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
                value.to_le_bytes()
            })
            .collect()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Response {
    tokens: Vec<Token>,
    final_audio_proc_ms: u64,
    finished: bool,
    error_code: Option<u16>,
    error_type: Option<String>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Token {
    text: String,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    is_final: bool,
}

#[derive(Debug, PartialEq)]
enum Failure {
    Retry,
    Fatal(String),
}
impl Response {
    fn check_error(&self) -> Result<(), Failure> {
        let Some(code) = self.error_code else { return Ok(()) };
        match self.error_type.as_deref() {
            Some("service_unavailable" | "internal_error" | "request_timeout" | "max_duration_reached") => Err(Failure::Retry),
            _ if code == 429 || code >= 500 => Err(Failure::Retry),
            _ => Err(Failure::Fatal(format!("Soniox rejected the stream (status {code}); check credentials, balance and model settings"))),
        }
    }
    fn captions(&self, offset_samples: u64, sent_samples: u64, replay_cutoff_ms: i64) -> Result<Vec<Caption>, Failure> {
        let invalid = || Failure::Fatal("Soniox returned invalid transcript timestamps".into());
        let offset_ms = i64::try_from(offset_samples.checked_mul(1000).ok_or_else(invalid)? / HZ).map_err(|_| invalid())?;
        let max_ms = i64::try_from(sent_samples.checked_mul(1000).ok_or_else(invalid)? / HZ).map_err(|_| invalid())?.checked_add(1).ok_or_else(invalid)?;
        let mut validated = Vec::new();
        let mut previous = [(0, 0), (0, 0)];
        for token in &self.tokens {
            if matches!(token.text.as_str(), "<end>" | "<fin>") {
                continue;
            }
            let (Some(start), Some(end)) = (token.start_ms, token.end_ms) else { return Err(invalid()) };
            let prior = &mut previous[usize::from(token.is_final)];
            let absolute_start = offset_ms.checked_add(start).ok_or_else(invalid)?;
            let absolute_end = offset_ms.checked_add(end).ok_or_else(invalid)?;
            if start < 0 || end < start || start < prior.0 || end < prior.1 || absolute_end > max_ms {
                return Err(invalid());
            }
            *prior = (start, end);
            validated.push((token, absolute_start, absolute_end));
        }
        let mut out = Vec::new();
        for is_final in [true, false] {
            let tokens: Vec<_> = validated.iter().filter(|(token, _, end)| token.is_final == is_final && *end > replay_cutoff_ms).collect();
            if tokens.is_empty() {
                continue;
            }
            let start_ms = tokens.first().unwrap().1;
            let end_ms = tokens.last().unwrap().2;
            let text = tokens.iter().map(|(token, _, _)| token.text.as_str()).collect::<String>();
            if !text.trim().is_empty() {
                out.push(Caption { start_ms, end_ms, text: text.trim().into(), is_final });
            }
        }
        Ok(out)
    }
}

async fn run(config: SonioxConfig, rx: Receiver<Vec<f32>>, tx: &Sender<SttEvent>, cancelled: Arc<AtomicBool>) -> Result<(), String> {
    let mut state = StreamState::default();
    let mut retries = 0;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(STOPPED.into());
        }
        let result = stream_once(&config, &rx, tx, &cancelled, &mut state).await;
        match result {
            Ok(()) => return Ok(()),
            Err(Failure::Fatal(message)) => return Err(message),
            Err(Failure::Retry) if retries < 3 => {
                retries += 1;
                let _ = tx.send(SttEvent::Reconnecting);
                tokio::select! {
                    _ = wait_cancelled(&cancelled) => return Err(STOPPED.into()),
                    _ = tokio::time::sleep(Duration::from_millis(250 * (1 << retries))) => {}
                }
            }
            Err(Failure::Retry) => return Err("Soniox connection failed after three retries; confirmed text and local audio were kept".into()),
        }
    }
}

async fn wait_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Permission revocation interrupts handshakes and blocked writes as well as
/// the audio loop. No UI command joins the network worker.
async fn interruptible<T>(cancelled: &AtomicBool, future: impl std::future::Future<Output = Result<T, Failure>>) -> Result<T, Failure> {
    if cancelled.load(Ordering::Acquire) {
        return Err(Failure::Fatal(STOPPED.into()));
    }
    tokio::select! {
        biased;
        _ = wait_cancelled(cancelled) => Err(Failure::Fatal(STOPPED.into())),
        result = future => result,
    }
}

#[derive(Default)]
struct StreamState {
    buffer: ReplayBuffer,
    closed_input: bool,
    final_end_ms: i64,
}

async fn stream_once(
    config: &SonioxConfig,
    rx: &Receiver<Vec<f32>>,
    tx: &Sender<SttEvent>,
    cancelled: &AtomicBool,
    state: &mut StreamState,
) -> Result<(), Failure> {
    let mut request = ENDPOINT.into_client_request().map_err(|_| Failure::Fatal("Invalid Soniox endpoint".into()))?;
    let mut authorization = format!("Bearer {}", config.api_key)
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        .map_err(|_| Failure::Fatal("Invalid Soniox API key".into()))?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("Authorization", authorization);
    let (socket, _) = interruptible(cancelled, async {
        tokio::time::timeout(Duration::from_secs(8), tokio_tungstenite::connect_async(request)).await.map_err(|_| Failure::Retry)?.map_err(|_| Failure::Retry)
    })
    .await?;
    stream_socket(config, rx, tx, cancelled, state, socket).await
}

async fn stream_socket<S>(
    config: &SonioxConfig,
    rx: &Receiver<Vec<f32>>,
    tx: &Sender<SttEvent>,
    cancelled: &AtomicBool,
    state: &mut StreamState,
    mut socket: tokio_tungstenite::WebSocketStream<S>,
) -> Result<(), Failure>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let StreamState { buffer, closed_input, final_end_ms } = state;
    let mut start =
        serde_json::json!({ "model": config.model, "audio_format": "pcm_f32le", "sample_rate": HZ, "num_channels": 1, "enable_endpoint_detection": true });
    if let Some(language) = &config.language {
        start["language_hints"] = serde_json::json!([language]);
    }
    interruptible(cancelled, async {
        tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(start.to_string().into())))
            .await
            .map_err(|_| Failure::Retry)?
            .map_err(|_| Failure::Retry)
    })
    .await?;
    let offset = buffer.base;
    let replay_cutoff = *final_end_ms;
    let mut sent = offset;
    let mut finishing: Option<tokio::time::Instant> = None;
    let mut last_send = tokio::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        tokio::select! {
            biased;
            _ = wait_cancelled(cancelled) => return Err(Failure::Fatal(STOPPED.into())),
            _ = tick.tick() => {
                if cancelled.load(Ordering::Acquire) { return Err(Failure::Fatal(STOPPED.into())); }
                for _ in 0..256 {
                    match rx.try_recv() {
                        Ok(pcm) => buffer.append(pcm).map_err(Failure::Fatal)?,
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => { *closed_input = true; break; }
                    }
                }
                let message = if sent < buffer.end() {
                    let end = (sent + 3200).min(buffer.end());
                    let bytes = buffer.bytes(sent, end);
                    sent = end;
                    Some(Message::Binary(bytes.into()))
                } else if *closed_input && finishing.is_none() {
                    finishing = Some(tokio::time::Instant::now());
                    Some(Message::Text("".into()))
                } else if finishing.is_none() && last_send.elapsed() >= Duration::from_secs(10) {
                    Some(Message::Text(r#"{"type":"keepalive"}"#.into()))
                } else { None };
                if let Some(message) = message {
                    interruptible(cancelled, async {
                        tokio::time::timeout(Duration::from_secs(5), socket.send(message)).await.map_err(|_| Failure::Retry)?.map_err(|_| Failure::Retry)
                    }).await?;
                    last_send = tokio::time::Instant::now();
                }
                if finishing.is_some_and(|t| t.elapsed() > Duration::from_secs(15)) { return Err(Failure::Retry); }
            }
            response = socket.next() => {
                let Some(Ok(message)) = response else { return Err(Failure::Retry) };
                match message {
                    Message::Text(json) => {
                        let response: Response = serde_json::from_str(&json).map_err(|_| Failure::Fatal("Soniox returned an invalid response".into()))?;
                        response.check_error()?;
                        for cap in response.captions(offset, sent, replay_cutoff)? {
                            if cap.is_final { *final_end_ms = (*final_end_ms).max(cap.end_ms); }
                            let _ = tx.send(SttEvent::Caption(cap));
                        }
                        let acknowledged = offset.saturating_add(response.final_audio_proc_ms.saturating_mul(HZ) / 1000);
                        if acknowledged > sent { return Err(Failure::Fatal("Soniox returned an invalid audio watermark".into())); }
                        buffer.acknowledge(acknowledged);
                        if response.finished {
                            return if finishing.is_some() { Ok(()) } else { Err(Failure::Retry) };
                        }
                    }
                    Message::Close(_) => return Err(Failure::Retry),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reconnect_replays_only_unacknowledged_audio_and_rebases_tokens() {
        let mut buffer = ReplayBuffer::default();
        buffer.append(vec![0.1; 32_000]).unwrap();
        buffer.acknowledge(16_000);
        assert_eq!(buffer.base, 16_000);
        assert_eq!(buffer.bytes(buffer.base, buffer.end()).len(), 16_000 * 4);
        let response: Response =
            serde_json::from_str(r#"{"tokens":[{"text":"next words","start_ms":100,"end_ms":500,"is_final":true}],"final_audio_proc_ms":500}"#).unwrap();
        let captions = response.captions(buffer.base, buffer.end(), 1000).unwrap();
        assert_eq!((captions[0].start_ms, captions[0].end_ms), (1100, 1500));
    }
    #[test]
    fn partial_tokens_replace_instead_of_becoming_final_and_markers_are_hidden() {
        let response: Response = serde_json::from_str(r#"{"tokens":[{"text":" Hello","start_ms":0,"end_ms":300,"is_final":true},{"text":" world","start_ms":300,"end_ms":700,"is_final":false},{"text":"<end>","is_final":true}]}"#).unwrap();
        let caps = response.captions(0, 16_000, 0).unwrap();
        assert_eq!(caps.len(), 2);
        assert!(caps[0].is_final);
        assert!(!caps[1].is_final);
        assert_eq!(caps[1].text, "world");
    }
    #[test]
    fn backlog_is_bounded_and_errors_do_not_echo_server_text() {
        let mut buffer = ReplayBuffer::default();
        assert!(buffer.append(vec![0.0; MAX_BUFFER]).is_ok());
        assert!(buffer.append(vec![0.0]).is_err());
        let response: Response =
            serde_json::from_str(r#"{"error_code":401,"error_type":"unauthenticated","error_message":"sensitive key or transcript"}"#).unwrap();
        let error = response.check_error().unwrap_err();
        assert!(!format!("{error:?}").contains("sensitive"));
        assert_eq!(Response { error_code: Some(503), error_type: Some("service_unavailable".into()), ..Default::default() }.check_error(), Err(Failure::Retry));
    }
    #[test]
    fn invalid_timestamps_are_rejected() {
        let response: Response = serde_json::from_str(r#"{"tokens":[{"text":"bad","start_ms":1000,"end_ms":900,"is_final":true}]}"#).unwrap();
        assert!(response.captions(0, 16_000, 0).is_err());
        let intermediate: Response = serde_json::from_str(r#"{"tokens":[{"text":"valid","start_ms":0,"end_ms":100,"is_final":true},{"text":"bad","start_ms":-2,"end_ms":-1,"is_final":true},{"text":"valid","start_ms":200,"end_ms":300,"is_final":true}]}"#).unwrap();
        assert!(intermediate.captions(0, 16_000, 500).is_err());
        let overflow: Response = serde_json::from_str(r#"{"tokens":[{"text":"bad","start_ms":0,"end_ms":9223372036854775807,"is_final":true}]}"#).unwrap();
        assert!(overflow.captions(16_000, 32_000, 0).is_err());
    }
    #[test]
    fn reconnect_does_not_duplicate_confirmed_tokens_when_watermark_lags() {
        let response: Response = serde_json::from_str(
            r#"{"tokens":[{"text":"old","start_ms":0,"end_ms":500,"is_final":true},{"text":" new","start_ms":500,"end_ms":900,"is_final":true}]}"#,
        )
        .unwrap();
        let caps = response.captions(0, 16_000, 500).unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].text, "new");
    }

    async fn fake_socket() -> (tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>, tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>) {
        let (client, server) = tokio::io::duplex(1 << 20);
        let client = tokio_tungstenite::WebSocketStream::from_raw_socket(client, tokio_tungstenite::tungstenite::protocol::Role::Client, None).await;
        let server = tokio_tungstenite::WebSocketStream::from_raw_socket(server, tokio_tungstenite::tungstenite::protocol::Role::Server, None).await;
        (client, server)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fake_provider_streams_pcm_and_waits_for_finished() {
        let (client, mut server) = fake_socket().await;
        let (pcm_tx, pcm_rx) = crossbeam_channel::unbounded();
        pcm_tx.send(vec![0.1; 1600]).unwrap();
        drop(pcm_tx);
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = StreamState::default();
        let config = SonioxConfig { api_key: "never-sent-in-config".into(), model: DEFAULT_MODEL.into(), language: Some("en".into()) };
        let cancelled = AtomicBool::new(false);
        let client_run = stream_socket(&config, &pcm_rx, &tx, &cancelled, &mut state, client);
        let server_run = async {
            let config = server.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(!config.contains("api_key"));
            assert!(config.contains("pcm_f32le"));
            let audio = server.next().await.unwrap().unwrap().into_data();
            assert_eq!(audio.len(), 1600 * 4);
            assert_eq!(server.next().await.unwrap().unwrap(), Message::Text("".into()));
            server
                .send(Message::Text(
                    r#"{"tokens":[{"text":"Confirmed","start_ms":0,"end_ms":100,"is_final":true}],"final_audio_proc_ms":100,"finished":true}"#.into(),
                ))
                .await
                .unwrap();
        };
        let (result, _) = tokio::join!(client_run, server_run);
        assert_eq!(result, Ok(()));
        assert_eq!(state.buffer.base, 1600);
        assert!(matches!(rx.try_recv().unwrap(), SttEvent::Caption(Caption { is_final: true, .. })));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fake_provider_disconnect_replays_unconfirmed_audio_on_a_new_session() {
        let (client, mut server) = fake_socket().await;
        let (pcm_tx, pcm_rx) = crossbeam_channel::unbounded();
        pcm_tx.send(vec![0.1; 6400]).unwrap();
        drop(pcm_tx);
        let (tx, rx) = crossbeam_channel::unbounded();
        let config = SonioxConfig { api_key: String::new(), model: DEFAULT_MODEL.into(), language: None };
        let cancelled = AtomicBool::new(false);
        let mut state = StreamState::default();
        let server_run = async {
            server.next().await.unwrap().unwrap();
            server.next().await.unwrap().unwrap();
            server
                .send(Message::Text(r#"{"tokens":[{"text":"First","start_ms":0,"end_ms":100,"is_final":true}],"final_audio_proc_ms":100}"#.into()))
                .await
                .unwrap();
            server.close(None).await.unwrap();
        };
        let (first, _) = tokio::join!(stream_socket(&config, &pcm_rx, &tx, &cancelled, &mut state, client), server_run);
        assert_eq!(first, Err(Failure::Retry));
        assert_eq!(state.buffer.base, 1600);
        let (client, mut server) = fake_socket().await;
        let server_run = async {
            server.next().await.unwrap().unwrap();
            let mut bytes = 0;
            loop {
                match server.next().await.unwrap().unwrap() {
                    Message::Binary(data) => bytes += data.len(),
                    Message::Text(text) if text.is_empty() => break,
                    _ => {}
                }
            }
            assert_eq!(bytes, (6400 - 1600) * 4);
            server
                .send(Message::Text(
                    r#"{"tokens":[{"text":"Second","start_ms":0,"end_ms":300,"is_final":true}],"final_audio_proc_ms":300,"finished":true}"#.into(),
                ))
                .await
                .unwrap();
        };
        let (second, _) = tokio::join!(stream_socket(&config, &pcm_rx, &tx, &cancelled, &mut state, client), server_run);
        assert_eq!(second, Ok(()));
        let caps: Vec<_> = rx
            .try_iter()
            .filter_map(|e| match e {
                SttEvent::Caption(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(caps.len(), 2);
        assert_eq!((caps[1].start_ms, caps[1].end_ms), (100, 400));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fake_provider_auth_error_is_redacted_and_cancellation_finishes() {
        let (client, mut server) = fake_socket().await;
        let (_pcm_tx, pcm_rx) = crossbeam_channel::unbounded();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let config = SonioxConfig { api_key: "private-key".into(), model: DEFAULT_MODEL.into(), language: None };
        let mut state = StreamState::default();
        let cancelled = AtomicBool::new(false);
        let server_run = async {
            server.next().await.unwrap().unwrap();
            server.send(Message::Text(r#"{"error_code":401,"error_type":"unauthenticated","error_message":"private-key transcript"}"#.into())).await.unwrap();
        };
        let (error, _) = tokio::join!(stream_socket(&config, &pcm_rx, &tx, &cancelled, &mut state, client), server_run);
        assert!(matches!(error, Err(Failure::Fatal(_))));
        assert!(!format!("{error:?}").contains("private-key"));
        let (client, _server) = fake_socket().await;
        cancelled.store(true, Ordering::Relaxed);
        let result = stream_socket(&config, &pcm_rx, &tx, &cancelled, &mut state, client).await;
        assert!(matches!(result, Err(Failure::Fatal(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn revocation_interrupts_a_pending_network_operation() {
        let cancelled = AtomicBool::new(false);
        let operation = interruptible(&cancelled, std::future::pending::<Result<(), Failure>>());
        let revoke = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            cancelled.store(true, Ordering::Release);
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(operation, revoke) }).await.unwrap();
        assert!(matches!(result, Err(Failure::Fatal(_))));
    }
}

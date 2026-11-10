use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

const QUEUE_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct Audit(Arc<Shared>);

struct Shared {
    sender: SyncSender<Message>,
    start: Instant,
    sequence: AtomicU64,
    dropped: Arc<AtomicU64>,
}

pub struct AuditLog {
    audit: Audit,
    writer: JoinHandle<io::Result<()>>,
}

enum Message {
    Event(Event),
    Stop,
}

#[derive(Serialize)]
struct Event {
    event: &'static str,
    monotonic_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_unix_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    packet: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recording_elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dropped_audit_events: Option<u64>,
}

impl Event {
    fn new(event: &'static str, start: Instant, at: Instant) -> Self {
        Self {
            event,
            monotonic_us: at.saturating_duration_since(start).as_micros() as u64,
            origin_unix_us: None,
            packet: None,
            bytes: None,
            provider_ms: None,
            recording_elapsed_ms: None,
            dropped_audit_events: None,
        }
    }
}

impl Audit {
    pub fn create(path: &Path) -> io::Result<(Self, AuditLog)> {
        let start = Instant::now();
        let origin_unix_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_micros() as u64;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let audit = Self(Arc::new(Shared {
            sender,
            start,
            sequence: AtomicU64::new(1),
            dropped: dropped.clone(),
        }));
        let writer = thread::Builder::new()
            .name("bazza-meet-audit-jsonl".into())
            .spawn(move || {
                let mut output = BufWriter::new(file);
                while let Ok(message) = receiver.recv() {
                    match message {
                        Message::Event(event) => {
                            let flush = event.event == "audit_start" || event.event == "adopted";
                            serde_json::to_writer(&mut output, &event).map_err(io::Error::other)?;
                            output.write_all(b"\n")?;
                            if flush {
                                output.flush()?;
                            }
                        }
                        Message::Stop => break,
                    }
                }
                let mut end = Event::new("audit_end", start, Instant::now());
                end.dropped_audit_events = Some(dropped.load(Ordering::Relaxed));
                serde_json::to_writer(&mut output, &end).map_err(io::Error::other)?;
                output.write_all(b"\n")?;
                output.flush()
            })?;
        let mut first = audit.event_at("audit_start", start);
        first.origin_unix_us = Some(origin_unix_us);
        audit.send(first);
        Ok((audit.clone(), AuditLog { audit, writer }))
    }

    pub fn mark(&self, event: &'static str) {
        self.mark_at(event, Instant::now());
    }

    pub(crate) fn mark_at(&self, event: &'static str, at: Instant) {
        self.send(self.event_at(event, at));
    }

    pub(crate) fn adopted(&self, at: Instant, origin_unix_us: Option<u64>) {
        let mut event = self.event_at("adopted", at);
        event.origin_unix_us = origin_unix_us;
        self.send(event);
    }

    pub(crate) fn raw(&self, event: &'static str, at: Instant, bytes: usize) -> u64 {
        let packet = self.0.sequence.fetch_add(1, Ordering::Relaxed);
        let mut trace = self.event_at(event, at);
        trace.packet = Some(packet);
        trace.bytes = Some(bytes);
        self.send(trace);
        packet
    }

    pub(crate) fn packet(
        &self,
        event: &'static str,
        packet: u64,
        bytes: Option<usize>,
        provider_ms: Option<i64>,
        recording_elapsed_ms: Option<u64>,
    ) {
        let mut trace = self.event_at(event, Instant::now());
        trace.packet = Some(packet);
        trace.bytes = bytes;
        trace.provider_ms = provider_ms;
        trace.recording_elapsed_ms = recording_elapsed_ms;
        self.send(trace);
    }

    fn event_at(&self, event: &'static str, at: Instant) -> Event {
        Event::new(event, self.0.start, at)
    }

    fn send(&self, event: Event) {
        if matches!(
            self.0.sender.try_send(Message::Event(event)),
            Err(TrySendError::Full(_))
        ) {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl AuditLog {
    pub fn finish(self) -> io::Result<()> {
        let _ = self.audit.0.sender.send(Message::Stop);
        self.writer
            .join()
            .map_err(|_| io::Error::other("audit writer panicked"))?
    }
}

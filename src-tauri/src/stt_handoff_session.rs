//! Native worker lifecycle for an already authorized service capture.

use super::{append_caption, drain_soniox_events, SonioxHandoffCancellation};
use bazza_asr::{
    provider::SttEvent,
    soniox::{SonioxConfig, SonioxWorker},
    Caption,
};
use bazza_capture_handoff::{
    ArtifactGuard, CaptureIdentity, HandoffError, ProviderProvenance, SttProvider,
};
use bazza_domain::capture_stt::{SonioxCaptureOutcome, SonioxIncompleteReason};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TrySendError};
use parking_lot::{Condvar, Mutex};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

const PCM_QUEUE: usize = 256;
const MAX_CONTROL_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SonioxPermissionBinding {
    pub capture_id: Uuid,
    pub artifact: CaptureIdentity,
    pub service_epoch: Uuid,
    pub permission_epoch: Uuid,
    pub model: String,
    pub language: Option<String>,
}

/// Native projection of a verified, short-lived service permission. Use a
/// monotonic deadline derived from the service reply on receipt. This struct
/// is not a network credential or a Tauri command argument.
#[derive(Clone, Debug)]
pub struct SonioxCapturePermit {
    pub binding: SonioxPermissionBinding,
    pub expires_at: Instant,
}

impl SonioxCapturePermit {
    fn live_at(&self, now: Instant) -> bool {
        self.expires_at
            .checked_duration_since(now)
            .is_some_and(|left| !left.is_zero() && left <= MAX_CONTROL_WINDOW)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SonioxSessionError {
    #[error("invalid or expired Soniox capture permission")]
    InvalidPermit,
    #[error("could not start Soniox capture control")]
    Start(#[from] io::Error),
    #[error("confirmed Soniox journal could not be completed")]
    Journal(#[from] HandoffError),
    #[error("Soniox caption consumer ended unexpectedly")]
    JournalPanicked,
}

struct Gate {
    permit: SonioxCapturePermit,
    pcm: Option<Sender<Vec<f32>>>,
    finished: bool,
}

struct Control {
    gate: Mutex<Gate>,
    changed: Condvar,
    cancellation: SonioxHandoffCancellation,
}

#[derive(Clone)]
pub struct SonioxCaptureControl(Arc<Control>);

impl SonioxCaptureControl {
    /// Used on the audio forwarder, outside the device callback and local
    /// writer. Only queue admission is synchronized with renewal/revocation.
    /// The gate never holds an RPC or I/O operation and the queue cannot block.
    pub fn try_send_pcm(&self, pcm: Vec<f32>) -> bool {
        let mut gate = self.0.gate.lock();
        if gate.finished || self.0.cancellation.cancelled() {
            return false;
        }
        if !gate.permit.live_at(Instant::now()) {
            self.cancel_locked(&mut gate, SonioxIncompleteReason::ControlUnavailable);
            return false;
        }
        match gate.pcm.as_ref().map(|sender| sender.try_send(pcm)) {
            Some(Ok(())) => true,
            Some(Err(TrySendError::Full(_))) => {
                self.cancel_locked(&mut gate, SonioxIncompleteReason::InputOverflow);
                false
            }
            Some(Err(TrySendError::Disconnected(_))) => {
                drop(gate.pcm.take());
                false
            }
            None => false,
        }
    }

    /// Renew only the exact live permission. A delayed or mismatched reply
    /// cannot adopt a new epoch or revive upload after revocation.
    pub fn renew(&self, next: SonioxCapturePermit) -> bool {
        let mut gate = self.0.gate.lock();
        if gate.finished || self.0.cancellation.cancelled() || next.binding != gate.permit.binding {
            return false;
        }
        let now = Instant::now();
        if !gate.permit.live_at(now) {
            self.cancel_locked(&mut gate, SonioxIncompleteReason::ControlUnavailable);
            return false;
        }
        if !next.live_at(now) || next.expires_at <= gate.permit.expires_at {
            return false;
        }
        gate.permit = next;
        self.0.changed.notify_all();
        true
    }

    /// Signal revocation only for this capture and permission. This returns
    /// admission of the cancel request. The drain result is the worker ACK.
    pub fn revoke(&self, binding: &SonioxPermissionBinding) -> bool {
        let mut gate = self.0.gate.lock();
        if gate.finished || *binding != gate.permit.binding {
            return false;
        }
        self.cancel_locked(&mut gate, SonioxIncompleteReason::PermissionRevoked);
        true
    }

    pub fn control_lost(&self) {
        let mut gate = self.0.gate.lock();
        if !gate.finished {
            self.cancel_locked(&mut gate, SonioxIncompleteReason::ControlUnavailable);
        }
    }

    fn cancel_locked(&self, gate: &mut Gate, reason: SonioxIncompleteReason) {
        self.0.cancellation.cancel(reason);
        drop(gate.pcm.take());
        self.0.changed.notify_all();
    }

    pub(crate) fn close_input(&self) {
        drop(self.0.gate.lock().pcm.take());
    }

    fn disarm(&self) {
        let mut gate = self.0.gate.lock();
        drop(gate.pcm.take());
        gate.finished = true;
        self.0.changed.notify_all();
    }

    fn watch(&self, ready: Sender<()>) {
        let mut gate = self.0.gate.lock();
        let _ = ready.send(());
        while !gate.finished && !self.0.cancellation.cancelled() {
            let deadline = gate.permit.expires_at;
            if Instant::now() >= deadline {
                self.cancel_locked(&mut gate, SonioxIncompleteReason::ControlUnavailable);
                return;
            }
            let _ = self.0.changed.wait_until(&mut gate, deadline);
        }
        drop(gate.pcm.take());
    }
}

type JournalThread = JoinHandle<Result<SonioxCaptureOutcome, HandoffError>>;
type JoinWorker = Box<dyn FnOnce() + Send>;

pub struct SonioxCaptureSession {
    control: SonioxCaptureControl,
    worker: Option<JoinWorker>,
    journal: Option<JournalThread>,
    watch: Option<JoinHandle<()>>,
}

/// Returned after the network worker and the confirmed-caption consumer stop.
/// A journal error still establishes that upload stopped, while the caller
/// must retain or recover the artifact guard before publishing a terminal.
pub struct SonioxCaptureDrain {
    pub binding: SonioxPermissionBinding,
    pub outcome: Result<SonioxCaptureOutcome, SonioxSessionError>,
}

impl SonioxCaptureSession {
    /// The native recorder supplies a verified service permission and the
    /// credential selected for it. A missing grant leaves media local and
    /// does not select a replacement recognition provider.
    pub fn start(
        guard: Arc<Mutex<ArtifactGuard>>,
        permit: SonioxCapturePermit,
        config: SonioxConfig,
        observe: impl FnMut(&Caption) + Send + 'static,
    ) -> Result<Self, SonioxSessionError> {
        if config.api_key.trim().is_empty()
            || config.model != permit.binding.model
            || config.language != permit.binding.language
        {
            return Err(SonioxSessionError::InvalidPermit);
        }
        Self::start_with(guard, permit, observe, move |pcm, events, cancelled| {
            let worker = SonioxWorker::start(config, pcm, events, cancelled);
            Box::new(move || worker.join())
        })
    }

    fn start_with(
        guard: Arc<Mutex<ArtifactGuard>>,
        permit: SonioxCapturePermit,
        mut observe: impl FnMut(&Caption) + Send + 'static,
        start_worker: impl FnOnce(Receiver<Vec<f32>>, Sender<SttEvent>, Arc<AtomicBool>) -> JoinWorker,
    ) -> Result<Self, SonioxSessionError> {
        let binding = &permit.binding;
        {
            let guard = guard.lock();
            if !permit.live_at(Instant::now())
                || binding.capture_id.is_nil()
                || binding.service_epoch.is_nil()
                || binding.permission_epoch.is_nil()
                || &binding.artifact != guard.identity()
                || guard.terminal().is_some()
                || binding.model.len() > 128
                || !binding
                    .model
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !binding
                    .model
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte))
            {
                return Err(SonioxSessionError::InvalidPermit);
            }
        }
        let provenance = ProviderProvenance {
            provider: SttProvider::Soniox,
            model_id: Some(binding.model.clone()),
        };
        let cancellation = SonioxHandoffCancellation::default();
        let (pcm, pcm_rx) = bounded(PCM_QUEUE);
        let (events, event_rx) = unbounded();
        let control = SonioxCaptureControl(Arc::new(Control {
            gate: Mutex::new(Gate {
                permit,
                pcm: Some(pcm),
                finished: false,
            }),
            changed: Condvar::new(),
            cancellation: cancellation.clone(),
        }));
        let mut session = Self {
            control: control.clone(),
            worker: None,
            journal: None,
            watch: None,
        };
        let (watch_ready, wait_for_watch) = bounded(1);
        session.watch = Some(
            thread::Builder::new()
                .name("bazza-soniox-permission".into())
                .spawn(move || control.watch(watch_ready))?,
        );
        wait_for_watch
            .recv()
            .map_err(|_| io::Error::other("Soniox permission watchdog ended"))?;
        let writer_cancellation = cancellation.clone();
        session.journal = Some(
            thread::Builder::new()
                .name("bazza-soniox-journal".into())
                .spawn(move || {
                    drain_soniox_events(event_rx, &writer_cancellation, |caption| {
                        append_caption(&mut guard.lock(), &provenance, caption.clone())?;
                        observe(&caption);
                        Ok(())
                    })
                })?,
        );
        {
            let gate = session.control.0.gate.lock();
            if !gate.permit.live_at(Instant::now()) || cancellation.cancelled() {
                return Err(SonioxSessionError::InvalidPermit);
            }
        }
        session.worker = Some(start_worker(pcm_rx, events, cancellation.worker_token()));
        Ok(session)
    }

    pub fn control(&self) -> SonioxCaptureControl {
        self.control.clone()
    }

    /// Run off the UI/capture threads. A normal stop sends PCM EOF. A revoke
    /// can interrupt this drain until the worker has acknowledged and joined.
    pub fn finish(self) -> SonioxCaptureDrain {
        self.control.close_input();
        self.join()
    }

    /// The supervisor may wait for cancellation or an explicit PCM EOF from
    /// its forwarder. Never join while holding the capture artifact mutex.
    pub(crate) fn join(mut self) -> SonioxCaptureDrain {
        self.worker.take().expect("session owns worker")();
        let outcome = match self.journal.take().expect("session owns journal").join() {
            Ok(result) => result.map_err(SonioxSessionError::Journal),
            Err(_) => Err(SonioxSessionError::JournalPanicked),
        };
        self.control.disarm();
        let _ = self.watch.take().expect("session owns watchdog").join();
        let binding = self.control.0.gate.lock().permit.binding.clone();
        SonioxCaptureDrain { binding, outcome }
    }
}

impl Drop for SonioxCaptureSession {
    fn drop(&mut self) {
        if self.worker.is_some() || self.journal.is_some() || self.watch.is_some() {
            // Dropping is not an ACK. Cancel and let the journal drain confirmed
            // events under its retained guard while the caller unwinds.
            self.control.control_lost();
            self.control.disarm();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<Mutex<ArtifactGuard>>,
        SonioxPermissionBinding,
    ) {
        let root = tempfile::tempdir().unwrap();
        let artifact = CaptureIdentity {
            library_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            worker_session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        };
        let directory = root.path().join(artifact.meeting_id.to_string());
        std::fs::create_dir(&directory).unwrap();
        let guard = ArtifactGuard::acquire(&directory, &artifact).unwrap();
        let binding = SonioxPermissionBinding {
            capture_id: Uuid::new_v4(),
            artifact,
            service_epoch: Uuid::new_v4(),
            permission_epoch: Uuid::new_v4(),
            model: "synthetic-soniox".into(),
            language: None,
        };
        (root, Arc::new(Mutex::new(guard)), binding)
    }

    fn permit(binding: &SonioxPermissionBinding, duration: Duration) -> SonioxCapturePermit {
        SonioxCapturePermit {
            binding: binding.clone(),
            expires_at: Instant::now() + duration,
        }
    }

    fn caption(text: &str, start_ms: i64) -> SttEvent {
        SttEvent::Caption(Caption {
            start_ms,
            end_ms: start_ms + 100,
            text: text.into(),
            is_final: true,
        })
    }

    #[test]
    fn exact_revoke_stops_pcm_and_ack_joins_the_remaining_confirmed_captions() {
        let (_root, guard, binding) = fixture();
        let (seen, worker_ready) = bounded(1);
        let (observe, observed) = unbounded();
        let session = SonioxCaptureSession::start_with(
            guard.clone(),
            permit(&binding, Duration::from_secs(5)),
            move |caption| observe.send(caption.text.clone()).unwrap(),
            move |pcm, events, cancelled| {
                let worker = thread::spawn(move || {
                    assert_eq!(
                        pcm.recv_timeout(Duration::from_secs(2)).unwrap(),
                        vec![0.25]
                    );
                    events.send(caption("confirmed before revoke", 0)).unwrap();
                    seen.send(()).unwrap();
                    assert!(pcm.recv_timeout(Duration::from_secs(2)).is_err());
                    assert!(cancelled.load(Ordering::Acquire));
                    events
                        .send(caption("  confirmed while stopping 🛰  ", 100))
                        .unwrap();
                    events
                        .send(SttEvent::Finished(Err("synthetic provider detail".into())))
                        .unwrap();
                });
                Box::new(move || worker.join().unwrap())
            },
        )
        .unwrap();
        let control = session.control();
        let mut stale = binding.clone();
        stale.permission_epoch = Uuid::new_v4();
        assert!(!control.revoke(&stale));
        assert!(!control.renew(permit(&stale, Duration::from_secs(6))));
        assert!(control.try_send_pcm(vec![0.25]));
        worker_ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(control.revoke(&binding));
        assert!(!control.try_send_pcm(vec![0.5]));
        assert!(!control.renew(permit(&binding, Duration::from_secs(6))));
        let ack = session.finish();
        assert_eq!(ack.binding, binding);
        assert_eq!(
            ack.outcome.unwrap(),
            SonioxCaptureOutcome::Incomplete {
                reason: SonioxIncompleteReason::PermissionRevoked
            }
        );
        let guard = guard.lock();
        assert_eq!(guard.confirmed().len(), 2);
        assert_eq!(guard.confirmed()[0].text, "confirmed before revoke");
        assert_eq!(guard.confirmed()[1].text, "  confirmed while stopping 🛰  ");
        assert_eq!(guard.last_sequence(), 2);
        assert_eq!(observed.try_iter().collect::<Vec<_>>(), [
            "confirmed before revoke",
            "  confirmed while stopping 🛰  ",
        ]);
    }

    #[test]
    fn permission_expires_even_without_more_pcm_and_interrupts_the_drain() {
        let (_root, guard, binding) = fixture();
        let lease = permit(&binding, Duration::from_millis(500));
        let session =
            SonioxCaptureSession::start_with(guard, lease.clone(), |_| {}, |_, events, cancelled| {
                let worker = thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while !cancelled.load(Ordering::Acquire) {
                        assert!(
                            Instant::now() < deadline,
                            "permission watchdog did not cancel"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    events
                        .send(SttEvent::Finished(Err("worker stopped".into())))
                        .unwrap();
                });
                Box::new(move || worker.join().unwrap())
            })
            .unwrap();
        let control = session.control();
        let ack = session.finish();
        assert!(Instant::now() >= lease.expires_at);
        assert_eq!(
            ack.outcome.unwrap(),
            SonioxCaptureOutcome::Incomplete {
                reason: SonioxIncompleteReason::ControlUnavailable
            }
        );
        assert!(!control.renew(permit(&binding, Duration::from_secs(1))));
    }

    #[test]
    fn invalid_permission_starts_no_worker_and_graceful_eof_keeps_empty_provider_ack() {
        let (_root, guard, binding) = fixture();
        let mut foreign = binding.clone();
        foreign.artifact.generation = Uuid::new_v4();
        assert!(matches!(
            SonioxCaptureSession::start_with(
                guard.clone(),
                permit(&foreign, Duration::from_secs(2)),
                |_| {},
                |_, _, _| panic!("unverified permission cannot start Soniox"),
            ),
            Err(SonioxSessionError::InvalidPermit)
        ));
        let session = SonioxCaptureSession::start_with(
            guard.clone(),
            permit(&binding, Duration::from_secs(2)),
            |_| {},
            |pcm, events, cancelled| {
                let worker = thread::spawn(move || {
                    assert!(matches!(
                        pcm.recv_timeout(Duration::from_secs(2)),
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected)
                    ));
                    assert!(!cancelled.load(Ordering::Acquire));
                    events.send(SttEvent::Finished(Ok(()))).unwrap();
                });
                Box::new(move || worker.join().unwrap())
            },
        )
        .unwrap();
        assert!(session
            .control()
            .renew(permit(&binding, Duration::from_secs(3))));
        let ack = session.finish();
        assert_eq!(ack.outcome.unwrap(), SonioxCaptureOutcome::Complete {});
        assert_eq!(guard.lock().last_sequence(), 0);
    }
}

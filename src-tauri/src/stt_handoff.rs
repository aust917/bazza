//! Connect the existing Soniox event stream to a capture's durable journal.
//! The caller supplies service permission to the native session before opening
//! Soniox. The journal itself uses no network or service connection.

use bazza_asr::{provider::SttEvent, Caption};
use bazza_capture_handoff::{
    ArtifactGuard, ConfirmedCaption, HandoffError, ProviderProvenance, SttProvider,
};
use bazza_domain::capture_stt::{SonioxCaptureOutcome, SonioxIncompleteReason};
use crossbeam_channel::Receiver;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
};

#[path = "stt_handoff_session.rs"]
mod session;
pub use session::{
    SonioxCaptureControl, SonioxCaptureDrain, SonioxCapturePermit, SonioxCaptureSession,
    SonioxPermissionBinding, SonioxSessionError,
};

#[derive(Clone, Default)]
pub struct SonioxHandoffCancellation {
    token: Arc<AtomicBool>,
    reason: Arc<AtomicU8>,
}

impl SonioxHandoffCancellation {
    /// Pass this exact token to the existing SonioxWorker. Callers set typed
    /// local stop reasons through cancel so capture never stores provider errors.
    pub fn worker_token(&self) -> Arc<AtomicBool> {
        self.token.clone()
    }

    pub fn cancel(&self, reason: SonioxIncompleteReason) {
        let code = match reason {
            SonioxIncompleteReason::PermissionRevoked => 1,
            SonioxIncompleteReason::ControlUnavailable => 2,
            SonioxIncompleteReason::ProviderFailed => 3,
            SonioxIncompleteReason::InputOverflow => 4,
            SonioxIncompleteReason::FinalizationInterrupted => 5,
        };
        let _ = self
            .reason
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        self.token.store(true, Ordering::Release);
    }

    fn incomplete(&self, fallback: SonioxIncompleteReason) -> SonioxCaptureOutcome {
        SonioxCaptureOutcome::Incomplete {
            reason: match self.reason.load(Ordering::Acquire) {
                1 => SonioxIncompleteReason::PermissionRevoked,
                2 => SonioxIncompleteReason::ControlUnavailable,
                3 => SonioxIncompleteReason::ProviderFailed,
                4 => SonioxIncompleteReason::InputOverflow,
                5 => SonioxIncompleteReason::FinalizationInterrupted,
                _ => fallback,
            },
        }
    }

    fn cancelled(&self) -> bool {
        self.token.load(Ordering::Acquire)
    }
}

/// Drain the receiver paired with SonioxWorker on its caption consumer thread.
/// Confirmed text is journaled even if local cancellation already stopped audio
/// upload. The provider's final event determines the terminal STT outcome.
/// A journal failure cancels upload and leaves prior frames under the caller's
/// guard. The caller still stops the local recorder and handles that failure.
pub fn consume_soniox_capture(
    guard: &mut ArtifactGuard,
    provenance: &ProviderProvenance,
    events: Receiver<SttEvent>,
    cancellation: &SonioxHandoffCancellation,
) -> Result<SonioxCaptureOutcome, HandoffError> {
    if provenance.provider != SttProvider::Soniox || provenance.model_id.is_none() {
        cancellation.cancel(SonioxIncompleteReason::FinalizationInterrupted);
        return Err(HandoffError::Invalid);
    }
    drain_soniox_events(events, cancellation, |caption| {
        append_caption(guard, provenance, caption)
    })
}

fn append_caption(
    guard: &mut ArtifactGuard,
    provenance: &ProviderProvenance,
    caption: Caption,
) -> Result<(), HandoffError> {
    guard.append_confirmed(&ConfirmedCaption {
        sequence: guard.last_sequence() + 1,
        start_ms: u64::try_from(caption.start_ms).map_err(|_| HandoffError::Invalid)?,
        end_ms: u64::try_from(caption.end_ms).map_err(|_| HandoffError::Invalid)?,
        speaker: None,
        text: caption.text,
        provenance: provenance.clone(),
    })?;
    Ok(())
}

fn drain_soniox_events(
    events: Receiver<SttEvent>,
    cancellation: &SonioxHandoffCancellation,
    mut append: impl FnMut(Caption) -> Result<(), HandoffError>,
) -> Result<SonioxCaptureOutcome, HandoffError> {
    for event in events {
        match event {
            SttEvent::Caption(caption) if caption.is_final => {
                if let Err(error) = append(caption) {
                    cancellation.cancel(SonioxIncompleteReason::FinalizationInterrupted);
                    return Err(error);
                }
            }
            SttEvent::Finished(Ok(())) => return Ok(SonioxCaptureOutcome::Complete {}),
            SttEvent::Finished(Err(_)) => {
                return Ok(cancellation.incomplete(SonioxIncompleteReason::ProviderFailed));
            }
            SttEvent::Caption(_) | SttEvent::Reconnecting => {}
        }
    }
    Ok(cancellation.incomplete(SonioxIncompleteReason::FinalizationInterrupted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_capture_handoff::{
        CaptureIdentity, CaptureTerminalStatus, SealedCaptureGuard, TerminalManifest,
        MANIFEST_VERSION,
    };
    use uuid::Uuid;

    fn fixture() -> (tempfile::TempDir, ArtifactGuard, ProviderProvenance) {
        let root = tempfile::tempdir().unwrap();
        let identity = CaptureIdentity {
            library_id: Uuid::new_v4(),
            meeting_id: Uuid::new_v4(),
            worker_session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        };
        let directory = root.path().join(identity.meeting_id.to_string());
        std::fs::create_dir(&directory).unwrap();
        let guard = ArtifactGuard::acquire(&directory, &identity).unwrap();
        let provenance = ProviderProvenance {
            provider: SttProvider::Soniox,
            model_id: Some("fixture-soniox".into()),
        };
        (root, guard, provenance)
    }

    fn caption(text: &str, is_final: bool, start_ms: i64) -> SttEvent {
        SttEvent::Caption(Caption {
            text: text.into(),
            is_final,
            start_ms,
            end_ms: 100,
        })
    }

    fn queued<const N: usize>(events: [SttEvent; N]) -> Receiver<SttEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        for event in events {
            tx.send(event).unwrap();
        }
        rx
    }

    #[test]
    fn revocation_preserves_confirmed_text_and_seals_without_provider_error_text() {
        let (_root, mut guard, provenance) = fixture();
        let cancellation = SonioxHandoffCancellation::default();
        let worker_token = cancellation.worker_token();
        cancellation.cancel(SonioxIncompleteReason::PermissionRevoked);
        cancellation.cancel(SonioxIncompleteReason::ControlUnavailable);
        assert!(worker_token.load(Ordering::Acquire));

        let outcome = consume_soniox_capture(
            &mut guard,
            &provenance,
            queued([
                caption("display-only partial", false, 0),
                SttEvent::Reconnecting,
                caption("  Подтверждённый текст 🛰  ", true, 2),
                SttEvent::Finished(Err("private provider error sentinel".into())),
            ]),
            &cancellation,
        )
        .unwrap();
        assert_eq!(
            outcome,
            SonioxCaptureOutcome::Incomplete {
                reason: SonioxIncompleteReason::PermissionRevoked
            }
        );
        let identity = guard.identity().clone();
        let directory = guard.directory().to_path_buf();
        let terminal = TerminalManifest {
            version: MANIFEST_VERSION,
            identity: identity.clone(),
            provenance,
            duration_ms: 100,
            ended_at: chrono::Utc::now(),
            status: CaptureTerminalStatus::Failed,
            error_code: None,
            has_audio: false,
            has_video: false,
            last_confirmed_sequence: guard.last_sequence(),
        };
        guard
            .publish_terminal_with_evidence(&terminal, None, Some(outcome))
            .unwrap();
        drop(guard);
        let sealed = SealedCaptureGuard::acquire(&directory, &identity).unwrap();
        assert_eq!(sealed.soniox_outcome().copied(), Some(outcome));
        let mut captions = Vec::new();
        sealed
            .scan_confirmed::<HandoffError>(|caption| {
                captions.push(caption);
                Ok(())
            })
            .unwrap();
        assert_eq!(captions.len(), 1);
        assert_eq!(captions[0].sequence, 1);
        assert_eq!(captions[0].text, "  Подтверждённый текст 🛰  ");
    }

    #[test]
    fn empty_provider_ack_is_distinct_from_channel_close_and_partial_events() {
        let (_root, mut guard, provenance) = fixture();
        let cancellation = SonioxHandoffCancellation::default();
        assert_eq!(
            consume_soniox_capture(
                &mut guard,
                &provenance,
                queued([caption("partial", false, 0), SttEvent::Finished(Ok(()))]),
                &cancellation,
            )
            .unwrap(),
            SonioxCaptureOutcome::Complete {}
        );
        assert_eq!(
            consume_soniox_capture(&mut guard, &provenance, queued([]), &cancellation).unwrap(),
            SonioxCaptureOutcome::Incomplete {
                reason: SonioxIncompleteReason::FinalizationInterrupted
            }
        );
        assert_eq!(guard.last_sequence(), 0);
    }

    #[test]
    fn invalid_final_event_cancels_upload_without_losing_previous_journal_frames() {
        let (_root, mut guard, provenance) = fixture();
        let cancellation = SonioxHandoffCancellation::default();
        assert!(matches!(
            consume_soniox_capture(
                &mut guard,
                &provenance,
                queued([
                    caption("previous confirmed", true, 0),
                    caption("invalid", true, -1)
                ]),
                &cancellation,
            ),
            Err(HandoffError::Invalid)
        ));
        assert!(cancellation.worker_token().load(Ordering::Acquire));
        assert_eq!(guard.confirmed().len(), 1);
        assert_eq!(guard.confirmed()[0].text, "previous confirmed");
    }
}

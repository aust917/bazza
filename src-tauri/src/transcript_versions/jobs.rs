use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

use bazza_domain::transcript_versions::TranscriptVersion;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::{commands::CmdResult, ui_message::UiMessage};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Queued,
    Reading,
    Loading,
    Recognizing,
    Aligning,
    Saving,
    Cancelling,
    Cancelled,
    Succeeded,
    Failed,
}

impl Phase {
    fn terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Succeeded | Self::Failed)
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct VersionStatus {
    id: Uuid,
    phase: Phase,
    progress: Option<f32>,
    elapsed_ms: u64,
    can_cancel: bool,
    version: Option<TranscriptVersion>,
    error: Option<UiMessage>,
}

#[derive(Serialize, Clone)]
struct ProgressEvent<'a> {
    meeting_id: &'a str,
    phase: Phase,
    progress: Option<f32>,
}

pub(crate) struct VersionJob {
    id: Uuid,
    started: Instant,
    state: Mutex<VersionStatus>,
    pub(crate) cancel: Arc<AtomicBool>,
}

impl VersionJob {
    fn new() -> Self {
        let id = Uuid::new_v4();
        Self {
            id,
            started: Instant::now(),
            state: Mutex::new(VersionStatus {
                id,
                phase: Phase::Queued,
                progress: None,
                elapsed_ms: 0,
                can_cancel: true,
                version: None,
                error: None,
            }),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn id(&self) -> Uuid {
        self.id
    }

    fn snapshot(&self) -> VersionStatus {
        let mut state = self.state.lock().clone();
        if !state.phase.terminal() {
            state.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        state
    }

    pub(crate) fn checkpoint(&self) -> CmdResult<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(super::cancelled())
        } else {
            Ok(())
        }
    }

    pub(crate) fn update(
        &self,
        app: &AppHandle,
        meeting_id: &str,
        phase: Phase,
        percent: Option<u8>,
    ) {
        let progress = percent.map(|percent| f32::from(percent.min(99)) / 100.0);
        {
            let mut state = self.state.lock();
            if state.phase.terminal()
                || self.cancel.load(Ordering::Relaxed)
                || (state.phase == phase && state.progress == progress)
            {
                return;
            }
            state.phase = phase;
            state.progress = progress;
        }
        let _ = app.emit(
            "transcript_version_progress",
            ProgressEvent {
                meeting_id,
                phase,
                progress,
            },
        );
    }

    pub(crate) fn begin_save(&self, app: &AppHandle, meeting_id: &str) -> CmdResult<()> {
        {
            let mut state = self.state.lock();
            self.checkpoint()?;
            state.phase = Phase::Saving;
            state.progress = None;
            state.can_cancel = false;
        }
        let _ = app.emit(
            "transcript_version_progress",
            ProgressEvent {
                meeting_id,
                phase: Phase::Saving,
                progress: None,
            },
        );
        Ok(())
    }

    fn request_cancel(&self) -> Option<bool> {
        let mut state = self.state.lock();
        if !state.can_cancel {
            return None;
        }
        let queued = state.phase == Phase::Queued;
        self.cancel.store(true, Ordering::Relaxed);
        state.phase = Phase::Cancelling;
        state.progress = None;
        state.can_cancel = false;
        Some(queued)
    }

    fn finish(&self, result: &CmdResult<TranscriptVersion>) {
        let mut state = self.state.lock();
        if state.phase.terminal() {
            return;
        }
        state.elapsed_ms = self.started.elapsed().as_millis() as u64;
        state.can_cancel = false;
        state.progress = None;
        state.phase = match result {
            Ok(version) => {
                state.progress = Some(1.0);
                state.version = Some(version.clone());
                Phase::Succeeded
            }
            Err(_) if self.cancel.load(Ordering::Relaxed) => Phase::Cancelled,
            Err(error) => {
                state.error = Some(error.clone());
                Phase::Failed
            }
        };
    }
}

pub(crate) struct Lease(pub(crate) Arc<VersionJob>);

impl Lease {
    pub(crate) fn finish(&self, result: &CmdResult<TranscriptVersion>) {
        self.0.finish(result);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.finish(&Err(super::missing()));
    }
}

#[derive(Default)]
pub(crate) struct VersionJobs(Mutex<HashMap<String, Arc<VersionJob>>>);

impl VersionJobs {
    pub(crate) fn start(&self, meeting: &str) -> CmdResult<Lease> {
        let mut jobs = self.0.lock();
        if jobs
            .get(meeting)
            .is_some_and(|job| !job.state.lock().phase.terminal())
        {
            return Err(super::version_busy());
        }
        if jobs.len() >= 64 {
            jobs.retain(|_, job| !job.state.lock().phase.terminal());
            if jobs.len() >= 64 {
                return Err(super::version_busy());
            }
        }
        let job = Arc::new(VersionJob::new());
        jobs.insert(meeting.to_owned(), Arc::clone(&job));
        Ok(Lease(job))
    }

    pub(super) fn status(&self, meeting: &str) -> Option<VersionStatus> {
        self.0.lock().get(meeting).map(|job| job.snapshot())
    }

    pub(super) fn cancel(&self, meeting: &str, id: Uuid) -> Option<bool> {
        let jobs = self.0.lock();
        jobs.get(meeting)
            .filter(|job| job.id == id)?
            .request_cancel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_domain::transcript_versions::TranscriptVersionSource;

    #[test]
    fn cancellation_is_scoped_to_the_current_job_and_retry_can_succeed() {
        let jobs = VersionJobs::default();
        let first = jobs.start("meeting").unwrap();
        assert!(jobs.start("meeting").is_err());
        assert_eq!(jobs.cancel("other", first.0.id()), None);
        assert_eq!(jobs.cancel("meeting", first.0.id()), Some(true));
        assert!(first.0.checkpoint().is_err());
        let id = first.0.id();
        drop(first);
        assert!(jobs.status("meeting").unwrap().phase == Phase::Cancelled);

        let retry = jobs.start("meeting").unwrap();
        assert_eq!(jobs.cancel("meeting", id), None);
        let version = TranscriptVersion {
            id: "new-version".into(),
            source: TranscriptVersionSource::Recording,
            model: None,
            language: None,
            created_at: chrono::Utc::now(),
            partial: false,
        };
        retry.finish(&Ok(version.clone()));
        let status = jobs.status("meeting").unwrap();
        assert!(status.phase == Phase::Succeeded);
        assert_eq!(status.progress, Some(1.0));
        assert_eq!(status.version, Some(version));
        assert_eq!(jobs.cancel("meeting", retry.0.id()), None);
    }
}

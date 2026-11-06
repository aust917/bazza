use std::{
    collections::HashMap,
    sync::{atomic::{AtomicBool, Ordering}, Arc},
    time::Instant,
};

use parking_lot::Mutex;
use serde::Serialize;
use uuid::Uuid;

use super::{cancelled, Source};
use crate::{commands::CmdResult, ui_message::UiMessage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    Queued,
    Reading,
    Loading,
    Transcribing,
    Aligning,
    Saving,
    Cancelling,
    Cancelled,
    Done,
    Failed,
}

impl Stage {
    fn terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Done | Self::Failed)
    }
}

#[derive(Clone, Serialize)]
pub struct Status {
    id: Uuid,
    source: Source,
    stage: Stage,
    elapsed_ms: u64,
    completed: Option<u64>,
    total: Option<u64>,
    model: Option<String>,
    backend: Option<String>,
    backend_reason: Option<String>,
    can_cancel: bool,
    error: Option<UiMessage>,
}

pub(super) struct Job {
    started: Instant,
    state: Mutex<Status>,
    pub(super) cancel: Arc<AtomicBool>,
}

impl Job {
    pub(super) fn new(source: Source, cancel: Arc<AtomicBool>) -> Self {
        Self {
            started: Instant::now(),
            state: Mutex::new(Status {
                id: Uuid::new_v4(), source, stage: Stage::Queued, elapsed_ms: 0,
                completed: None, total: None, model: None, backend: None, backend_reason: None,
                can_cancel: true, error: None,
            }),
            cancel,
        }
    }

    pub(super) fn elapsed_ms(&self) -> u64 { self.started.elapsed().as_millis() as u64 }

    fn snapshot(&self) -> Status {
        let mut status = self.state.lock().clone();
        if !status.stage.terminal() {
            status.elapsed_ms = self.elapsed_ms();
        }
        status
    }

    pub(super) fn checkpoint(&self) -> CmdResult<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(cancelled()) } else { Ok(()) }
    }

    pub(super) fn stage(&self, stage: Stage, progress: Option<(u64, u64)>) {
        let mut state = self.state.lock();
        if self.cancel.load(Ordering::Relaxed) || state.stage.terminal() { return; }
        state.stage = stage;
        (state.completed, state.total) = progress.map_or((None, None), |(n, total)| {
            (Some(n.min(total)), (total > 0).then_some(total))
        });
    }

    pub(super) fn engine(&self, model: &str, backend: &str, reason: Option<&str>) {
        let mut state = self.state.lock();
        if state.model.as_deref() != Some(model) { state.model = Some(model.into()); }
        if state.backend.as_deref() != Some(backend) { state.backend = Some(backend.into()); }
        if state.backend_reason.as_deref() != reason { state.backend_reason = reason.map(str::to_owned); }
    }

    pub(super) fn begin_publish(&self) -> CmdResult<()> {
        let mut state = self.state.lock();
        self.checkpoint()?;
        state.can_cancel = false;
        Ok(())
    }

    fn finish<T>(&self, result: &CmdResult<T>) {
        let mut state = self.state.lock();
        if state.stage.terminal() { return; }
        state.elapsed_ms = self.elapsed_ms();
        state.can_cancel = false;
        (state.completed, state.total) = (None, None);
        state.stage = match result {
            Ok(_) => Stage::Done,
            Err(_) if self.cancel.load(Ordering::Relaxed) => Stage::Cancelled,
            Err(error) => { state.error = Some(error.clone()); Stage::Failed }
        };
    }

    fn request_cancel(&self, id: Uuid) -> Option<bool> {
        let mut state = self.state.lock();
        if state.id != id || !state.can_cancel { return None; }
        let was_queued = state.stage == Stage::Queued;
        self.cancel.store(true, Ordering::Relaxed);
        state.stage = Stage::Cancelling;
        state.can_cancel = false;
        (state.completed, state.total) = (None, None);
        Some(was_queued)
    }
}

/// A queued closure can be dropped or unwind. Do not leave its UI in progress.
pub(super) struct Lease(Arc<Job>);

impl Lease {
    pub(super) fn id(&self) -> Uuid { self.0.state.lock().id }
    pub(super) fn job(&self) -> &Job { &self.0 }
    pub(super) fn handle(&self) -> Arc<Job> { Arc::clone(&self.0) }
    pub(super) fn finish<T>(&self, result: &CmdResult<T>) { self.0.finish(result); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_job_cannot_publish_and_its_token_cannot_cancel_a_retry() {
        let jobs = Jobs::default();
        let first = jobs.start("meeting", Source::Discord).unwrap();
        let id = first.id();
        assert_eq!(jobs.cancel("meeting", Source::Discord, id), Some(true));
        assert!(first.job().begin_publish().is_err());
        drop(first);
        assert_eq!(jobs.list("meeting")[0].stage, Stage::Cancelled);

        let retry = jobs.start("meeting", Source::Discord).unwrap();
        assert_eq!(jobs.cancel("meeting", Source::Discord, id), None);
        assert!(retry.job().begin_publish().is_ok());
        assert_eq!(jobs.cancel("meeting", Source::Discord, retry.id()), None);
        retry.finish(&Ok(()));
        assert_eq!(jobs.list("meeting")[0].stage, Stage::Done);
    }
}

impl Drop for Lease {
    fn drop(&mut self) { self.0.finish::<()>(&Err(super::failed())); }
}

#[derive(Default)]
pub struct Jobs(Mutex<HashMap<(String, Source), Arc<Job>>>);

impl Jobs {
    pub(super) fn start(&self, meeting: &str, source: Source) -> CmdResult<Lease> {
        let mut jobs = self.0.lock();
        let key = (meeting.to_string(), source);
        if jobs.get(&key).is_some_and(|job| !job.state.lock().stage.terminal()) {
            return Err(super::busy());
        }
        // Retain recent terminal states for reopen, without accumulating all history.
        if jobs.len() >= 64 {
            jobs.retain(|_, job| !job.state.lock().stage.terminal());
        }
        let job = Arc::new(Job::new(source, Arc::new(AtomicBool::new(false))));
        jobs.insert(key, Arc::clone(&job));
        Ok(Lease(job))
    }

    pub(super) fn list(&self, meeting: &str) -> Vec<Status> {
        let jobs = self.0.lock();
        [Source::Meet, Source::Discord].into_iter()
            .filter_map(|source| jobs.get(&(meeting.to_string(), source)))
            .map(|job| job.snapshot()).collect()
    }

    pub(super) fn cancel(&self, meeting: &str, source: Source, id: Uuid) -> Option<bool> {
        self.0.lock().get(&(meeting.to_string(), source))?.request_cancel(id)
    }
}

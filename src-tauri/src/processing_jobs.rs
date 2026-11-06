//! One heavy meeting job at a time, including startup recovery and explicit reruns.
//! Queued work stays recoverable in the meeting database; duplicate IDs are rejected.

use crate::state::AppState;
use parking_lot::Mutex;
use std::collections::{HashSet, VecDeque};
use std::time::Duration;
use tauri::{AppHandle, Manager};

type Task = Box<dyn FnOnce(&AppHandle) + Send + 'static>;

struct Job {
    id: String,
    tag: Option<uuid::Uuid>,
    task: Task,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Job>,
    ids: HashSet<String>,
    worker: bool,
}

#[derive(Default)]
pub struct ProcessingJobs {
    queue: Mutex<Queue>,
}

pub struct Reservation<'a> {
    jobs: &'a ProcessingJobs,
    id: String,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) { self.jobs.finish(&self.id); }
}

impl ProcessingJobs {
    fn push(&self, id: String, task: Task) -> Result<bool, String> {
        self.push_tagged(id, None, task)
    }

    fn push_tagged(&self, id: String, tag: Option<uuid::Uuid>, task: Task) -> Result<bool, String> {
        let mut queue = self.queue.lock();
        if !queue.ids.insert(id.clone()) {
            return Err("This meeting already has a processing job queued or running.".into());
        }
        queue.pending.push_back(Job { id, tag, task });
        let start_worker = !queue.worker;
        queue.worker = true;
        Ok(start_worker)
    }

    pub(crate) fn cancel_pending(&self, tag: uuid::Uuid) -> bool {
        let mut queue = self.queue.lock();
        let Some(index) = queue.pending.iter().position(|job| job.tag == Some(tag)) else { return false; };
        let job = queue.pending.remove(index).expect("known pending job");
        queue.ids.remove(&job.id);
        drop(queue);
        drop(job);
        true
    }

    fn next(&self) -> Option<Job> {
        let mut queue = self.queue.lock();
        let job = queue.pending.pop_front();
        if job.is_none() {
            queue.worker = false;
        }
        job
    }

    fn finish(&self, id: &str) {
        self.queue.lock().ids.remove(id);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.queue.lock().ids.contains(id)
    }

    /// Reserve a meeting for a destructive operation without holding the queue lock
    /// while doing filesystem work. Enqueue and deletion cannot race on one ID.
    pub fn reserve(&self, id: &str) -> Result<Reservation<'_>, String> {
        if !self.queue.lock().ids.insert(id.to_string()) {
            return Err("Wait for this meeting's processing job to finish before deleting it.".into());
        }
        Ok(Reservation { jobs: self, id: id.to_string() })
    }

    pub fn is_idle(&self) -> bool {
        self.queue.lock().ids.is_empty()
    }
}

pub fn enqueue(app: &AppHandle, id: String, task: impl FnOnce(&AppHandle) + Send + 'static) -> Result<(), String> {
    enqueue_inner(app, id, None, task)
}

pub(crate) fn enqueue_tagged(app: &AppHandle, id: String, tag: uuid::Uuid, task: impl FnOnce(&AppHandle) + Send + 'static) -> Result<(), String> {
    enqueue_inner(app, id, Some(tag), task)
}

fn enqueue_inner(app: &AppHandle, id: String, tag: Option<uuid::Uuid>, task: impl FnOnce(&AppHandle) + Send + 'static) -> Result<(), String> {
    let state = app.state::<AppState>();
    let task = Box::new(task);
    let start_worker = if let Some(tag) = tag {
        state.processing_jobs.push_tagged(id.clone(), Some(tag), task)?
    } else {
        state.processing_jobs.push(id.clone(), task)?
    };
    log::info!("processing job queued: {id}");
    if start_worker {
        let app = app.clone();
        std::thread::Builder::new().name("bazza-processing".into()).spawn(move || run(app)).expect("spawn processing queue");
    }
    Ok(())
}

fn run(app: AppHandle) {
    loop {
        let state = app.state::<AppState>();
        // Let a recording or an explicit library copy keep its current resources.
        // Never wait for this guard on the window event thread.
        let copy_guard = state.data_copy.try_read();
        let lifecycle = crate::recorder::try_recording_lifecycle();
        if copy_guard.is_none() || lifecycle.is_none() || state.recording.lock().is_some() {
            drop(copy_guard);
            drop(lifecycle);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let Some(job) = state.processing_jobs.next() else { return };
        drop(copy_guard);
        drop(lifecycle);
        log::info!("processing job started: {}", job.id);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.task)(&app)));
        state.processing_jobs.finish(&job.id);
        if outcome.is_err() {
            log::error!("processing job panicked: {}", job.id);
        }
        log::info!("processing job finished: {}", job.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fifo_jobs_reject_duplicates_until_the_running_job_finishes() {
        let jobs = ProcessingJobs::default();
        assert!(jobs.push("first".into(), Box::new(|_| {})).unwrap());
        assert!(!jobs.push("second".into(), Box::new(|_| {})).unwrap());
        assert!(jobs.push("first".into(), Box::new(|_| {})).is_err());
        let first = jobs.next().unwrap();
        assert_eq!(first.id, "first");
        assert!(jobs.contains("first"));
        assert!(jobs.push("first".into(), Box::new(|_| {})).is_err());
        jobs.finish(&first.id);
        let second = jobs.next().unwrap();
        assert_eq!(second.id, "second");
        jobs.finish(&second.id);
        assert!(jobs.next().is_none());
        assert!(jobs.is_idle());
        assert!(jobs.push("first".into(), Box::new(|_| {})).unwrap());
    }

    #[test]
    fn concurrent_submissions_create_one_worker_and_keep_every_unique_job() {
        let jobs = Arc::new(ProcessingJobs::default());
        let handles: Vec<_> = (0..32).map(|index| {
            let jobs = jobs.clone();
            std::thread::spawn(move || jobs.push(index.to_string(), Box::new(|_| {})).unwrap())
        }).collect();
        let starts = handles.into_iter().map(|handle| usize::from(handle.join().unwrap())).sum::<usize>();
        assert_eq!(starts, 1);
        let mut seen = HashSet::new();
        while let Some(job) = jobs.next() {
            assert!(seen.insert(job.id.clone()));
            jobs.finish(&job.id);
        }
        assert_eq!(seen.len(), 32);
        assert!(jobs.is_idle());
    }

    #[test]
    fn deletion_reservation_excludes_enqueue_and_is_released_even_on_early_exit() {
        let jobs = ProcessingJobs::default();
        let reservation = jobs.reserve("meeting").unwrap();
        assert!(!jobs.is_idle());
        assert!(jobs.push("meeting".into(), Box::new(|_| {})).is_err());
        assert!(jobs.reserve("meeting").is_err());
        drop(reservation);
        assert!(jobs.is_idle());
        assert!(jobs.push("meeting".into(), Box::new(|_| {})).unwrap());
        assert!(jobs.reserve("meeting").is_err());
    }
}

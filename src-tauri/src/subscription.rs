//! In-memory lifecycle for explicit subscription requests. No library state is loaded here.

use crate::{commands, ui_message};
use bazza_summary_runner::{Agent, AskError, Output, SubscriptionAvailability, SubscriptionStatus};
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::{Manager, RunEvent, State};

const MAX_RUNNING: usize = 8;
const MAX_RETAINED: usize = 16;
const RETENTION: Duration = Duration::from_secs(30 * 60);
const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct SubscriptionJob {
    request_id: String,
    agent: Agent,
    status: JobStatus,
    reply: Option<Output>,
    error: Option<ui_message::UiMessage>,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
}

struct Job {
    snapshot: SubscriptionJob,
    cancel: Arc<AtomicBool>,
    finished: Option<Instant>,
}

#[derive(Default)]
struct Jobs(HashMap<String, Job>);

impl Jobs {
    fn start(
        &mut self,
        chat_key: &str,
        agent: Agent,
    ) -> commands::CmdResult<(SubscriptionJob, Arc<AtomicBool>)> {
        self.prune();
        if self
            .0
            .get(chat_key)
            .is_some_and(|job| job.finished.is_none())
        {
            return Err(ui_message::app(
                "native.subscriptionAlreadyRunning",
                "A subscription request is already running in this chat.",
            ));
        }
        if self.0.values().filter(|job| job.finished.is_none()).count() >= MAX_RUNNING {
            return Err(ui_message::app(
                "native.subscriptionTooManyRequests",
                "Too many subscription requests are running. Try again after one finishes.",
            ));
        }
        let snapshot = SubscriptionJob {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent,
            status: JobStatus::Running,
            reply: None,
            error: None,
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            finished_at_ms: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.0.insert(
            chat_key.to_owned(),
            Job {
                snapshot: snapshot.clone(),
                cancel: cancel.clone(),
                finished: None,
            },
        );
        Ok((snapshot, cancel))
    }

    fn get(&mut self, chat_key: &str) -> Option<SubscriptionJob> {
        self.prune();
        self.0.get(chat_key).map(|job| job.snapshot.clone())
    }

    fn cancel(&mut self, chat_key: &str, request_id: &str) -> Option<SubscriptionJob> {
        self.prune();
        let job = self.0.get_mut(chat_key)?;
        if job.snapshot.request_id == request_id && job.finished.is_none() {
            job.cancel.store(true, Ordering::Relaxed);
            job.snapshot.status = JobStatus::Cancelling;
        }
        Some(job.snapshot.clone())
    }

    fn finish(&mut self, chat_key: &str, request_id: &str, result: Result<Output, AskError>) {
        let Some(job) = self.0.get_mut(chat_key) else {
            return;
        };
        if job.snapshot.request_id != request_id || job.finished.is_some() {
            return;
        }
        let result = if job.cancel.load(Ordering::Relaxed) {
            Err(AskError::Cancelled)
        } else {
            result
        };
        job.snapshot.status = match result {
            Ok(reply) => {
                job.snapshot.reply = Some(reply);
                JobStatus::Succeeded
            }
            Err(AskError::Cancelled) => JobStatus::Cancelled,
            Err(error) => {
                job.snapshot.error = Some(subscription_error(error));
                JobStatus::Failed
            }
        };
        job.finished = Some(Instant::now());
        job.snapshot.finished_at_ms = Some(chrono::Utc::now().timestamp_millis());
        self.prune();
    }

    fn prune(&mut self) {
        self.0
            .retain(|_, job| !job.finished.is_some_and(|time| time.elapsed() >= RETENTION));
        if self.0.len() <= MAX_RETAINED {
            return;
        }
        let mut finished: Vec<_> = self
            .0
            .iter()
            .filter_map(|(key, job)| job.finished.map(|time| (time, key.clone())))
            .collect();
        let excess = finished.len().saturating_sub(MAX_RETAINED);
        if excess > 0 {
            finished.sort_unstable();
            for (_, key) in finished.into_iter().take(excess) {
                self.0.remove(&key);
            }
        }
    }

    fn cancel_all(&self) {
        for job in self.0.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
struct JobState {
    jobs: Mutex<Jobs>,
    completed: Condvar,
}

#[derive(Default)]
pub struct SubscriptionJobs(Arc<JobState>);

impl SubscriptionJobs {
    fn shutdown(&self) {
        let mut jobs = self.0.jobs.lock();
        jobs.cancel_all();
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while jobs.0.values().any(|job| job.finished.is_none()) {
            if self.0.completed.wait_until(&mut jobs, deadline).timed_out() {
                break;
            }
        }
    }
}

impl Drop for SubscriptionJobs {
    fn drop(&mut self) {
        self.0.jobs.lock().cancel_all();
    }
}

pub(crate) fn on_app_event(app: &tauri::AppHandle, event: RunEvent) {
    if matches!(event, RunEvent::Exit) {
        app.state::<SubscriptionJobs>().shutdown();
    }
}

fn check_key(chat_key: &str) -> commands::CmdResult<()> {
    if chat_key.trim().is_empty() || chat_key.len() > 128 {
        return Err(ui_message::app(
            "native.subscriptionJobKey",
            "Invalid subscription chat key.",
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn subscription_agents() -> commands::CmdResult<Vec<SubscriptionAvailability>> {
    commands::blocking(|| Ok(bazza_summary_runner::subscription_availability())).await
}

#[tauri::command]
pub fn get_subscription_job(
    chat_key: String,
    jobs: State<'_, SubscriptionJobs>,
) -> commands::CmdResult<Option<SubscriptionJob>> {
    check_key(&chat_key)?;
    Ok(jobs.inner().0.jobs.lock().get(&chat_key))
}

#[tauri::command]
pub fn start_subscription(
    chat_key: String,
    agent: Agent,
    question: String,
    context: Option<String>,
    jobs: State<'_, SubscriptionJobs>,
) -> commands::CmdResult<SubscriptionJob> {
    check_key(&chat_key)?;
    let (snapshot, cancel) = jobs.inner().0.jobs.lock().start(&chat_key, agent)?;
    let request_id = snapshot.request_id.clone();
    let jobs = jobs.inner().0.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || {
            bazza_summary_runner::ask(agent, &question, context.as_deref(), cancel)
        })
        .await
        .unwrap_or(Err(AskError::RequestFailed));
        jobs.jobs.lock().finish(&chat_key, &request_id, result);
        jobs.completed.notify_all();
    });
    Ok(snapshot)
}

#[tauri::command]
pub fn cancel_subscription(
    chat_key: String,
    request_id: String,
    jobs: State<'_, SubscriptionJobs>,
) -> commands::CmdResult<Option<SubscriptionJob>> {
    check_key(&chat_key)?;
    Ok(jobs.inner().0.jobs.lock().cancel(&chat_key, &request_id))
}

fn subscription_error(error: AskError) -> ui_message::UiMessage {
    use SubscriptionStatus::{CheckFailed, Missing, Ready, SignInRequired};

    match error {
        AskError::InvalidInput => ui_message::app(
            "native.subscriptionInput",
            "Enter a question and keep the question and selected context under 2 MiB.",
        ),
        AskError::Unavailable(Missing) => ui_message::app(
            "native.subscriptionMissing",
            "The selected CLI was not found. Install its native executable and check PATH.",
        ),
        AskError::Unavailable(SignInRequired) => ui_message::app(
            "native.subscriptionSignIn",
            "Sign in with a subscription account in the selected CLI.",
        ),
        AskError::Unavailable(CheckFailed | Ready) => ui_message::app(
            "native.subscriptionCheckFailed",
            "Could not verify CLI sign-in. Check its version and try again.",
        ),
        AskError::RequestFailed => ui_message::app(
            "native.subscriptionRequestFailed",
            "The selected CLI could not return a reply. Check its version and subscription limits.",
        ),
        AskError::Cancelled => unreachable!("cancellation is a job state"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Output {
        Output {
            markdown: "synthetic answer".into(),
            model: "synthetic model".into(),
        }
    }

    #[test]
    fn a_request_is_exclusive_and_cancellation_is_scoped_to_its_id() {
        let mut jobs = Jobs::default();
        let (first, cancel_first) = jobs.start("chat", Agent::Codex).unwrap();
        assert!(jobs.start("chat", Agent::Claude).is_err());
        assert!(!cancel_first.load(Ordering::Relaxed));
        assert!(jobs.cancel("different-chat", &first.request_id).is_none());
        assert_eq!(
            jobs.cancel("chat", "old-id").unwrap().status,
            JobStatus::Running
        );
        assert_eq!(
            jobs.cancel("chat", &first.request_id).unwrap().status,
            JobStatus::Cancelling
        );
        assert!(jobs.start("chat", Agent::Claude).is_err());
        assert!(cancel_first.load(Ordering::Relaxed));
        jobs.finish("chat", &first.request_id, Ok(reply()));
        let cancelled = jobs.get("chat").unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(cancelled.reply.is_none());
        assert!(cancelled.error.is_none());

        let (second, cancel_second) = jobs.start("chat", Agent::Claude).unwrap();
        assert_ne!(first.request_id, second.request_id);
        assert_eq!(
            jobs.cancel("chat", &first.request_id).unwrap().status,
            JobStatus::Running
        );
        assert!(!cancel_second.load(Ordering::Relaxed));
        jobs.finish("chat", &first.request_id, Err(AskError::RequestFailed));
        jobs.finish("chat", &second.request_id, Ok(reply()));
        let succeeded = jobs.cancel("chat", &second.request_id).unwrap();
        assert_eq!(succeeded.status, JobStatus::Succeeded);
        assert_eq!(succeeded.reply.unwrap().markdown, "synthetic answer");
    }

    #[test]
    fn terminal_jobs_remain_readable_and_retention_is_bounded() {
        let mut jobs = Jobs::default();
        for index in 0..=MAX_RETAINED {
            let key = format!("chat-{index}");
            let (job, _) = jobs.start(&key, Agent::Codex).unwrap();
            jobs.finish(&key, &job.request_id, Err(AskError::RequestFailed));
            let failed = jobs.get(&key).unwrap();
            let json = serde_json::to_value(failed).unwrap();
            assert_eq!(json["status"], "failed");
            assert_eq!(json["error"]["code"], "native.subscriptionRequestFailed");
            assert!(json["reply"].is_null());
            assert!(json["error"]["detail"].is_null());
        }
        assert_eq!(jobs.0.len(), MAX_RETAINED);
        for index in 0..MAX_RUNNING {
            jobs.start(&format!("active-{index}"), Agent::Claude)
                .unwrap();
        }
        assert_eq!(
            jobs.start("one-more", Agent::Claude).unwrap_err().code,
            "native.subscriptionTooManyRequests"
        );
    }

    #[test]
    fn shutdown_signals_cancellation_and_allows_the_worker_to_finish() {
        let jobs = SubscriptionJobs::default();
        let (job, cancel) = jobs.0.jobs.lock().start("chat", Agent::Codex).unwrap();
        std::thread::scope(|scope| {
            let state = jobs.0.clone();
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    std::thread::yield_now();
                }
                state
                    .jobs
                    .lock()
                    .finish("chat", &job.request_id, Err(AskError::Cancelled));
                state.completed.notify_all();
            });
            jobs.shutdown();
        });
        assert_eq!(
            jobs.0.jobs.lock().get("chat").unwrap().status,
            JobStatus::Cancelled
        );
    }
}

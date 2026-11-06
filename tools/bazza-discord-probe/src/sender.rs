use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, BufWriter, IsTerminal, Read, Write},
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use bazza_discord::{DiscordSender, DiscordSenderConfig};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::sleep_until,
};

use crate::scenario::{Action, Membership, ParticipantPlan, Plan};

const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    participants: Vec<Credential>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    participant_id: String,
    bot_id: String,
    token: String,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    guild_id: String,
    channel_id: String,
    duration_ms: u64,
    scenario_started_unix_us: Option<u64>,
    status: &'static str,
    participants: Vec<ParticipantOutcome>,
    events: Vec<ObservedEvent>,
    utterances: Vec<ObservedUtterance>,
}

#[derive(Serialize)]
struct ParticipantOutcome {
    participant_id: String,
    bot_id: String,
    outcome: &'static str,
    disconnect_confirmed: Option<bool>,
}

#[derive(Serialize)]
struct ObservedEvent {
    participant_id: String,
    action: Membership,
    planned_at_ms: u64,
    dispatched_at_ms: u64,
    confirmed_at_ms: Option<u64>,
}

#[derive(Serialize)]
struct ObservedUtterance {
    id: String,
    participant_id: String,
    planned_at_ms: u64,
    dispatched_at_ms: u64,
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    played_for_ms: Option<u64>,
}

struct ParticipantReport {
    outcome: ParticipantOutcome,
    events: Vec<ObservedEvent>,
    utterances: Vec<ObservedUtterance>,
}

fn read_credentials(reader: impl Read) -> Result<Credentials> {
    let mut data = Vec::new();
    let parsed = reader
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| anyhow::anyhow!("could not read private credential JSON"))
        .and_then(|_| {
            ensure!(
                data.len() as u64 <= MAX_CREDENTIAL_BYTES,
                "private credential JSON exceeds 64 KiB"
            );
            serde_json::from_slice(&data)
                .map_err(|_| anyhow::anyhow!("invalid private credential JSON"))
        });
    data.fill(0);
    parsed
}

fn load_credentials(path: &Path, plans: &[ParticipantPlan]) -> Result<HashMap<String, Credential>> {
    let Credentials { participants } = if path == Path::new("-") {
        let input = io::stdin();
        ensure!(
            !input.is_terminal(),
            "pipe private credential JSON into stdin"
        );
        read_credentials(input.lock())?
    } else {
        let metadata = fs::symlink_metadata(path).context("open private credential file")?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_CREDENTIAL_BYTES,
            "invalid private credential file"
        );
        read_credentials(fs::File::open(path).context("open private credential file")?)?
    };
    let expected: HashSet<_> = plans.iter().map(|plan| plan.id.as_str()).collect();
    ensure!(
        participants.len() == expected.len(),
        "one credential is required per scenario participant"
    );

    let mut result = HashMap::with_capacity(participants.len());
    let mut ids = HashSet::new();
    let mut tokens = HashSet::new();
    for credential in &participants {
        let parts: Vec<_> = credential.token.split('.').collect();
        ensure!(
            credential.token.len() <= 512
                && parts.len() == 3
                && parts.iter().all(|part| !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')),
            "invalid bot token format"
        );
        let bot_id = crate::snowflake(credential.bot_id.as_ref(), "bot")?;
        ensure!(
            bot_id.to_string() == credential.bot_id
                && ids.insert(bot_id)
                && tokens.insert(&credential.token),
            "each sender needs a distinct Discord bot ID and token"
        );
        ensure!(
            expected.contains(credential.participant_id.as_str()),
            "credentials refer to an unknown participant"
        );
    }
    drop(tokens);
    for credential in participants {
        ensure!(
            result
                .insert(credential.participant_id.clone(), credential)
                .is_none(),
            "duplicate participant credentials"
        );
    }
    Ok(result)
}

pub(crate) async fn send(
    guild: u64,
    channel: u64,
    plan: Plan,
    credentials_path: &Path,
    report_path: &Path,
) -> Result<()> {
    let mut credentials = load_credentials(credentials_path, &plan.participants)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report_path)
        .context("reserve new sender report")?;
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let interrupt = cancel_tx.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            interrupt.send_replace(true);
        }
    });
    let (origin_tx, origin_rx) = watch::channel(None);
    let participant_count = plan.participants.len();
    let (ready_tx, mut ready_rx) = mpsc::channel(participant_count);
    let mut workers = JoinSet::new();
    for participant in plan.participants {
        let credential = credentials
            .remove(&participant.id)
            .expect("validated credentials");
        let config = DiscordSenderConfig {
            token: credential.token,
            guild_id: guild.to_string(),
            channel_id: channel.to_string(),
        };
        workers.spawn(participant_worker(
            participant,
            credential.bot_id,
            config,
            plan.duration_ms,
            ready_tx.clone(),
            origin_rx.clone(),
            cancel_tx.clone(),
        ));
    }
    drop(ready_tx);

    let mut ready_count = 0;
    while let Some(ready) = ready_rx.recv().await {
        if ready {
            ready_count += 1;
        }
    }
    let all_ready = ready_count == participant_count;
    let mut report = Report {
        schema_version: 1,
        guild_id: guild.to_string(),
        channel_id: channel.to_string(),
        duration_ms: plan.duration_ms,
        scenario_started_unix_us: None,
        status: "failed",
        participants: Vec::new(),
        events: Vec::new(),
        utterances: Vec::new(),
    };
    if all_ready && !*cancel_rx.borrow() {
        let origin = Instant::now();
        report.scenario_started_unix_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_micros()).ok());
        origin_tx.send_replace(Some(origin));
        println!("Discord sender scenario started. Ctrl+C stops the senders.");
    } else {
        cancel_tx.send_replace(true);
    }

    let mut workers_ok = all_ready;
    while let Some(result) = workers.join_next().await {
        match result {
            Ok(result) => {
                workers_ok &= result.outcome.outcome == "completed"
                    && result.outcome.disconnect_confirmed == Some(true);
                report.participants.push(result.outcome);
                report.events.extend(result.events);
                report.utterances.extend(result.utterances);
            }
            Err(_) => {
                workers_ok = false;
                cancel_tx.send_replace(true);
            }
        }
    }
    signal.abort();
    report
        .participants
        .sort_by(|a, b| a.participant_id.cmp(&b.participant_id));
    report.events.sort_by(|a, b| {
        (a.planned_at_ms, &a.participant_id).cmp(&(b.planned_at_ms, &b.participant_id))
    });
    report
        .utterances
        .sort_by(|a, b| (a.planned_at_ms, &a.id).cmp(&(b.planned_at_ms, &b.id)));
    report.status = if workers_ok {
        "completed"
    } else if all_ready
        && report.participants.len() == participant_count
        && report.participants.iter().all(|participant| {
            matches!(participant.outcome, "completed" | "cancelled")
                && participant.disconnect_confirmed != Some(false)
        })
    {
        "cancelled"
    } else {
        "failed"
    };

    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &report).context("write sender report")?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    println!(
        "Saved sender report to {} ({})",
        report_path.display(),
        report.status
    );
    ensure!(
        workers_ok,
        "sender scenario did not complete; see its report"
    );
    Ok(())
}

async fn participant_worker(
    plan: ParticipantPlan,
    bot_id: String,
    config: DiscordSenderConfig,
    duration_ms: u64,
    ready: mpsc::Sender<bool>,
    mut origin: watch::Receiver<Option<Instant>>,
    cancel: watch::Sender<bool>,
) -> ParticipantReport {
    let mut report = ParticipantReport {
        outcome: ParticipantOutcome {
            participant_id: plan.id,
            bot_id,
            outcome: "connect_failed",
            disconnect_confirmed: None,
        },
        events: Vec::new(),
        utterances: Vec::new(),
    };
    let Ok(sender) = DiscordSender::connect(config).await else {
        let _ = ready.send(false).await;
        return report;
    };
    let matches_identity = sender.user_id() == report.outcome.bot_id;
    let _ = ready.send(matches_identity).await;
    drop(ready);
    if matches_identity {
        let mut stopped = cancel.subscribe();
        let result = tokio::select! {
            biased;
            _ = stopped.wait_for(|value| *value) => None,
            result = origin.wait_for(Option::is_some) => result.ok().and_then(|value| *value),
        };
        report.outcome.outcome = match result {
            None => "cancelled",
            Some(origin) => tokio::select! {
                biased;
                _ = stopped.wait_for(|value| *value) => "cancelled",
                result = run_steps(&sender, plan.steps, origin, duration_ms, &mut report) => match result {
                    Ok(()) => "completed",
                    Err(stage) => {
                        cancel.send_replace(true);
                        stage
                    }
                },
            },
        };
    } else {
        report.outcome.outcome = "identity_mismatch";
    }
    report.outcome.disconnect_confirmed = Some(sender.close().await.is_ok());
    if report.outcome.disconnect_confirmed == Some(false) {
        cancel.send_replace(true);
    }
    report
}

async fn run_steps(
    sender: &DiscordSender,
    steps: Vec<crate::scenario::Step>,
    origin: Instant,
    duration_ms: u64,
    report: &mut ParticipantReport,
) -> std::result::Result<(), &'static str> {
    let now = || relative_ms(origin, Instant::now());
    for step in steps {
        sleep_until((origin + Duration::from_millis(step.at_ms)).into()).await;
        match step.action {
            Action::Membership(action) => {
                report.events.push(ObservedEvent {
                    participant_id: report.outcome.participant_id.clone(),
                    action,
                    planned_at_ms: step.at_ms,
                    dispatched_at_ms: now(),
                    confirmed_at_ms: None,
                });
                match action {
                    Membership::Join => sender.join().await.map_err(|_| "join_failed")?,
                    Membership::Leave => sender.leave().await.map_err(|_| "leave_failed")?,
                }
                report
                    .events
                    .last_mut()
                    .expect("dispatched event")
                    .confirmed_at_ms = Some(now());
            }
            Action::Play { id, wav, .. } => {
                report.utterances.push(ObservedUtterance {
                    id,
                    participant_id: report.outcome.participant_id.clone(),
                    planned_at_ms: step.at_ms,
                    dispatched_at_ms: now(),
                    started_at_ms: None,
                    ended_at_ms: None,
                    played_for_ms: None,
                });
                let playback = sender.play_wav(&wav).await.map_err(|_| "playback_failed")?;
                let observed = report.utterances.last_mut().expect("dispatched utterance");
                observed.started_at_ms = Some(relative_ms(origin, playback.started_at));
                observed.ended_at_ms = Some(relative_ms(origin, playback.ended_at));
                observed.played_for_ms = Some(playback.played_for.as_millis() as u64);
            }
        }
    }
    sleep_until((origin + Duration::from_millis(duration_ms)).into()).await;
    Ok(())
}

fn relative_ms(origin: Instant, timestamp: Instant) -> u64 {
    timestamp.saturating_duration_since(origin).as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_reader_enforces_size_and_hides_parse_details() {
        let mut data = br#"{"participants":[]}"#.to_vec();
        data.resize(MAX_CREDENTIAL_BYTES as usize, b' ');
        assert!(read_credentials(data.as_slice()).is_ok());
        data.push(b' ');
        let oversized = read_credentials(data.as_slice()).err().unwrap();
        assert_eq!(
            oversized.to_string(),
            "private credential JSON exceeds 64 KiB"
        );

        let malformed = read_credentials(&br#"{"participants":"fake-secret"}"#[..])
            .err()
            .unwrap();
        assert_eq!(malformed.to_string(), "invalid private credential JSON");
    }
}

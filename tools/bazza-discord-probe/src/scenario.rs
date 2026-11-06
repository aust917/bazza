use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use bazza_discord::validate_sender_wav;
use serde::{Deserialize, Serialize};

const MAX_SCENARIO_BYTES: u64 = 1024 * 1024;
const MAX_DURATION_MS: u64 = 3_600_000;

#[derive(Deserialize)]
struct Scenario {
    schema_version: u32,
    duration_ms: u64,
    participants: Vec<Participant>,
    events: Vec<Event>,
    utterances: Vec<Utterance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Participant {
    id: String,
    name: String,
    voice: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Membership {
    Join,
    Leave,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    at_ms: u64,
    participant_id: String,
    action: Membership,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Utterance {
    id: String,
    participant_id: String,
    at_ms: u64,
    duration_ms: u64,
    speech_start_ms: u64,
    speech_end_ms: u64,
    wav: PathBuf,
    text: String,
}

pub(crate) struct Plan {
    pub duration_ms: u64,
    pub participants: Vec<ParticipantPlan>,
    pub utterance_count: usize,
}

pub(crate) struct ParticipantPlan {
    pub id: String,
    pub steps: Vec<Step>,
}

pub(crate) struct Step {
    pub at_ms: u64,
    pub action: Action,
}

pub(crate) enum Action {
    Membership(Membership),
    Play {
        id: String,
        wav: PathBuf,
        duration_ms: u64,
    },
}

fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

fn validate_wav(directory: &Path, relative: &Path, duration_ms: u64) -> Result<PathBuf> {
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "WAV path must be relative to the scenario directory"
    );
    let path = directory
        .join(relative)
        .canonicalize()
        .context("open scenario WAV")?;
    ensure!(
        path.starts_with(directory),
        "WAV path leaves the scenario directory"
    );
    let actual = validate_sender_wav(&path)?;
    ensure!(
        actual.abs_diff(Duration::from_millis(duration_ms)) < Duration::from_millis(1),
        "WAV length differs from duration_ms"
    );
    Ok(path)
}

pub(crate) fn load(path: &Path) -> Result<Plan> {
    let path = path.canonicalize().context("open scenario file")?;
    let metadata = fs::metadata(&path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_SCENARIO_BYTES,
        "invalid scenario file"
    );
    let Scenario {
        schema_version,
        duration_ms,
        participants,
        events,
        utterances,
    } = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|_| anyhow::anyhow!("invalid scenario JSON"))?;
    ensure!(schema_version == 1, "unsupported scenario schema");
    ensure!(
        (1..=MAX_DURATION_MS).contains(&duration_ms),
        "invalid scenario duration"
    );
    ensure!(
        matches!(participants.len(), 2 | 8 | 16),
        "scenario requires 2, 8 or 16 participants"
    );

    let mut by_id = HashMap::new();
    let mut plans = Vec::with_capacity(participants.len());
    for participant in participants {
        ensure!(
            identifier(&participant.id)
                && !participant.name.trim().is_empty()
                && !participant.voice.trim().is_empty(),
            "participant needs a valid ID, name and voice"
        );
        ensure!(
            by_id.insert(participant.id.clone(), plans.len()).is_none(),
            "duplicate participant ID"
        );
        plans.push(ParticipantPlan {
            id: participant.id,
            steps: Vec::new(),
        });
    }
    for event in events {
        let index = by_id
            .get(&event.participant_id)
            .context("event refers to an unknown participant")?;
        ensure!(
            event.at_ms <= duration_ms,
            "event lies outside the scenario"
        );
        plans[*index].steps.push(Step {
            at_ms: event.at_ms,
            action: Action::Membership(event.action),
        });
    }

    let utterance_count = utterances.len();
    let mut ids = HashSet::new();
    let directory = path.parent().context("missing scenario directory")?;
    for utterance in utterances {
        let index = by_id
            .get(&utterance.participant_id)
            .context("utterance refers to an unknown participant")?;
        ensure!(
            identifier(&utterance.id) && ids.insert(utterance.id.clone()),
            "invalid or duplicate utterance ID"
        );
        ensure!(
            utterance.speech_start_ms < utterance.speech_end_ms
                && utterance.speech_end_ms <= utterance.duration_ms
                && utterance
                    .at_ms
                    .checked_add(utterance.duration_ms)
                    .is_some_and(|end| end <= duration_ms)
                && !utterance.text.trim().is_empty(),
            "invalid timing or empty text for utterance {}",
            utterance.id
        );
        let wav = validate_wav(directory, &utterance.wav, utterance.duration_ms)
            .with_context(|| format!("validate utterance {}", utterance.id))?;
        plans[*index].steps.push(Step {
            at_ms: utterance.at_ms,
            action: Action::Play {
                id: utterance.id,
                wav,
                duration_ms: utterance.duration_ms,
            },
        });
    }

    for plan in &mut plans {
        plan.steps
            .sort_by_key(|step| (step.at_ms, matches!(&step.action, Action::Play { .. })));
        validate_steps(plan).with_context(|| format!("participant {}", plan.id))?;
    }
    Ok(Plan {
        duration_ms,
        participants: plans,
        utterance_count,
    })
}

fn validate_steps(plan: &ParticipantPlan) -> Result<()> {
    let mut joined = false;
    let mut busy_until = 0;
    let mut spoken = false;
    let mut last_at = None;
    let mut last_join = false;
    for step in &plan.steps {
        ensure!(
            last_at != Some(step.at_ms) || last_join && matches!(&step.action, Action::Play { .. }),
            "two steps have the same scheduled time"
        );
        ensure!(
            step.at_ms >= busy_until,
            "an action overlaps this participant's playback"
        );
        match &step.action {
            Action::Membership(Membership::Join) if !joined => joined = true,
            Action::Membership(Membership::Leave) if joined => joined = false,
            Action::Play { duration_ms, .. } if joined => {
                spoken = true;
                busy_until = step.at_ms + *duration_ms;
            }
            Action::Play { .. } => bail!("playback scheduled while the participant is away"),
            Action::Membership(_) => bail!("join and leave actions must alternate"),
        }
        last_at = Some(step.at_ms);
        last_join = matches!(&step.action, Action::Membership(Membership::Join));
    }
    ensure!(spoken, "participant has no scheduled utterance");
    Ok(())
}

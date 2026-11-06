use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::{
    attendee_recording::{AttendeeRecording, AttendeeRecordingSelection},
    discord_recording::{DiscordRecording, DiscordRecordingSelection},
    ui_message::UiMessage,
};

const EVENT: &str = "recording_bot_connection_status";
const CONNECTION_TTL: Duration = Duration::from_secs(300);
const EXIT_GRACE: Duration = Duration::from_secs(20);

/// This recording-local policy also fences post-call processing after restart.
/// Commit the whole marker before any device opens; a partial write must never
/// make a selected bot recording look like an ordinary mixed recording.
pub(crate) fn persist_live_policy(directory: &Path, provider: &str, capture_system: bool) -> Result<(), String> {
    if !matches!(provider, "meet" | "discord") {
        return Err("Invalid bot live-caption provider".into());
    }
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(temporary.as_file_mut(), &serde_json::json!({
            "schema": 1,
            "source": "participant_only",
            "provider": provider,
            "capture_system": capture_system,
        }))?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(directory.join("bot-live-policy.json"))?;
        Ok(())
    };
    write().map_err(|error| format!("Could not save the bot recording source policy: {error}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionPhase {
    Connecting,
    WaitingAdmission,
    Connected,
    Failed,
    Disconnected,
}

#[derive(Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub(crate) enum RecordingBotTarget {
    Meet(AttendeeRecordingSelection),
    Discord(DiscordRecordingSelection),
}

impl RecordingBotTarget {
    pub(crate) fn validate(&mut self) -> Result<(), UiMessage> {
        if let Self::Meet(selection) = self {
            selection.meeting_url = bazza_attendee::normalize_meeting_url(&selection.meeting_url)
                .map_err(|_| {
                UiMessage::app(
                    "native.attendeeInvalidMeetingUrl",
                    "Enter a Google Meet link with a meeting code.",
                )
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordingBotStatus {
    request_id: Uuid,
    revision: u32,
    phase: ConnectionPhase,
    message: Option<UiMessage>,
}

enum Bot {
    Meet(AttendeeRecording),
    Discord(DiscordRecording),
}

impl Bot {
    fn ready_to_adopt(&self) -> bool {
        match self {
            Self::Meet(bot) => bot.ready_to_adopt(),
            Self::Discord(bot) => bot.ready_to_adopt(),
        }
    }

    fn finish(self) {
        match self {
            Self::Meet(recording) => recording.finish(),
            Self::Discord(recording) => recording.finish(),
        }
    }

    fn app_name(&self) -> &'static str {
        match self {
            Self::Meet(_) => "meet",
            Self::Discord(_) => "discord",
        }
    }
}

struct PendingBot {
    generation: Uuid,
    expires_at: Instant,
    status: RecordingBotStatus,
    bot: Option<Bot>,
}

impl PendingBot {
    fn set_phase(&mut self, phase: ConnectionPhase) -> bool {
        if phase == self.status.phase {
            return false;
        }
        self.status.phase = phase;
        self.status.revision += 1;
        self.status.message = (phase == ConnectionPhase::Failed).then(|| {
            UiMessage::app(
                "setup.botConnectionLost",
                "The bot connection is unavailable.",
            )
        });
        true
    }
}

#[derive(Clone, Default)]
pub(crate) struct BotConnections(Arc<Mutex<ConnectionState>>);

#[derive(Default)]
struct ConnectionState {
    pending: Option<PendingBot>,
    claimed: bool,
    closing: bool,
    work: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl ConnectionState {
    fn queue(&mut self, operation: impl FnOnce() + Send + 'static) {
        let previous = self.work.take();
        self.work = Some(tauri::async_runtime::spawn_blocking(move || {
            if let Some(previous) = previous {
                let _ = tauri::async_runtime::block_on(previous);
            }
            operation();
        }));
    }

    fn disconnect(&mut self, mut pending: PendingBot) -> RecordingBotStatus {
        pending.set_phase(ConnectionPhase::Disconnected);
        if let Some(bot) = pending.bot {
            self.queue(move || bot.finish());
        }
        pending.status
    }
}

impl BotConnections {
    pub(crate) fn connect(
        &self,
        app: &AppHandle,
        request_id: Uuid,
        target: RecordingBotTarget,
    ) -> Result<RecordingBotStatus, UiMessage> {
        let status = RecordingBotStatus {
            request_id,
            revision: 0,
            phase: ConnectionPhase::Connecting,
            message: None,
        };
        let generation = Uuid::new_v4();
        let expires_at = Instant::now() + CONNECTION_TTL;
        let mut state = self.0.lock();
        if state.claimed || state.closing {
            return Err(UiMessage::app(
                "native.captureUnavailable",
                "Recording controls are unavailable.",
            ));
        }
        if let Some(pending) = state
            .pending
            .as_ref()
            .filter(|p| p.status.request_id == request_id)
        {
            return Ok(pending.status.clone());
        }
        let previous = state.pending.replace(PendingBot {
            generation,
            expires_at,
            status: status.clone(),
            bot: None,
        });
        let disconnected = previous.map(|pending| state.disconnect(pending));
        let setup = self.clone();
        let setup_app = app.clone();
        // In particular, Discord must leave the old channel before reusing its token.
        state.queue(move || {
            let app = setup_app;
            if !setup
                .0
                .lock()
                .pending
                .as_ref()
                .is_some_and(|p| p.generation == generation)
            {
                return;
            }
            let status_app = app.clone();
            let status_state = Arc::downgrade(&setup.0);
            let on_status = move |phase| {
                let Some(state) = status_state.upgrade() else {
                    return;
                };
                let update = {
                    let mut state = state.lock();
                    let Some(pending) = state
                        .pending
                        .as_mut()
                        .filter(|p| p.generation == generation)
                    else {
                        return;
                    };
                    if !pending.set_phase(phase) || pending.bot.is_none() {
                        return;
                    }
                    pending.status.clone()
                };
                let _ = status_app.emit(EVENT, update);
            };
            let bot = match target {
                RecordingBotTarget::Meet(selection) => {
                    Bot::Meet(AttendeeRecording::prepare(&app, selection, on_status))
                }
                RecordingBotTarget::Discord(selection) => {
                    Bot::Discord(DiscordRecording::prepare(&app, selection, on_status))
                }
            };
            let update = {
                let mut state = setup.0.lock();
                let Some(pending) = state
                    .pending
                    .as_mut()
                    .filter(|p| p.generation == generation)
                else {
                    drop(state);
                    bot.finish();
                    return;
                };
                pending.bot = Some(bot);
                pending.status.clone()
            };
            let _ = app.emit(EVENT, update);
        });
        drop(state);
        if let Some(disconnected) = disconnected {
            let _ = app.emit(EVENT, disconnected);
        }
        let expiry_app = app.clone();
        let expiry = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep_until(expires_at.into()).await;
            expiry.cancel_if(&expiry_app, |pending| pending.generation == generation);
        });
        Ok(status)
    }

    pub(crate) fn cancel(&self, app: &AppHandle, request_id: Uuid) {
        self.cancel_if(app, |p| p.status.request_id == request_id);
    }

    pub(crate) fn cancel_all(&self, app: &AppHandle) {
        self.cancel_if(app, |_| true);
    }

    fn cancel_if(&self, app: &AppHandle, predicate: impl FnOnce(&PendingBot) -> bool) {
        let status = {
            let mut state = self.0.lock();
            if !state.pending.as_ref().is_some_and(predicate) {
                return;
            }
            let pending = state.pending.take().expect("checked pending bot");
            state.disconnect(pending)
        };
        let _ = app.emit(EVENT, status);
    }

    pub(crate) fn claim(
        &self,
        app: &AppHandle,
        request_id: Uuid,
        app_name: Option<&str>,
    ) -> Option<PreparedBot> {
        let mut state = self.0.lock();
        let pending = state.pending.as_ref().filter(|pending| {
            pending.status.request_id == request_id
                && pending.status.phase == ConnectionPhase::Connected
                && pending.expires_at > Instant::now()
                && pending
                    .bot
                    .as_ref()
                    .is_some_and(|bot| Some(bot.app_name()) == app_name)
        })?;
        debug_assert!(pending.bot.is_some());
        state.claimed = true;
        Some(PreparedBot {
            app: app.clone(),
            connections: self.clone(),
            pending: state.pending.take(),
        })
    }
}

pub(crate) fn cancel_pending(app: &AppHandle) {
    if let Some(connections) = app.try_state::<BotConnections>() {
        connections.cancel_all(app);
    }
}

pub(crate) fn exit_after_pending(app: &AppHandle) {
    let Some(connections) = app.try_state::<BotConnections>() else {
        app.exit(0);
        return;
    };
    let (status, work) = {
        let mut state = connections.0.lock();
        if state.closing {
            return;
        }
        state.closing = true;
        let status = state
            .pending
            .take()
            .map(|pending| state.disconnect(pending));
        (status, state.work.take())
    };
    if let Some(status) = status {
        let _ = app.emit(EVENT, status);
    }
    if let Some(work) = work {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if tokio::time::timeout(EXIT_GRACE, work).await.is_err() {
                log::warn!("Timed out waiting for the prepared recording bot to disconnect");
            }
            app.exit(0);
        });
    } else {
        app.exit(0);
    }
}

/// Start owns this connection before opening media devices. If startup fails,
/// dropping the claim disconnects the same bot and informs the picker.
pub(crate) struct PreparedBot {
    app: AppHandle,
    connections: BotConnections,
    pending: Option<PendingBot>,
}

impl PreparedBot {
    pub(crate) fn provider(&self) -> &'static str {
        self.pending.as_ref().and_then(|p| p.bot.as_ref()).expect("claimed bot").app_name()
    }

    /// Check the live session, not the picker snapshot taken before the claim.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.pending.as_ref().and_then(|p| p.bot.as_ref())
            .filter(|bot| bot.ready_to_adopt())
            .map(|_| ())
            .ok_or_else(|| "The bot connection is unavailable. Connect the bot again before recording.".into())
    }

    pub(crate) fn adopt(
        mut self,
        meeting_id: &str,
        directory: &Path,
        origin: (Instant, u64),
        live: Option<crate::participant_live::ParticipantAudioSender>,
    ) -> Result<(Option<DiscordRecording>, Option<AttendeeRecording>), String> {
        self.validate()?;
        let bot = self
            .pending
            .as_mut()
            .and_then(|p| p.bot.as_mut())
            .expect("claimed bot");
        let result = match bot {
            Bot::Meet(bot) => bot.adopt(meeting_id, directory, origin, live),
            Bot::Discord(bot) => bot.adopt(meeting_id, directory, origin, live),
        };
        if let Err(error) = result {
            log::warn!("Could not adopt prepared recording bot: {error}");
            return Err("The bot connection is unavailable. Connect the bot again before recording.".into());
        }
        Ok(match self
            .pending
            .take()
            .and_then(|pending| pending.bot)
            .expect("claimed bot")
        {
            Bot::Meet(bot) => (None, Some(bot)),
            Bot::Discord(bot) => (Some(bot), None),
        })
    }
}

impl Drop for PreparedBot {
    fn drop(&mut self) {
        let status = {
            let mut state = self.connections.0.lock();
            state.claimed = false;
            self.pending.take().map(|pending| state.disconnect(pending))
        };
        if let Some(status) = status {
            let _ = self.app.emit(EVENT, status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn participant_only_policy_is_atomic_and_does_not_replace_a_previous_policy() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("recording");
        persist_live_policy(&directory, "meet", true).unwrap();
        let marker = directory.join("bot-live-policy.json");
        let original = std::fs::read(&marker).unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&original).unwrap(), json!({
            "schema": 1, "source": "participant_only", "provider": "meet", "capture_system": true,
        }));
        assert!(persist_live_policy(&directory, "discord", false).is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), original);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    }

    #[test]
    fn policy_failure_never_leaves_a_partial_marker() {
        let root = tempfile::tempdir().unwrap();
        let occupied = root.path().join("recording");
        std::fs::write(&occupied, "existing file").unwrap();
        assert!(persist_live_policy(&occupied, "discord", false).is_err());
        assert_eq!(std::fs::read_to_string(&occupied).unwrap(), "existing file");
        let invalid = root.path().join("invalid");
        assert!(persist_live_policy(&invalid, "unknown", false).is_err());
        assert!(!invalid.exists());
    }

    #[test]
    fn picker_wire_contract_uses_provider_specific_fields_and_revisioned_phases() {
        let mut meet = serde_json::from_value::<RecordingBotTarget>(json!({
            "provider": "meet",
            "meetingUrl": "https://meet.google.com/abc-defg-hij",
        }))
        .unwrap();
        meet.validate().unwrap();
        assert!(matches!(meet, RecordingBotTarget::Meet(_)));
        let discord = serde_json::from_value::<RecordingBotTarget>(json!({
            "provider": "discord",
            "guild_id": "1234",
            "channel_id": "5678",
        }))
        .unwrap();
        assert!(matches!(discord, RecordingBotTarget::Discord(_)));

        let request_id = Uuid::new_v4();
        let mut pending = PendingBot {
            generation: Uuid::new_v4(),
            expires_at: Instant::now() + CONNECTION_TTL,
            status: RecordingBotStatus {
                request_id,
                revision: 0,
                phase: ConnectionPhase::Connecting,
                message: None,
            },
            bot: None,
        };
        assert!(pending.set_phase(ConnectionPhase::WaitingAdmission));
        assert!(!pending.set_phase(ConnectionPhase::WaitingAdmission));
        assert_eq!(
            serde_json::to_value(&pending.status).unwrap(),
            json!({"requestId": request_id, "revision": 1, "phase": "waiting_admission", "message": null}),
        );
        assert!(pending.set_phase(ConnectionPhase::Disconnected));
        assert_eq!(pending.status.revision, 2);
    }
}

//! Optional Discord tracks are owned by the same local recording as the media writers.
use bazza_discord::{
    tracks::SAMPLE_RATE, LiveAudioSink, LiveVoiceFrame, VoiceSession, VoiceStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

use crate::bot_connection::ConnectionPhase;
use crate::participant_live::ParticipantAudioSender;

const EVENT: &str = "discord_recording_status";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DiscordRecordingSelection {
    guild_id: String,
    channel_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct DiscordRecordingStatus {
    meeting_id: String,
    state: &'static str,
}

pub(crate) struct DiscordRecording {
    app: AppHandle,
    meeting_id: watch::Sender<Option<String>>,
    paused: Arc<AtomicBool>,
    voice: Option<VoiceSession>,
}

type PrepareStatus = Box<dyn Fn(ConnectionPhase) + Send + Sync>;

impl DiscordRecording {
    pub(crate) fn start(
        app: &AppHandle,
        meeting_id: &str,
        directory: &Path,
        selection: DiscordRecordingSelection,
        origin: (Instant, u64),
        live: Option<ParticipantAudioSender>,
    ) -> Self {
        let voice = credentials(app, selection).map(|(guild_id, channel_id, token)| {
            tauri::async_runtime::block_on(async {
                let directory = directory.join("discord");
                match live {
                    Some(live) => VoiceSession::start_with_live_audio(
                        guild_id,
                        channel_id,
                        directory,
                        token,
                        origin.0,
                        live_sink(live),
                    ),
                    None => VoiceSession::start(guild_id, channel_id, directory, token),
                }
            })
        });
        Self::with_voice(app, Some(meeting_id.to_owned()), voice, None)
    }

    pub(crate) fn prepare(
        app: &AppHandle,
        selection: DiscordRecordingSelection,
        on_status: impl Fn(ConnectionPhase) + Send + Sync + 'static,
    ) -> Self {
        let voice = credentials(app, selection).map(|(guild_id, channel_id, token)| {
            tauri::async_runtime::block_on(async {
                VoiceSession::prepare(guild_id, channel_id, token)
            })
        });
        Self::with_voice(app, None, voice, Some(Box::new(on_status)))
    }

    pub(crate) fn adopt(
        &mut self,
        meeting_id: &str,
        directory: &Path,
        origin: (Instant, u64),
        live: Option<ParticipantAudioSender>,
    ) -> Result<(), String> {
        if self.meeting_id.borrow().is_some() {
            return Err("Discord preparation already adopted".into());
        }
        let voice = self
            .voice
            .as_mut()
            .ok_or("Discord preparation is unavailable")?;
        self.meeting_id.send_replace(Some(meeting_id.to_owned()));
        voice
            .adopt_with_live_audio(directory.join("discord"), origin, live.map(live_sink))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn ready_to_adopt(&self) -> bool {
        self.meeting_id.borrow().is_none()
            && self.voice.as_ref().is_some_and(|voice| {
                matches!(*voice.subscribe().borrow(), VoiceStatus::Connected | VoiceStatus::Receiving)
            })
    }

    fn with_voice(
        app: &AppHandle,
        meeting_id: Option<String>,
        voice: Option<VoiceSession>,
        mut on_status: Option<PrepareStatus>,
    ) -> Self {
        let paused = Arc::new(AtomicBool::new(false));
        let (meeting_id, mut meeting) = watch::channel(meeting_id);
        let this = Self {
            app: app.clone(),
            meeting_id,
            paused,
            voice,
        };
        if let Some(voice) = &this.voice {
            let mut status = voice.subscribe();
            let app = app.clone();
            let paused = this.paused.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let status_now = status.borrow_and_update().clone();
                    let meeting_now = meeting.borrow_and_update().clone();
                    let recording = meeting_now.is_some();
                    if let Some(id) = meeting_now {
                        on_status = None;
                        let state = label(&status_now, paused.load(Ordering::Acquire));
                        publish(&app, &id, state);
                        if state == "unavailable" {
                            warn_unavailable(&app);
                            break;
                        }
                    } else if let Some(notify) = &on_status {
                        let phase = match &status_now {
                            VoiceStatus::Connecting => ConnectionPhase::Connecting,
                            VoiceStatus::Connected | VoiceStatus::Receiving => {
                                ConnectionPhase::Connected
                            }
                            VoiceStatus::Stopped | VoiceStatus::Interrupted => {
                                ConnectionPhase::Disconnected
                            }
                            VoiceStatus::Failed(_) => ConnectionPhase::Failed,
                        };
                        notify(phase);
                    }
                    if matches!(
                        status_now,
                        VoiceStatus::Stopped | VoiceStatus::Interrupted | VoiceStatus::Failed(_)
                    ) {
                        if recording || meeting.changed().await.is_err() {
                            break;
                        }
                        continue;
                    }
                    let changed = tokio::select! {
                        biased;
                        result = meeting.changed() => result,
                        result = status.changed() => result,
                    };
                    if changed.is_err() {
                        break;
                    }
                }
            });
        } else if let Some(id) = this.meeting_id.borrow().as_deref() {
            publish(app, id, "unavailable");
            warn_unavailable(app);
        } else if let Some(notify) = on_status {
            tauri::async_runtime::spawn(async move { notify(ConnectionPhase::Failed) });
        }
        this
    }

    pub(crate) fn status(&self) -> DiscordRecordingStatus {
        let paused = self.paused.load(Ordering::Acquire);
        let state = self.voice.as_ref().map_or("unavailable", |voice| {
            label(&voice.subscribe().borrow(), paused)
        });
        DiscordRecordingStatus {
            meeting_id: self.meeting_id.borrow().clone().unwrap_or_default(),
            state,
        }
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        if let Some(voice) = &self.voice {
            voice.set_paused(paused);
            self.paused.store(paused, Ordering::Release);
            let _ = self.app.emit(EVENT, self.status());
        }
    }

    pub(crate) fn request_stop(&self) {
        if let Some(voice) = &self.voice {
            voice.request_stop();
        }
    }

    pub(crate) fn finish(self) {
        if let Some(voice) = self.voice {
            match tauri::async_runtime::block_on(voice.stop()) {
                Ok(outcome) if outcome.disconnected => {}
                _ => crate::recorder::notice_message(
                    &self.app,
                    "warning",
                    crate::ui_message::app(
                        "native.discordFinishUnconfirmed",
                        "Could not confirm that the Discord bot left cleanly.",
                    ),
                ),
            }
        }
    }
}

fn live_sink(live: ParticipantAudioSender) -> LiveAudioSink {
    Arc::new(move |frame: LiveVoiceFrame| {
        live.try_send(
            format!("discord:{}", frame.user_id),
            frame.display_name,
            frame.start_ms,
            SAMPLE_RATE,
            frame.samples,
        );
    })
}

fn credentials(
    app: &AppHandle,
    selection: DiscordRecordingSelection,
) -> Option<(u64, u64, String)> {
    let guild_id = snowflake(&selection.guild_id)?;
    let channel_id = snowflake(&selection.channel_id)?;
    let token = crate::discord_settings::recording_token(app, guild_id, channel_id).ok()?;
    Some((guild_id, channel_id, token))
}

fn snowflake(value: &str) -> Option<u64> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|id| *id != 0)
}

fn label(status: &VoiceStatus, paused: bool) -> &'static str {
    match status {
        VoiceStatus::Connecting => "connecting",
        VoiceStatus::Connected | VoiceStatus::Receiving if paused => "paused",
        VoiceStatus::Connected => "connected",
        VoiceStatus::Receiving => "receiving",
        VoiceStatus::Stopped => "stopped",
        VoiceStatus::Interrupted | VoiceStatus::Failed(_) => "unavailable",
    }
}

fn publish(app: &AppHandle, meeting_id: &str, state: &'static str) {
    if state != "stopped" {
        let _ = app.emit(
            EVENT,
            DiscordRecordingStatus {
                meeting_id: meeting_id.to_owned(),
                state,
            },
        );
    }
}

fn warn_unavailable(app: &AppHandle) {
    crate::recorder::notice_message(
        app,
        "warning",
        crate::ui_message::app(
            "native.discordCaptureUnavailable",
            "Discord audio is unavailable. Local recording continues.",
        ),
    );
}

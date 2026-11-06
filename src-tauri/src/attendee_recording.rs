//! Owns an Attendee bot from connection through an optional desktop recording.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use bazza_attendee::{Error, LiveAudioSink, Phase, RecordingClock, Session, Status};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::{bot_connection::ConnectionPhase, participant_live::ParticipantAudioSender};

const EVENT: &str = "attendee_recording_status";
static STARTUP: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttendeeRecordingSelection {
    pub(crate) meeting_url: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttendeeRecordingStatus {
    meeting_id: String,
    phase: &'static str,
    message: Option<String>,
}

pub(crate) struct AttendeeRecording {
    app: AppHandle,
    state: Arc<Mutex<State>>,
    clock: Option<RecordingClock>,
}

struct State {
    connection: Connection,
    events: Events,
    reported: Option<Status>,
}

#[derive(Clone)]
enum Events {
    Picker(Arc<dyn Fn(ConnectionPhase) + Send + Sync>),
    Meeting(String),
}

enum Launch {
    Prepare,
    Record {
        directory: PathBuf,
        clock: RecordingClock,
        live: Option<LiveAudioSink>,
    },
}

enum Connection {
    Starting,
    Active(ActiveBot),
    Finishing,
    Failed,
    Stopped,
}

struct ActiveBot {
    session: Session,
    _managed: Option<crate::attendee_managed::Runtime>,
}

impl AttendeeRecording {
    pub(crate) fn start(
        app: &AppHandle,
        meeting_id: &str,
        directory: &Path,
        selection: AttendeeRecordingSelection,
        origin: (Instant, u64),
        live: Option<ParticipantAudioSender>,
    ) -> Self {
        let clock = RecordingClock::from_origin(origin.0, origin.1);
        publish(app, meeting_id, Phase::Connecting, None);
        Self::launch(
            app,
            selection,
            Events::Meeting(meeting_id.to_owned()),
            Launch::Record {
                directory: directory.to_path_buf(),
                clock,
                live: live.map(live_sink),
            },
        )
    }

    pub(crate) fn prepare(
        app: &AppHandle,
        selection: AttendeeRecordingSelection,
        on_status: impl Fn(ConnectionPhase) + Send + Sync + 'static,
    ) -> Self {
        Self::launch(
            app,
            selection,
            Events::Picker(Arc::new(on_status)),
            Launch::Prepare,
        )
    }

    fn launch(
        app: &AppHandle,
        selection: AttendeeRecordingSelection,
        events: Events,
        launch: Launch,
    ) -> Self {
        let clock = match &launch {
            Launch::Prepare => None,
            Launch::Record { clock, .. } => Some(clock.clone()),
        };
        let state = Arc::new(Mutex::new(State {
            connection: Connection::Starting,
            events,
            reported: clock.as_ref().map(|_| Status {
                phase: Phase::Connecting,
                message: None,
            }),
        }));
        let recording = Self {
            app: app.clone(),
            state: Arc::clone(&state),
            clock,
        };
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            // WSL can take time to start. Keep capture and its controls available.
            // An immediate retry waits for a canceled startup to release its runtime.
            let startup = STARTUP.lock();
            if !matches!(state.lock().connection, Connection::Starting) {
                return;
            }
            let config = crate::attendee_settings::recording_config(&app);
            if !matches!(state.lock().connection, Connection::Starting) {
                return;
            }
            let bot = config
                .map_err(|error| error.to_string())
                .and_then(|(config, managed)| {
                    let callback_app = app.clone();
                    let callback_state = Arc::downgrade(&state);
                    let on_status = move |status| {
                        if let Some(state) = callback_state.upgrade() {
                            report(&callback_app, &state, Some(status));
                        }
                    };
                    let session = tauri::async_runtime::block_on(async move {
                        match launch {
                            Launch::Prepare => {
                                Session::prepare(config, selection.meeting_url, on_status).await
                            }
                            Launch::Record {
                                directory,
                                clock,
                                live,
                            } => Session::start_with_live_audio(
                                config,
                                selection.meeting_url,
                                directory,
                                clock,
                                on_status,
                                live,
                            ),
                        }
                    })
                    .map_err(|error| error.to_string())?;
                    Ok(ActiveBot {
                        session,
                        _managed: managed,
                    })
                });
            let mut current = state.lock();
            if !matches!(current.connection, Connection::Starting) {
                drop(current);
                if let Ok(bot) = bot {
                    bot.session.request_stop();
                    tauri::async_runtime::block_on(finish_bot(&app, bot));
                }
                return;
            }
            let failure = match bot {
                Ok(bot) => {
                    current.connection = Connection::Active(bot);
                    None
                }
                Err(error) => {
                    current.connection = Connection::Failed;
                    Some(Status {
                        phase: Phase::Failed,
                        message: Some(error),
                    })
                }
            };
            drop(current);
            drop(startup);
            // The task can report before its Session is installed in the state.
            // Read the current phase after installation so Connected always has a handle.
            report(&app, &state, failure);
        });
        recording
    }

    pub(crate) fn adopt(
        &mut self,
        meeting_id: &str,
        directory: &Path,
        origin: (Instant, u64),
        live: Option<ParticipantAudioSender>,
    ) -> Result<(), String> {
        let clock = RecordingClock::from_origin(origin.0, origin.1);
        let mut state = self.state.lock();
        let (Connection::Active(bot), Events::Picker(_)) = (&state.connection, &state.events)
        else {
            return Err("The Attendee bot is no longer available.".into());
        };
        bot.session
            .adopt_with_live_audio(directory, clock.clone(), live.map(live_sink))
            .map_err(|error| error.to_string())?;
        state.events = Events::Meeting(meeting_id.to_owned());
        state.reported = None;
        self.clock = Some(clock);
        drop(state);
        report(&self.app, &self.state, None);
        Ok(())
    }

    pub(crate) fn status(&self) -> AttendeeRecordingStatus {
        let state = self.state.lock();
        let meeting_id = match &state.events {
            Events::Picker(_) => "",
            Events::Meeting(meeting_id) => meeting_id,
        };
        let phase = match &state.connection {
            Connection::Starting => Phase::Connecting,
            Connection::Active(bot) => bot.session.status().phase,
            Connection::Finishing => state
                .reported
                .as_ref()
                .map_or(Phase::Finished, |status| status.phase),
            Connection::Failed => Phase::Failed,
            Connection::Stopped => Phase::Finished,
        };
        status(meeting_id, phase)
    }

    pub(crate) fn ready_to_adopt(&self) -> bool {
        let state = self.state.lock();
        matches!(&state.events, Events::Picker(_))
            && matches!(&state.connection, Connection::Active(bot)
                if matches!(bot.session.status().phase, Phase::Connected | Phase::Waiting | Phase::Receiving))
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        if let Some(clock) = &self.clock {
            clock.set_paused(paused);
        }
    }

    pub(crate) fn request_stop(&self) {
        let mut state = self.state.lock();
        if matches!(&state.events, Events::Picker(_)) {
            let connection = std::mem::replace(&mut state.connection, Connection::Stopped);
            drop(state);
            self.finish_background(connection);
            return;
        }
        match &state.connection {
            Connection::Starting => state.connection = Connection::Stopped,
            Connection::Active(bot) => bot.session.request_stop(),
            Connection::Finishing | Connection::Failed | Connection::Stopped => {}
        }
    }

    pub(crate) fn finish(self) {
        let connection =
            std::mem::replace(&mut self.state.lock().connection, Connection::Finishing);
        if let Connection::Active(bot) = connection {
            tauri::async_runtime::block_on(finish_bot(&self.app, bot));
        }
    }

    fn finish_background(&self, connection: Connection) {
        if let Connection::Active(bot) = connection {
            bot.session.request_stop();
            let app = self.app.clone();
            tauri::async_runtime::spawn(async move { finish_bot(&app, bot).await });
        }
    }
}

fn live_sink(live: ParticipantAudioSender) -> LiveAudioSink {
    Arc::new(move |id, name, start_ms, rate, samples| {
        live.try_send(format!("meet:{id}"), name, start_ms, rate, samples);
    })
}

impl Drop for AttendeeRecording {
    fn drop(&mut self) {
        let connection = std::mem::replace(&mut self.state.lock().connection, Connection::Stopped);
        self.finish_background(connection);
    }
}

async fn finish_bot(app: &AppHandle, bot: ActiveBot) {
    // Keep the managed runtime alive until the server confirms that the bot left.
    let ActiveBot { session, _managed } = bot;
    if matches!(
        session.finish().await,
        Err(Error::LeaveUnconfirmed | Error::JoinUnconfirmed | Error::RuntimeUnavailable)
    ) {
        crate::recorder::notice_message(
            app,
            "warning",
            crate::ui_message::app(
                "native.attendeeFinishUnconfirmed",
                "Could not confirm that the Attendee bot left the meeting.",
            ),
        );
    }
}

fn report(app: &AppHandle, state: &Mutex<State>, pending: Option<Status>) {
    let (events, status) = {
        let mut state = state.lock();
        let status = match &state.connection {
            Connection::Active(bot) => bot.session.status(),
            Connection::Failed | Connection::Finishing => match pending {
                Some(status) => status,
                None => return,
            },
            Connection::Starting | Connection::Stopped => return,
        };
        if state.reported.as_ref() == Some(&status) {
            return;
        }
        state.reported = Some(status.clone());
        (state.events.clone(), status)
    };
    match events {
        Events::Picker(notify) => {
            if let Some(message) = status.message {
                log::warn!("Attendee: {message}");
            }
            notify(match status.phase {
                Phase::Connecting => ConnectionPhase::Connecting,
                Phase::WaitingAdmission => ConnectionPhase::WaitingAdmission,
                Phase::Connected | Phase::Waiting | Phase::Receiving => ConnectionPhase::Connected,
                Phase::Failed => ConnectionPhase::Failed,
                Phase::Finished => ConnectionPhase::Disconnected,
            });
        }
        Events::Meeting(meeting_id) => {
            publish(app, &meeting_id, status.phase, status.message.as_deref())
        }
    }
}

fn status(meeting_id: &str, phase: Phase) -> AttendeeRecordingStatus {
    AttendeeRecordingStatus {
        meeting_id: meeting_id.to_owned(),
        phase: match phase {
            Phase::Connecting => "connecting",
            Phase::WaitingAdmission | Phase::Connected | Phase::Waiting => "waiting",
            Phase::Receiving => "receiving",
            Phase::Failed => "failed",
            Phase::Finished => "finished",
        },
        message: None,
    }
}

fn publish(app: &AppHandle, meeting_id: &str, phase: Phase, detail: Option<&str>) {
    let _ = app.emit(EVENT, status(meeting_id, phase));
    if let Some(detail) = detail {
        log::warn!("Attendee: {detail}");
    }
    let warning = match phase {
        Phase::Failed => crate::ui_message::app(
            "native.attendeeCaptureUnavailable",
            "Google Meet participant audio is unavailable. Check the Attendee connection.",
        ),
        Phase::Finished if detail.is_some() => crate::ui_message::app(
            "native.attendeeTracksIncomplete",
            "Attendee finished with incomplete audio or names. Check the meeting tracks.",
        ),
        _ => return,
    };
    crate::recorder::notice_message(app, "warning", warning);
}

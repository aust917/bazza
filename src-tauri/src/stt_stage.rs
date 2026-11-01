//! STT selection and persistence. Cloud transcription is explicit and never
//! owns the media writer; failures retain audio and confirmed provider text.
use crate::{events, recorder::notice, settings::Settings, state::AppState};
use hark_asr::{
    provider::{SttEvent, SttProvider, Transcript, TranscriptStatus},
    soniox::{SonioxConfig, SonioxWorker},
    Caption, LiveTranscriber,
};
use parking_lot::{Mutex, RwLock};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};

const KEY_ROW: &str = "private:soniox_api_key";
fn transcript_key(id: &str) -> String {
    format!("stt:{id}")
}

fn cloud_allowed(settings: &Settings) -> bool {
    settings.stt_provider == SttProvider::Soniox && settings.soniox_consent
}

fn revoke_registered(slot: &mut Option<Arc<AtomicBool>>) {
    if let Some(cancel) = slot.take() {
        cancel.store(true, Ordering::Release);
    }
}

/// Registration and credential lookup share the revocation locks. A settings
/// change cannot miss a worker that has not yet reached ActiveRecording.
fn register_cloud(
    settings: &RwLock<Settings>,
    cancel_slot: &Mutex<Option<Arc<AtomicBool>>>,
    load_key: impl FnOnce() -> Option<String>,
) -> Result<(Arc<AtomicBool>, String), String> {
    let current = settings.read();
    let mut slot = cancel_slot.lock();
    if !cloud_allowed(&current) {
        return Err("Soniox permission was revoked or the provider changed; local media continues".into());
    }
    let key = load_key().filter(|key| !key.is_empty()).ok_or("Soniox API key is missing; local media continues")?;
    let cancel = Arc::new(AtomicBool::new(false));
    revoke_registered(&mut slot);
    *slot = Some(cancel.clone());
    Ok((cancel, key))
}

pub fn commit_settings(
    settings: &RwLock<Settings>,
    cancel_slot: &Mutex<Option<Arc<AtomicBool>>>,
    new: Settings,
    save: impl FnOnce(&Settings) -> Result<(), String>,
) -> Result<String, String> {
    let mut current = settings.write();
    let mut slot = cancel_slot.lock();
    let old_hotkey = current.hotkey.clone();
    save(&new)?;
    *current = new;
    if !cloud_allowed(&current) {
        revoke_registered(&mut slot);
    }
    Ok(old_hotkey)
}

fn commit_key(cancel_slot: &Mutex<Option<Arc<AtomicBool>>>, key: &str, save: impl FnOnce() -> Result<(), String>) -> Result<bool, String> {
    let mut slot = cancel_slot.lock();
    save()?;
    if key.is_empty() {
        revoke_registered(&mut slot);
    }
    Ok(!key.is_empty())
}

#[tauri::command]
pub fn soniox_key_status(state: State<AppState>) -> bool {
    state.store.get_setting::<String>(KEY_ROW).ok().flatten().is_some_and(|k| !k.is_empty())
}

#[tauri::command]
pub fn save_soniox_key(state: State<AppState>, key: Option<String>) -> Result<bool, String> {
    let key = key.unwrap_or_default().trim().to_string();
    if key.len() > 4096 || key.contains(['\n', '\r']) {
        return Err("Invalid Soniox API key".into());
    }
    // Separate from serializable Settings and recording bundles; only presence
    // returns to the UI. Clearing writes an empty private value.
    commit_key(&state.cloud_stt_cancel, &key, || state.store.set_setting(KEY_ROW, &key).map_err(|_| "Could not save Soniox credential".to_string()))
}

#[tauri::command]
pub fn stt_transcript(state: State<AppState>, id: String) -> Result<Option<Transcript>, String> {
    if state.store.get_meeting(&id).map_err(|e| e.to_string())?.is_none() {
        return Err("No such meeting".into());
    }
    state.store.get_setting(&transcript_key(&id)).map_err(|e| e.to_string())
}

pub fn saved(state: &AppState, id: &str) -> Result<Option<Transcript>, String> {
    state.store.get_setting(&transcript_key(id)).map_err(|_| "Could not read saved STT selection; the previous transcript was kept".into())
}

pub fn persist(state: &AppState, id: &str, transcript: &Transcript) -> Result<(), String> {
    state.store.set_setting(&transcript_key(id), transcript).map_err(|_| "Could not save STT provenance and confirmed text to the local library".into())
}

/// Save the explicit selection before opening any devices or network session.
/// A storage failure must never make automatic post-processing fall back to
/// Whisper because the cloud provider marker was missing.
pub fn prepare_selection(state: &AppState, id: &str, settings: &Settings) -> Result<(), String> {
    if settings.stt_provider == SttProvider::Soniox {
        persist(state, id, &Transcript::new(SttProvider::Soniox, settings.soniox_model.clone(), settings.language.clone()))?;
    }
    Ok(())
}

pub enum SttWorker {
    Local(LiveTranscriber),
    Soniox(SonioxWorker),
}
impl SttWorker {
    pub fn join(self) {
        match self {
            Self::Local(worker) => worker.join(),
            Self::Soniox(worker) => worker.join(),
        }
    }
}
pub struct LiveSession {
    pub input: Option<crossbeam_channel::Sender<Vec<f32>>>,
    pub worker: Option<SttWorker>,
    pub captions: Option<JoinHandle<()>>,
    pub overflow: Option<Arc<AtomicBool>>,
}

pub fn start_live(app: &AppHandle, meeting_id: &str, settings: &Settings) -> LiveSession {
    let state = app.state::<AppState>();
    if settings.stt_provider == SttProvider::Local {
        let tier = settings.tier(&state.hardware);
        let (live_id, _, _) = settings.model_ids(&state.catalog, tier);
        let Some(engine) = state.whisper_or_any(&[&live_id]) else {
            notice(app, "info", "Live captions off: transcription model not downloaded yet.".into());
            return LiveSession { input: None, worker: None, captions: None, overflow: None };
        };
        let (pcm_tx, pcm_rx) = crossbeam_channel::unbounded();
        let (cap_tx, cap_rx) = crossbeam_channel::unbounded::<Caption>();
        let live = LiveTranscriber::start(engine, pcm_rx, cap_tx, settings.language.clone());
        let (app, meeting_id) = (app.clone(), meeting_id.to_string());
        let captions = std::thread::spawn(move || {
            for caption in cap_rx {
                crate::live_feed::LiveFeed::push_for(&app.state::<AppState>(), &meeting_id, &caption);
                let _ = app.emit(events::CAPTION, caption);
            }
        });
        return LiveSession { input: Some(pcm_tx), worker: Some(SttWorker::Local(live)), captions: Some(captions), overflow: None };
    }

    let mut transcript = Transcript::new(SttProvider::Soniox, settings.soniox_model.clone(), settings.language.clone());
    let registered = register_cloud(&state.settings, &state.cloud_stt_cancel, || state.store.get_setting::<String>(KEY_ROW).ok().flatten());
    let (overflow, key) = match registered {
        Ok(registered) => registered,
        Err(reason) => {
            transcript.apply(&SttEvent::Finished(Err(reason.clone())));
            if let Err(error) = persist(&state, meeting_id, &transcript) {
                notice(app, "error", error);
            }
            notice(app, "warning", reason);
            return LiveSession { input: None, worker: None, captions: None, overflow: None };
        }
    };
    let (pcm_tx, pcm_rx) = crossbeam_channel::bounded(256);
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let worker = SonioxWorker::start(
        SonioxConfig { api_key: key, model: settings.soniox_model.clone(), language: settings.language.clone() },
        pcm_rx,
        event_tx,
        overflow.clone(),
    );
    let (app, meeting_id) = (app.clone(), meeting_id.to_string());
    let captions = std::thread::spawn(move || {
        let mut last_save = Instant::now();
        let mut dirty = false;
        let mut warned_save = false;
        for event in event_rx {
            transcript.apply(&event);
            match &event {
                SttEvent::Caption(caption) => {
                    crate::live_feed::LiveFeed::push_for(&app.state::<AppState>(), &meeting_id, caption);
                    let _ = app.emit(events::CAPTION, caption);
                    dirty |= caption.is_final;
                }
                SttEvent::Reconnecting => {
                    notice(&app, "warning", "Soniox connection interrupted; retrying unconfirmed audio. Local recording continues.".into())
                }
                SttEvent::Finished(result) => {
                    if let Err(error) = result {
                        notice(&app, "warning", format!("{error}. Local media and confirmed transcript text were kept."));
                    }
                    dirty = true;
                }
            }
            if dirty && (last_save.elapsed() >= Duration::from_secs(1) || matches!(event, SttEvent::Finished(_))) {
                if let Err(error) = persist(&app.state::<AppState>(), &meeting_id, &transcript) {
                    if !warned_save {
                        notice(&app, "error", error);
                        warned_save = true;
                    }
                }
                dirty = false;
                last_save = Instant::now();
            }
        }
        if transcript.status == TranscriptStatus::Streaming {
            transcript.apply(&SttEvent::Finished(Err("Soniox worker ended before finalization".into())));
        }
        if let Err(error) = persist(&app.state::<AppState>(), &meeting_id, &transcript) {
            notice(&app, "error", error);
        }
    });
    LiveSession { input: Some(pcm_tx), worker: Some(SttWorker::Soniox(worker)), captions: Some(captions), overflow: Some(overflow) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed_settings() -> Settings {
        Settings { stt_provider: SttProvider::Soniox, soniox_consent: true, ..Settings::default() }
    }

    #[test]
    fn current_permissions_and_key_are_checked_before_registering_a_worker() {
        let settings = RwLock::new(Settings::default());
        let slot = Mutex::new(None);
        assert!(register_cloud(&settings, &slot, || panic!("must not load a key without consent")).is_err());
        *settings.write() = allowed_settings();
        assert!(register_cloud(&settings, &slot, || Some(String::new())).is_err());
        assert!(slot.lock().is_none());
    }

    #[test]
    fn changing_to_local_or_revoking_consent_cancels_an_active_worker_without_joining() {
        for revoke_consent in [false, true] {
            let settings = RwLock::new(allowed_settings());
            let slot = Mutex::new(None);
            let (token, _) = register_cloud(&settings, &slot, || Some("private-key".into())).unwrap();
            let mut next = allowed_settings();
            if revoke_consent {
                next.soniox_consent = false;
            } else {
                next.stt_provider = SttProvider::Local;
            }
            commit_settings(&settings, &slot, next, |_| Ok(())).unwrap();
            assert!(token.load(Ordering::Acquire));
            assert!(slot.lock().is_none());
            assert!(register_cloud(&settings, &slot, || Some("private-key".into())).is_err());
        }
    }

    #[test]
    fn revocation_cannot_miss_a_start_that_has_not_published_active_recording() {
        // Pause registration inside credential lookup, before the worker exists.
        // A revoker must then wait for registration and cancel the new token.
        for clear_key in [false, true] {
            let settings = Arc::new(RwLock::new(allowed_settings()));
            let slot = Arc::new(Mutex::new(None));
            let key = Arc::new(Mutex::new("private-key".to_string()));
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let (register_settings, register_slot, register_key) = (settings.clone(), slot.clone(), key.clone());
            let registering = std::thread::spawn(move || {
                register_cloud(&register_settings, &register_slot, || {
                    entered_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Some(register_key.lock().clone())
                })
                .unwrap()
                .0
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(slot.try_lock().is_none());
            assert!(settings.try_write().is_none());
            let (revoke_settings, revoke_slot, revoke_key) = (settings.clone(), slot.clone(), key.clone());
            let revoking = std::thread::spawn(move || {
                if clear_key {
                    commit_key(&revoke_slot, "", || {
                        revoke_key.lock().clear();
                        Ok(())
                    })
                    .unwrap();
                } else {
                    commit_settings(&revoke_settings, &revoke_slot, Settings::default(), |_| Ok(())).unwrap();
                }
            });
            resume_tx.send(()).unwrap();
            let token = registering.join().unwrap();
            revoking.join().unwrap();
            assert!(token.load(Ordering::Acquire));
            assert!(slot.lock().is_none());
            assert!(register_cloud(&settings, &slot, || Some(key.lock().clone())).is_err());
        }
    }
}

use super::CmdResult;
use crate::events::{self, ModelProgressPayload};
use crate::state::{AppState, RuntimeState};
use bazza_models::{Hardware, ModelSpec, Tier};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

enum Runtime<'a> {
    Service(State<'a, RuntimeState>),
    Legacy(State<'a, AppState>),
}

impl<'a> Runtime<'a> {
    fn get(app: &'a AppHandle) -> Self {
        app.try_state::<RuntimeState>().map_or_else(|| Self::Legacy(app.state()), Self::Service)
    }

    fn models_dir(&self) -> std::path::PathBuf {
        match self {
            Self::Service(state) => state.models_dir(),
            Self::Legacy(state) => state.models_dir(),
        }
    }
}

impl std::ops::Deref for Runtime<'_> {
    type Target = RuntimeState;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Service(state) => state,
            Self::Legacy(state) => state,
        }
    }
}

#[derive(Serialize)]
pub struct HardwareInfo {
    pub hardware: Hardware,
    pub tier: Tier,
    pub effective_tier: Tier,
}

#[tauri::command]
pub fn probe_hardware(app: AppHandle) -> HardwareInfo {
    let state = Runtime::get(&app);
    // `tier` = what Auto would pick (the UI marks it "Recommended"); it must
    // agree with the effective tier when there is no override.
    let settings = state.settings.read().clone();
    let tier = crate::settings::Settings { tier_override: None, ..settings.clone() }.tier(&state.hardware);
    let effective_tier = settings.tier(&state.hardware);
    HardwareInfo { hardware: state.hardware.clone(), tier, effective_tier }
}

#[derive(Serialize)]
pub struct ModelRow {
    pub spec: ModelSpec,
    pub present: bool,
    pub downloading: bool,
    /// Which slot(s) the effective tier uses this model for.
    pub roles: Vec<&'static str>,
}

#[tauri::command]
pub fn list_models(app: AppHandle) -> Vec<ModelRow> {
    let state = Runtime::get(&app);
    let dir = state.models_dir();
    let settings = state.settings.read();
    let tier = settings.tier(&state.hardware);
    let (live, quality, llm) = settings.model_ids(&state.catalog, tier);
    let downloads = state.downloads.lock();
    state
        .catalog
        .all()
        .iter()
        .map(|spec| {
            let mut roles = Vec::new();
            if spec.id == live {
                roles.push("live");
            }
            if spec.id == quality {
                roles.push("quality");
            }
            if spec.id == llm {
                roles.push("llm");
            }
            if state.catalog.common_ids().iter().any(|c| c == &spec.id) {
                roles.push(if spec.kind == bazza_models::ModelKind::Embedding { "search" } else { "speakers" });
            }
            ModelRow {
                spec: spec.clone(),
                present: bazza_models::is_present(&dir, spec),
                downloading: downloads.contains_key(&spec.id),
                roles,
            }
        })
        .collect()
}

#[tauri::command]
pub async fn download_model(app: AppHandle, id: String) -> CmdResult<()> {
    let state = Runtime::get(&app);
    let spec = state.catalog.get(&id).ok_or_else(|| crate::ui_message::app("native.modelUnknown", "unknown model"))?.clone();
    let dir = state.models_dir();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut d = state.downloads.lock();
        if d.contains_key(&id) {
            return Err(crate::ui_message::app("native.modelDownloading", "already downloading"));
        }
        d.insert(id.clone(), cancel.clone());
    }
    let progress_app = app.clone();
    let pid = id.clone();
    let result = bazza_models::download(
        &spec,
        &dir,
        move |done, total| {
            let _ = progress_app.emit(
                events::MODEL_PROGRESS,
                ModelProgressPayload { id: pid.clone(), done, total, status: "downloading", error: None },
            );
        },
        {
            let c = cancel.clone();
            move || c.load(Ordering::Relaxed)
        },
    )
    .await;
    state.downloads.lock().remove(&id);
    match result {
        Ok(_) => {
            let _ = app.emit(
                events::MODEL_PROGRESS,
                ModelProgressPayload { id, done: spec.size_bytes, total: spec.size_bytes, status: "done", error: None },
            );
            if matches!(&state, Runtime::Legacy(_)) {
                if spec.kind == bazza_models::ModelKind::Llm {
                    crate::pipeline::run_deferred_ai(app.clone());
                } else if spec.kind == bazza_models::ModelKind::Embedding {
                    crate::pipeline::run_deferred_embeddings(app.clone());
                }
            }
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            let _ = app.emit(
                events::MODEL_PROGRESS,
                ModelProgressPayload { id, done: 0, total: spec.size_bytes, status: "failed", error: Some(msg.clone()) },
            );
            Err(msg.into())
        }
    }
}

#[tauri::command]
pub fn cancel_download(app: AppHandle, id: String) {
    let state = Runtime::get(&app);
    if let Some(c) = state.downloads.lock().get(&id) {
        c.store(true, Ordering::Relaxed);
    };
}

#[tauri::command]
pub fn remove_model(app: AppHandle, id: String) -> CmdResult<()> {
    let state = Runtime::get(&app);
    let spec = state.catalog.get(&id).ok_or_else(|| crate::ui_message::app("native.modelUnknown", "unknown model"))?;
    state.engines.lock().whisper.remove(&id);
    let p = bazza_models::model_path(&state.models_dir(), spec);
    if p.exists() {
        std::fs::remove_file(&p).map_err(super::err)?;
    }
    Ok(())
}

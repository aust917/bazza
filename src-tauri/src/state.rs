use crate::recorder::ActiveRecording;
use crate::settings::Settings;
use crate::ui_locale::UiLocale;
use bazza_asr::WhisperEngine;
use bazza_llm::LlmBackend;
use bazza_models::{Catalog, Hardware};
use bazza_service_client::{
    api::{
        desktop::DesktopLocale,
        rpc::{ErrorCode, RpcError},
        templates::{TemplateSeedRequest, MAX_TEMPLATE_DATA_BYTES},
    },
    ClientError, RepositoryClient,
};
use bazza_store::Store;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

/// Loaded AI engines, keyed by model id. Loaded lazily, dropped on `unload_all`.
pub const EMBED_MODEL_ID: &str = "bge-small-en-v1.5-q8";

#[derive(Default)]
pub struct Engines {
    pub whisper: HashMap<String, Arc<WhisperEngine>>,
    /// (backend key, backend) - key changes when settings change.
    pub llm: Option<(String, Arc<dyn LlmBackend>)>,
    pub embed: Option<Arc<bazza_llm::EmbedEngine>>,
    pub last_used: Option<Instant>,
}

/// Local settings snapshots, windows, engines and their in-process tasks.
/// The library owner is provided independently by the startup mode.
pub struct RuntimeState {
    model_root: PathBuf,
    pub settings: RwLock<Settings>,
    pub ui_locale: RwLock<UiLocale>,
    /// Serialize persistence, native relabeling and the broadcast as one update.
    pub ui_locale_update: Mutex<()>,
    pub hardware: Hardware,
    pub catalog: Catalog,
    /// Captions + running notes of the recording in progress (for the Meeting page).
    pub live_feed: Mutex<Option<crate::live_feed::LiveFeed>>,
    /// Pipelines (post-processing, re-runs, chat) currently using the engines;
    /// the idle unloader leaves them alone while this is > 0.
    pub busy: std::sync::atomic::AtomicUsize,
    pub engines: Mutex<Engines>,
    /// The service desktop's native worker and optional track transcription share local ASR resources.
    pub(crate) local_asr_work: Mutex<()>,
    /// Concurrent async chat/summary requests must share one cold model load.
    llm_load: Mutex<()>,
    /// Detector is paused (user toggle or while recording).
    pub detection_paused: AtomicBool,
    pub snooze_until: Mutex<Option<Instant>>,
    pub ffmpeg: Option<PathBuf>,
    pub diarize_bin: Option<PathBuf>,
    /// Model downloads in flight; value = cancel flag.
    pub downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Streaming chat replies in flight, by chat id.
    pub chat_cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Explicit subscription-agent summaries in flight, by meeting id.
    pub summary_cancels: Mutex<HashMap<String, SummaryJob>>,
    /// Per-meeting counter bumped on every speaker rename; lets the delayed
    /// summary refresh run once after a burst of renames.
    pub rename_seq: Mutex<HashMap<String, u64>>,
    /// Cached calendar events from the configured .ics sources.
    pub calendar: RwLock<Vec<bazza_calendar::CalEvent>>,
    pub calendar_errors: RwLock<Vec<String>>,
}

/// State for the legacy GUI's direct Store and native writer lifecycle.
pub struct AppState {
    pub store: Arc<Store>,
    pub recording: Mutex<Option<ActiveRecording>>,
    pub processing_jobs: crate::processing_jobs::ProcessingJobs,
    /// Serializes explicit library copies against capture start and job admission.
    pub data_copy: RwLock<()>,
    pub share_ops: Mutex<()>,
    /// Registered before a cloud worker starts, including pending recording starts.
    /// Lock order for STT permission/credential changes: settings -> cancel -> store.
    pub cloud_stt_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// LAN share server, running only while at least one share is enabled.
    pub share_server: Mutex<Option<bazza_share::ShareServerHandle>>,
    runtime: RuntimeState,
}

impl Deref for AppState {
    type Target = RuntimeState;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

pub struct SummaryJob {
    pub cancel: Arc<AtomicBool>,
    pub agent: Option<bazza_summary_runner::Agent>,
}

impl RuntimeState {
    fn new(settings: Settings, ui_locale: UiLocale, hardware: Hardware, model_root: PathBuf) -> Self {
        Self {
            model_root,
            settings: RwLock::new(settings),
            ui_locale: RwLock::new(ui_locale),
            ui_locale_update: Mutex::new(()),
            hardware,
            catalog: Catalog::builtin(),
            engines: Mutex::new(Engines::default()),
            local_asr_work: Mutex::new(()),
            llm_load: Mutex::new(()),
            detection_paused: AtomicBool::new(false),
            snooze_until: Mutex::new(None),
            ffmpeg: find_sidecar("ffmpeg"),
            diarize_bin: find_sidecar("bazza-diarize"),
            downloads: Mutex::new(HashMap::new()),
            chat_cancels: Mutex::new(HashMap::new()),
            summary_cancels: Mutex::new(HashMap::new()),
            rename_seq: Mutex::new(HashMap::new()),
            live_feed: Mutex::new(None),
            busy: std::sync::atomic::AtomicUsize::new(0),
            calendar: RwLock::new(Vec::new()),
            calendar_errors: RwLock::new(Vec::new()),
        }
    }

    /// Initialize before publishing this repository to other window tasks.
    /// Keep one library/epoch throughout startup and use the exact catalog
    /// revision when asking the owner to seed built-ins.
    pub(crate) async fn initialize_from_service(
        repository: &RepositoryClient,
    ) -> Result<Self, ClientError> {
        Self::initialize_from_service_with(repository, bazza_models::probe).await
    }

    async fn initialize_from_service_with(
        repository: &RepositoryClient,
        hardware: impl FnOnce() -> Hardware,
    ) -> Result<Self, ClientError> {
        let resync = || ClientError::from(RpcError::new(ErrorCode::ResyncRequired));
        let bootstrap = repository.bootstrap().await?;
        let native = repository.native_configuration().await?;
        if native.identity != bootstrap.identity {
            return Err(resync());
        }
        // These private DTOs share the native Settings serialization. Report
        // a fixed protocol error rather than printing a settings parse error.
        let settings: Settings = serde_json::to_value(native.data.settings)
            .and_then(serde_json::from_value)
            .map_err(|_| ClientError::Protocol)?;
        if settings.hotkey != bootstrap.data.preferences.hotkey
            || settings.close_to_tray != bootstrap.data.preferences.close_to_tray
        {
            return Err(resync());
        }

        let catalog = repository
            .templates_list(None, 1, MAX_TEMPLATE_DATA_BYTES as u32)
            .await?;
        if catalog.identity != bootstrap.identity {
            return Err(resync());
        }
        let seeded = repository
            .templates_seed_missing(TemplateSeedRequest {
                // One request per observed catalog revision. Two windows
                // starting on the same snapshot can replay the same seed.
                idempotency_key: catalog.catalog_revision.clone(),
                expected_catalog_revision: catalog.catalog_revision,
            })
            .await?;
        let confirmed = repository.preferences().await?;
        if seeded.identity != bootstrap.identity
            || confirmed.identity != bootstrap.identity
            || confirmed.data != bootstrap.data.preferences
        {
            return Err(resync());
        }

        let ui_locale = match confirmed.data.ui_locale {
            DesktopLocale::Ru => UiLocale::Ru,
            DesktopLocale::En => UiLocale::En,
        };
        Ok(Self::new(settings, ui_locale, hardware(), repository.root().join("models")))
    }

    /// RAII marker: engines stay loaded while any `BusyGuard` is alive.
    pub fn busy_guard(&self) -> BusyGuard<'_> {
        self.busy.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        BusyGuard(self)
    }

    pub fn unload_engines(&self) {
        let mut e = self.engines.lock();
        e.whisper.clear();
        e.llm = None;
        e.embed = None;
        e.last_used = None;
    }
}

impl AppState {
    pub fn new(store: Store) -> Self {
        let settings = Settings::load(&store);
        let ui_locale = UiLocale::load(&store);
        let builtins: Vec<bazza_store::Template> = bazza_llm::summary::builtin_templates()
            .into_iter()
            .map(|t| bazza_store::Template { id: t.id.to_string(), name: t.name, description: t.description, body: t.body, builtin: true })
            .collect();
        if let Err(e) = store.seed_templates(&builtins) {
            log::error!("seed templates: {e}");
        }
        let runtime = RuntimeState::new(settings, ui_locale, bazza_models::probe(), store.root().join("models"));
        Self {
            store: Arc::new(store),
            recording: Mutex::new(None),
            processing_jobs: crate::processing_jobs::ProcessingJobs::default(),
            data_copy: RwLock::new(()),
            share_ops: Mutex::new(()),
            cloud_stt_cancel: Mutex::new(None),
            share_server: Mutex::new(None),
            runtime,
        }
    }

    pub fn models_dir(&self) -> PathBuf {
        self.store.models_dir()
    }
}

impl RuntimeState {
    /// Loading an existing model never creates directories. Download and
    /// relocation remain the responsibility of the owning library.
    pub fn models_dir(&self) -> PathBuf {
        self.model_root.clone()
    }

    /// Path of a catalog model if downloaded.
    pub fn model_file(&self, id: &str) -> Option<PathBuf> {
        let spec = self.catalog.get(id)?;
        let dir = self.models_dir();
        bazza_models::is_present(&dir, spec).then(|| bazza_models::model_path(&dir, spec))
    }

    /// Get or load a whisper engine for `model_id`. `None` if the model isn't downloaded.
    pub fn whisper(&self, model_id: &str) -> Option<Arc<WhisperEngine>> {
        {
            let mut e = self.engines.lock();
            if let Some(w) = e.whisper.get(model_id).cloned() {
                e.last_used = Some(Instant::now());
                return Some(w);
            }
        }
        let path = self.model_file(model_id)?;
        let use_gpu = self.hardware.gpu.is_some();
        log::info!("loading whisper {model_id} from {}", path.display());
        match WhisperEngine::load(&path, use_gpu) {
            Ok(w) => {
                let w = Arc::new(w.tagged(model_id));
                let mut e = self.engines.lock();
                e.whisper.insert(model_id.to_string(), w.clone());
                e.last_used = Some(Instant::now());
                Some(w)
            }
            Err(err) => {
                log::error!("load whisper {model_id}: {err}");
                None
            }
        }
    }

    /// Preferred model if downloaded, else any downloaded ASR model (largest first).
    pub fn whisper_or_any(&self, preferred: &[&str]) -> Option<Arc<WhisperEngine>> {
        for id in preferred {
            if let Some(w) = self.whisper(id) {
                return Some(w);
            }
        }
        let dir = self.models_dir();
        let mut candidates: Vec<&bazza_models::ModelSpec> = self
            .catalog
            .all()
            .iter()
            .filter(|m| m.kind == bazza_models::ModelKind::Asr && bazza_models::is_present(&dir, m))
            .collect();
        candidates.sort_by_key(|m| std::cmp::Reverse(m.size_bytes));
        candidates.into_iter().find_map(|m| self.whisper(&m.id))
    }

    /// Legacy segmentation stays available for voice-sample enrollment.
    pub fn diarize_models(&self) -> Option<(PathBuf, PathBuf)> {
        Some((self.model_file("pyannote-seg-3")?, self.model_file("titanet-small")?))
    }

    /// Nemotron meeting diarization does not alter the TitaNet voice identity.
    pub fn nemotron_model(&self) -> Option<PathBuf> {
        let model = self.model_file("nemotron3-q8")?;
        // Official v0.3.0 macOS binaries require 13.3. Keep the application's
        // inherited 13.0 support through the existing pyannote path.
        #[cfg(target_os = "macos")]
        {
            let version = std::process::Command::new("/usr/bin/sw_vers").arg("-productVersion").output().ok()?;
            let text = String::from_utf8_lossy(&version.stdout);
            let mut parts = text.trim().split('.').filter_map(|v| v.parse::<u32>().ok());
            let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
            if !version.status.success() || (major, minor) < (13, 3) {
                log::warn!("Nemotron runtime requires macOS 13.3; using legacy diarization");
                return None;
            }
        }
        Some(model)
    }

    pub fn nemotron_runtime(&self) -> Option<PathBuf> {
        let relative = PathBuf::from("nemo-speech").join("0.3.0")
            .join(env!("BAZZA_TARGET_TRIPLE"))
            .join(if cfg!(windows) { "bin" } else { "lib" });
        let filename = if cfg!(windows) { "nemo_speech_asr_c.dll" } else { "libnemo_speech_asr_c.dylib" };
        let mut roots = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                roots.push(dir.to_path_buf());
                roots.push(dir.join("../Resources"));
            }
        }
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources"));
        roots.into_iter().map(|root| root.join(&relative))
            .find(|dir| dir.join(filename).is_file())
    }

    /// The configured chat model: bundled llama.cpp (if the GGUF is downloaded)
    /// or an OpenAI-compatible endpoint. `None` when unavailable.
    pub fn llm(&self) -> Option<Arc<dyn LlmBackend>> {
        let settings = self.settings.read().clone();
        self.llm_with_settings(&settings)
    }

    pub(crate) fn bundled_llm(&self, settings: &Settings) -> Option<Arc<dyn LlmBackend>> {
        if settings.llm_backend != "bundled" {
            return None;
        }
        self.llm_with_settings(settings)
    }

    fn llm_with_settings(&self, s: &Settings) -> Option<Arc<dyn LlmBackend>> {
        let _load = self.llm_load.lock();
        let tier = s.tier(&self.hardware);
        let (_, _, llm_id) = s.model_ids(&self.catalog, tier);
        let key = if s.llm_backend == "openai" {
            format!("openai|{}|{}|{}", s.llm_endpoint, s.llm_endpoint_model, s.llm_api_key.is_some())
        } else {
            format!("bundled|{llm_id}")
        };
        {
            let mut e = self.engines.lock();
            let cached = e.llm.as_ref().filter(|(k, _)| *k == key).map(|(_, b)| b.clone());
            if let Some(b) = cached {
                e.last_used = Some(Instant::now());
                return Some(b);
            }
        }
        let backend: Arc<dyn LlmBackend> = if s.llm_backend == "openai" {
            Arc::new(bazza_llm::OpenAiCompat::new(&s.llm_endpoint, s.llm_api_key.as_deref(), &s.llm_endpoint_model))
        } else {
            let path = self.model_file(&llm_id)?;
            let gpu_layers = if self.hardware.gpu.is_some() { 999 } else { 0 };
            match bazza_llm::LlamaEngine::load(&path, gpu_layers, 8192) {
                Ok(e) => Arc::new(e),
                Err(err) => {
                    log::error!("load llm {llm_id}: {err}");
                    return None;
                }
            }
        };
        let mut e = self.engines.lock();
        e.llm = Some((key, backend.clone()));
        e.last_used = Some(Instant::now());
        Some(backend)
    }

    /// The embedding model for semantic search, if downloaded.
    pub fn embed(&self) -> Option<Arc<bazza_llm::EmbedEngine>> {
        {
            let mut e = self.engines.lock();
            if let Some(m) = e.embed.clone() {
                e.last_used = Some(Instant::now());
                return Some(m);
            }
        }
        let path = self.model_file(EMBED_MODEL_ID)?;
        let engine = match bazza_llm::EmbedEngine::load(&path) {
            Ok(e) => Arc::new(e),
            Err(err) => {
                log::error!("load embed model: {err}");
                return None;
            }
        };
        let mut e = self.engines.lock();
        e.embed = Some(engine.clone());
        e.last_used = Some(Instant::now());
        Some(engine)
    }
}

/// Locate a sidecar binary: next to the executable (bundled), then the dev
/// `src-tauri/binaries/` folder, then the cargo target dir, then PATH.
pub(crate) fn find_sidecar(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let exe_name = exe_name.as_str();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(exe_name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let triple = env!("BAZZA_TARGET_TRIPLE");
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{name}-{triple}{}", if cfg!(windows) { ".exe" } else { "" }));
    if dev.exists() {
        return Some(dev);
    }
    which(exe_name)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

pub struct BusyGuard<'a>(&'a RuntimeState);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.engines.lock().last_used = Some(Instant::now());
    }
}

#[cfg(all(test, windows))]
#[path = "state_startup_tests.rs"]
mod startup_tests;

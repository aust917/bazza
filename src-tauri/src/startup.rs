//! Library state is admitted after the single-instance plugin and initialized
//! before Tauri creates configured webviews or dispatches their first commands.
use tauri::{plugin::TauriPlugin, AppHandle, Runtime};

pub(crate) fn library_state_plugin<R: Runtime>(
    initialize: impl FnOnce(&AppHandle<R>) -> Result<(), Box<dyn std::error::Error>> + Send + 'static,
) -> TauriPlugin<R> {
    tauri::plugin::Builder::new("bazza-library-state")
        .setup(move |app, _api| initialize(app))
        .build()
}

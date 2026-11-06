pub mod ai;
pub mod codex_mcp;
pub mod devices;
pub mod meetings;
pub mod models;
pub mod organize;
pub mod recording;
pub(crate) mod service_library;
pub(crate) mod service_summary;
pub mod settings;
pub mod speakers;

/// Stable message codes localize app errors without rewriting external diagnostics.
pub type CmdResult<T> = Result<T, crate::ui_message::UiMessage>;

pub fn err<E: std::fmt::Display>(e: E) -> crate::ui_message::UiMessage {
    e.to_string().into()
}

/// Run device, filesystem and model work without occupying the window event loop.
pub async fn blocking<T: Send + 'static>(work: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(err)?
}

#[cfg(test)]
mod performance_tests;

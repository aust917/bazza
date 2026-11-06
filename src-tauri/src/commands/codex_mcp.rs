use super::CmdResult;
use bazza_service_client::RepositoryClient;
use tauri::{State, WebviewWindow};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Status {
    enabled: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ClaudeStatus {
    code_enabled: bool,
    desktop_enabled: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeClient {
    Code,
    Desktop,
}

#[tauri::command]
pub async fn get_codex_mcp_status(
    window: WebviewWindow,
    repository: State<'_, RepositoryClient>,
) -> CmdResult<Option<Status>> {
    if window.label() != "main" {
        return Err("Codex MCP settings belong to the main window".into());
    }
    #[cfg(windows)]
    return invoke(repository.root(), "codex-status").await;
    #[cfg(not(windows))]
    {
        let _ = repository;
        Ok(None)
    }
}

#[tauri::command]
pub async fn set_codex_mcp_enabled(
    window: WebviewWindow,
    repository: State<'_, RepositoryClient>,
    enabled: bool,
) -> CmdResult<Status> {
    if window.label() != "main" {
        return Err("Codex MCP settings belong to the main window".into());
    }
    #[cfg(windows)]
    {
        let action = if enabled {
            "enable-codex"
        } else {
            "disable-codex"
        };
        if let Some(status) = invoke(repository.root(), action).await? {
            return Ok(status);
        }
    }
    #[cfg(not(windows))]
    let _ = (repository, enabled);
    Err("Codex MCP setup is unavailable for this library".into())
}

#[cfg(windows)]
async fn invoke<T: serde::de::DeserializeOwned>(
    root: &std::path::Path,
    action: &str,
) -> CmdResult<Option<T>> {
    use bazza_service_client::installed::Install;
    use std::{process::Stdio, time::Duration};

    let Some(install) = Install::load(root)
        .map_err(super::err)?
        .filter(|state| state.installed)
    else {
        return Ok(None);
    };
    let adapter = std::env::current_exe()
        .map_err(super::err)?
        .with_file_name("bazza-mcp.exe");
    if install.adapter != adapter {
        return Ok(None);
    }
    let mut command = tokio::process::Command::new(adapter);
    command
        .args([action, "--library"])
        .arg(root)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(25), command.output())
        .await
        .map_err(super::err)?
        .map_err(super::err)?;
    if !output.status.success() {
        return Err("Could not update or read MCP settings".into());
    }
    serde_json::from_slice(&output.stdout).map_err(super::err)
}

#[tauri::command]
pub async fn get_claude_mcp_status(
    window: WebviewWindow,
    repository: State<'_, RepositoryClient>,
) -> CmdResult<Option<ClaudeStatus>> {
    if window.label() != "main" {
        return Err("Claude MCP settings belong to the main window".into());
    }
    #[cfg(windows)]
    return invoke(repository.root(), "claude-status").await;
    #[cfg(not(windows))]
    {
        let _ = repository;
        Ok(None)
    }
}

#[tauri::command]
pub async fn set_claude_mcp_enabled(
    window: WebviewWindow,
    repository: State<'_, RepositoryClient>,
    client: ClaudeClient,
    enabled: bool,
) -> CmdResult<ClaudeStatus> {
    if window.label() != "main" {
        return Err("Claude MCP settings belong to the main window".into());
    }
    #[cfg(windows)]
    {
        let action = match (client, enabled) {
            (ClaudeClient::Code, true) => "enable-claude-code",
            (ClaudeClient::Code, false) => "disable-claude-code",
            (ClaudeClient::Desktop, true) => "enable-claude-desktop",
            (ClaudeClient::Desktop, false) => "disable-claude-desktop",
        };
        if let Some(status) = invoke(repository.root(), action).await? {
            return Ok(status);
        }
    }
    #[cfg(not(windows))]
    let _ = (repository, client, enabled);
    Err("Claude MCP setup is unavailable for this library".into())
}

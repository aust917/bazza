//! Windows installer and Settings integration for local MCP read grants.

use crate::{claude_config::Host, codex_config, projection::FixtureReads, protocol};
use anyhow::{bail, Context, Result};
use bazza_service_api::external_read::{IssueExternalReadGrant, RevokeExternalReadGrant};
use bazza_service_client::{
    broker, external_read_credentials,
    installed::{self, ClientInstall, Install},
    validate_managed_root, ServiceClient,
};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Copy)]
enum Client {
    Codex,
    Claude(Host),
}

const CLIENTS: [Client; 3] = [
    Client::Codex,
    Client::Claude(Host::Code),
    Client::Claude(Host::Desktop),
];

impl Client {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude(Host::Code) => "Claude Code",
            Self::Claude(Host::Desktop) => "Claude Desktop",
        }
    }

    fn entry(self, state: &Install) -> Option<ClientInstall> {
        match self {
            Self::Codex => Some(ClientInstall {
                enabled: state.enabled,
                connection_id: state.connection_id,
                config: state.config.clone(),
            }),
            Self::Claude(Host::Code) => state.claude_code.clone(),
            Self::Claude(Host::Desktop) => state.claude_desktop.clone(),
        }
    }

    fn save(self, root: &Path, state: &mut Install, entry: ClientInstall) -> Result<()> {
        match self {
            Self::Codex => {
                state.enabled = entry.enabled;
                state.connection_id = entry.connection_id;
            }
            Self::Claude(Host::Code) => state.claude_code = Some(entry),
            Self::Claude(Host::Desktop) => state.claude_desktop = Some(entry),
        }
        persist(root, state)
    }

    fn serve_command(self) -> &'static str {
        match self {
            Self::Codex => "serve-codex",
            Self::Claude(Host::Code) => "serve-claude-code",
            Self::Claude(Host::Desktop) => "serve-claude-desktop",
        }
    }

    fn launch_args(self, root: &Path) -> Result<[String; 3]> {
        Ok([
            self.serve_command().into(),
            "--library".into(),
            root.to_str().context("Invalid library path")?.into(),
        ])
    }

    fn check_available(self, root: &Path, state: &Install, entry: &ClientInstall) -> Result<()> {
        match self {
            Self::Codex => codex_config::check_available(state, root),
            Self::Claude(host) => {
                host.check_available(&entry.config, &state.adapter, &self.launch_args(root)?)
            }
        }
    }

    fn update_config(
        self,
        root: &Path,
        state: &Install,
        entry: &ClientInstall,
        enabled: bool,
    ) -> Result<()> {
        match self {
            Self::Codex => codex_config::update(state, root, enabled),
            Self::Claude(host) => host.update(
                &entry.config,
                &state.adapter,
                &self.launch_args(root)?,
                enabled,
            ),
        }
    }
}

pub fn handles(action: &OsString) -> bool {
    matches!(
        action.to_str(),
        Some(
            "serve-codex"
                | "install-codex"
                | "uninstall-codex"
                | "codex-status"
                | "enable-codex"
                | "disable-codex"
                | "install-mcp"
                | "uninstall-mcp"
                | "serve-claude-code"
                | "serve-claude-desktop"
                | "claude-status"
                | "enable-claude-code"
                | "disable-claude-code"
                | "enable-claude-desktop"
                | "disable-claude-desktop"
        )
    )
}

#[derive(Debug, Default, PartialEq, Eq)]
struct InstallChoices {
    codex: Option<bool>,
    claude: Option<bool>,
}

fn parse_options(
    action: &OsStr,
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(Option<PathBuf>, InstallChoices)> {
    let mut root = None;
    let mut choices = InstallChoices::default();
    let installing = action == "install-codex" || action == "install-mcp";
    while let Some(argument) = arguments.next() {
        if argument == "--library" && root.is_none() {
            root = Some(PathBuf::from(
                arguments.next().context("Missing --library path")?,
            ));
        } else if installing
            && (argument == "--disabled" || argument == "--enable-codex")
            && choices.codex.is_none()
        {
            choices.codex = Some(argument == "--enable-codex");
        } else if action == "install-mcp"
            && (argument == "--claude-disabled" || argument == "--enable-claude")
            && choices.claude.is_none()
        {
            choices.claude = Some(argument == "--enable-claude");
        } else {
            bail!("Unexpected argument for Bazza MCP setup");
        }
    }
    Ok((root, choices))
}

pub async fn run(action: OsString, arguments: impl Iterator<Item = OsString>) -> Result<()> {
    let (root, choices) = parse_options(&action, arguments)?;
    let installing = action == "install-codex" || action == "install-mcp";
    let uninstalling = action == "uninstall-codex" || action == "uninstall-mcp";
    let root = root
        .or_else(installed::default_library)
        .context("Bazza library path unavailable")?;
    if !root.is_absolute() {
        bail!("Bazza requires an absolute library path");
    }
    if let Some(client) = CLIENTS
        .into_iter()
        .find(|client| action == client.serve_command())
    {
        let (_, library_id) = validate_managed_root(&root)?;
        let state = Install::load(&root)?.filter(|state| state.installed);
        let id = state
            .and_then(|state| client.entry(&state))
            .filter(|entry| entry.enabled)
            .and_then(|entry| entry.connection_id)
            .context("Bazza access is disabled")?;
        let reads = FixtureReads::connect_managed_or_start(&root, id).await?;
        if reads.library_id() != library_id {
            bail!("Bazza library identity changed");
        }
        return protocol::serve_stdio(reads)
            .await
            .map_err(|_| anyhow::anyhow!("Bazza MCP stdio session failed"));
    }
    if installing {
        return install(&root, choices).await;
    }
    if action == "codex-status" || action == "claude-status" {
        let status = match Install::load(&root)?.filter(|state| state.installed) {
            Some(state) => Some(status(&state, action == "claude-status")?),
            None => None,
        };
        println!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    if Install::load(&root)?.is_none() {
        if uninstalling {
            return Ok(());
        }
        bail!("Bazza MCP setup has not been installed");
    }
    let (root, _) = validate_managed_root(&root)?;
    let _lock = lock(&root)?;
    let mut state = Install::load(&root)?.context("Bazza MCP setup has not been installed")?;
    if uninstalling {
        return uninstall(&root, &mut state).await;
    }
    if !state.installed {
        bail!("Bazza MCP setup has not been installed");
    }
    let (client, enabled) = match action.to_str() {
        Some("enable-codex") => (Client::Codex, true),
        Some("disable-codex") => (Client::Codex, false),
        Some("enable-claude-code") => (Client::Claude(Host::Code), true),
        Some("disable-claude-code") => (Client::Claude(Host::Code), false),
        Some("enable-claude-desktop") => (Client::Claude(Host::Desktop), true),
        Some("disable-claude-desktop") => (Client::Claude(Host::Desktop), false),
        _ => bail!("Unknown Bazza MCP setup action"),
    };
    if enabled {
        enable(&root, &mut state, client).await?;
    } else {
        disable(&root, &mut state, client).await?;
    }
    println!("{}", status(&state, matches!(client, Client::Claude(_)))?);
    Ok(())
}

fn lock(root: &Path) -> Result<File> {
    let path = root.join(".bazza-codex-install.lock");
    if let Ok(metadata) = path.symlink_metadata() {
        if !metadata.is_file() || metadata.is_symlink() {
            bail!("Bazza installation lock must be a regular file");
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()
        .context("Another Bazza integration update is in progress")?;
    Ok(file)
}

fn persist(root: &Path, state: &Install) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(root.join(installed::FILE))
        .map_err(|error| error.error)?;
    Ok(())
}

fn config_path() -> Result<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
        .filter(|path| path.is_absolute())
        .context("Codex settings directory is unavailable")?;
    Ok(home.join("config.toml"))
}

async fn prepare_library(service: &Path, root: &Path, action: &str) -> Result<()> {
    let mut command = tokio::process::Command::new(service);
    command
        .args([action, "--library"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    if !tokio::time::timeout(Duration::from_secs(120), command.status())
        .await??
        .success()
    {
        bail!("Bazza library setup failed ({action})");
    }
    Ok(())
}

async fn install(root: &Path, choices: InstallChoices) -> Result<()> {
    let adapter = std::env::current_exe()?;
    let service = adapter.with_file_name("bazza-service.exe");
    let new_library = !root.try_exists()?;
    if new_library {
        if installed::default_library().as_deref() == Some(root)
            && std::env::var_os("APPDATA").is_some_and(|path| {
                let legacy = PathBuf::from(path).join("Bazza");
                legacy.join("bazza.db").exists() || legacy.join("data-dir.txt").exists()
            })
        {
            bail!("The existing Bazza library remains on the legacy installation path");
        }
        prepare_library(&service, root, "init-library").await?;
    }
    let (root, library_id) = validate_managed_root(root)?;
    let _lock = lock(&root)?;
    let mut state = match Install::load(&root)? {
        Some(previous) => previous,
        None => Install {
            version: 1,
            library_id,
            installed: false,
            enabled: false,
            connection_id: None,
            adapter: adapter.clone(),
            config: config_path()?,
            claude_code: None,
            claude_desktop: None,
        },
    };
    if state.adapter != adapter {
        bail!("The Bazza integration is registered to another installation");
    }
    if !new_library && ServiceClient::connect_managed(&root).await.is_err() {
        // The migration command holds the repository's owner lock. Finish it
        // before refreshing the broker and allowing the new service to start.
        prepare_library(&service, &root, "upgrade-library").await?;
    }
    broker::register_managed(&service, &root).await?;
    state.installed = true;
    persist(&root, &state)?;
    let mut result = Ok(());
    for client in CLIENTS {
        let choice = match client {
            Client::Codex => choices.codex,
            Client::Claude(_) => choices.claude,
        };
        let current = match choice {
            Some(true) => enable(&root, &mut state, client).await,
            Some(false) => disable(&root, &mut state, client).await,
            None => continue,
        };
        result = result.and(current.with_context(|| format!("{} MCP setup failed", client.name())));
    }
    result
}

fn status(state: &Install, claude: bool) -> Result<serde_json::Value> {
    Ok(if claude {
        serde_json::json!({
            "code_enabled": effective_status(state, Client::Claude(Host::Code))?,
            "desktop_enabled": effective_status(state, Client::Claude(Host::Desktop))?,
        })
    } else {
        serde_json::json!({"enabled": effective_status(state, Client::Codex)?})
    })
}

fn effective_status(state: &Install, client: Client) -> Result<bool> {
    let Some(connection_id) = client
        .entry(state)
        .filter(|entry| state.installed && entry.enabled)
        .and_then(|entry| entry.connection_id)
    else {
        return Ok(false);
    };
    // A running Codex client can still have an active grant after its config
    // entry changes. Keep the Disable control available until access is revoked.
    Ok(external_read_credentials::load(state.library_id, connection_id)?.is_some())
}

fn new_entry(client: Client, state: &Install) -> Result<ClientInstall> {
    if let Some(entry) = client.entry(state) {
        return Ok(entry);
    }
    let Client::Claude(host) = client else {
        bail!("Codex configuration path is unavailable");
    };
    Ok(ClientInstall {
        enabled: false,
        connection_id: None,
        config: host.config_path()?,
    })
}

async fn enable(root: &Path, state: &mut Install, client: Client) -> Result<()> {
    let mut entry = new_entry(client, state)?;
    if let Client::Claude(host @ Host::Desktop) = client {
        if entry.connection_id.is_none() {
            entry.config = host.config_path()?;
        }
    }
    client.check_available(root, state, &entry)?;
    if let Some(id) = entry.connection_id {
        if entry.enabled && external_read_credentials::load(state.library_id, id)?.is_some() {
            client.update_config(root, state, &entry, true)?;
            return Ok(());
        }
        revoke(root, state, client, &mut entry).await?;
    }
    let mut service = ServiceClient::connect_or_start_managed(root).await?;
    let grant = service
        .issue_external_read_grant(IssueExternalReadGrant {
            client_label: format!("Bazza for {}", client.name()),
        })
        .await?;
    let id = grant.connection_id;
    entry.connection_id = Some(id);
    entry.enabled = true;
    let result = (|| {
        client.save(root, state, entry.clone())?;
        external_read_credentials::store(state.library_id, id, &grant.secret)?;
        client.update_config(root, state, &entry, true)?;
        Ok(())
    })();
    if result.is_err() {
        // Keep a failed pairing inaccessible. The durable ID lets a retry
        // finish cleanup even when a service or credential-store call fails.
        let _ = revoke(root, state, client, &mut entry).await;
    }
    result
}

async fn revoke(
    root: &Path,
    state: &mut Install,
    client: Client,
    entry: &mut ClientInstall,
) -> Result<()> {
    if let Some(connection_id) = entry.connection_id {
        external_read_credentials::remove(state.library_id, connection_id)?;
        let mut service = ServiceClient::connect_or_start_managed(root).await?;
        service
            .revoke_external_read_grant(RevokeExternalReadGrant { connection_id })
            .await?;
        entry.connection_id = None;
        client.save(root, state, entry.clone())?;
    }
    Ok(())
}

async fn disable(root: &Path, state: &mut Install, client: Client) -> Result<()> {
    let mut entry = new_entry(client, state)?;
    entry.enabled = false;
    client.save(root, state, entry.clone())?;
    revoke(root, state, client, &mut entry).await?;
    client.update_config(root, state, &entry, false)
}

async fn uninstall(root: &Path, state: &mut Install) -> Result<()> {
    state.installed = false;
    persist(root, state)?;
    let mut result = Ok(());
    for client in CLIENTS {
        if let Some(mut entry) = client.entry(state) {
            let current = async {
                revoke(root, state, client, &mut entry).await?;
                client.update_config(root, state, &entry, false)
            }
            .await;
            result = result.and(current);
        }
    }
    result?;
    // The desktop or a recording may own the same service. Wait for normal
    // idle exit rather than using Shutdown or Stop-ScheduledTask.
    let deadline = Instant::now() + Duration::from_secs(65);
    loop {
        if !root.join(broker::BROKER_FILE).try_exists()? {
            return Ok(());
        }
        match broker::unregister_managed(root).await {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error.into()),
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_defaults_to_preserving_access_and_requires_opt_in() {
        for action in ["install-mcp", "install-codex"] {
            let (_, choices) = parse_options(OsStr::new(action), std::iter::empty()).unwrap();
            assert_eq!(choices, InstallChoices::default());
        }
        let (root, choices) = parse_options(
            OsStr::new("install-mcp"),
            ["--library", r"C:\Bazza test", "--enable-claude"]
                .map(OsString::from)
                .into_iter(),
        )
        .unwrap();
        assert_eq!(root, Some(PathBuf::from(r"C:\Bazza test")));
        assert_eq!(
            choices,
            InstallChoices {
                codex: None,
                claude: Some(true),
            },
        );
        let (_, choices) = parse_options(
            OsStr::new("install-mcp"),
            ["--enable-codex", "--claude-disabled"]
                .map(OsString::from)
                .into_iter(),
        )
        .unwrap();
        assert_eq!(
            choices,
            InstallChoices {
                codex: Some(true),
                claude: Some(false),
            },
        );
    }

    #[test]
    fn installation_rejects_conflicting_or_misplaced_choices() {
        for args in [
            ["--enable-codex", "--disabled"],
            ["--claude-disabled", "--enable-claude"],
        ] {
            assert!(parse_options(
                OsStr::new("install-mcp"),
                args.map(OsString::from).into_iter(),
            )
            .is_err());
        }
        assert!(parse_options(
            OsStr::new("codex-status"),
            std::iter::once(OsString::from("--enable-codex")),
        )
        .is_err());
    }
}

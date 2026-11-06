use anyhow::{bail, Context, Result};
use bazza_service_client::installed::Install;
use std::{
    fs,
    io::{ErrorKind, Read, Write},
    path::Path,
};
use toml_edit::{value, Array, DocumentMut, Item, Table};

const SERVER: &str = "bazza";
const MAX_CONFIG: u64 = 4 * 1024 * 1024;

fn read(path: &Path) -> Result<Option<String>> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() || metadata.is_symlink() => {
            bail!("Codex settings must be a regular file")
        }
        Ok(_) => {}
    }
    let mut body = String::new();
    fs::File::open(path)?
        .take(MAX_CONFIG + 1)
        .read_to_string(&mut body)?;
    if body.len() as u64 > MAX_CONFIG {
        bail!("Codex settings are too large");
    }
    Ok(Some(body))
}

fn parse(body: Option<&str>) -> Result<DocumentMut> {
    // A parse error can include existing settings values. Keep it out of logs.
    body.unwrap_or_default()
        .parse()
        .map_err(|_| anyhow::anyhow!("Could not parse Codex settings"))
}

fn entry(document: &DocumentMut) -> Option<&Item> {
    document.get("mcp_servers")?.get(SERVER)
}

fn owns(entry: &Item, install: &Install, root: &Path) -> bool {
    let command = entry.get("command").and_then(Item::as_str);
    let arguments = entry.get("args").and_then(Item::as_array);
    command.is_some_and(|command| Path::new(command) == install.adapter)
        && arguments.is_some_and(|args| {
            args.len() == 3
                && args.get(0).and_then(|v| v.as_str()) == Some("serve-codex")
                && args.get(1).and_then(|v| v.as_str()) == Some("--library")
                && args.get(2).and_then(|v| v.as_str()).map(Path::new) == Some(root)
        })
}

#[cfg(test)]
fn status(install: &Install, root: &Path) -> Result<bool> {
    let body = read(&install.config)?;
    let document = parse(body.as_deref())?;
    Ok(entry(&document).is_some_and(|entry| {
        owns(entry, install, root) && entry.get("enabled").and_then(Item::as_bool) != Some(false)
    }))
}

pub fn check_available(install: &Install, root: &Path) -> Result<()> {
    let body = read(&install.config)?;
    let document = parse(body.as_deref())?;
    if entry(&document).is_some_and(|entry| !owns(entry, install, root)) {
        bail!("A different Bazza server is already configured in Codex");
    }
    Ok(())
}

pub fn update(install: &Install, root: &Path, enabled: bool) -> Result<()> {
    let path = &install.config;
    let before = read(path)?;
    let mut document = parse(before.as_deref())?;
    let current = entry(&document);
    if current.is_some_and(|entry| !owns(entry, install, root)) {
        if enabled {
            bail!("A different Bazza server is already configured in Codex");
        }
        return Ok(());
    }
    if enabled {
        if !document.contains_key("mcp_servers") {
            let mut servers = Table::new();
            servers.set_implicit(true);
            document.insert("mcp_servers", Item::Table(servers));
        }
        let servers = document["mcp_servers"]
            .as_table_mut()
            .context("Codex MCP settings must be a TOML table")?;
        if let Some(server) = servers.get_mut(SERVER) {
            if server.get("enabled").and_then(Item::as_bool) != Some(false) {
                return Ok(());
            }
            server["enabled"] = value(true);
        } else {
            let mut server = Table::new();
            server.insert(
                "command",
                value(install.adapter.to_str().context("Invalid adapter path")?),
            );
            let mut arguments = Array::new();
            arguments.push("serve-codex");
            arguments.push("--library");
            arguments.push(root.to_str().context("Invalid library path")?);
            server.insert("args", value(arguments));
            servers.insert(SERVER, Item::Table(server));
        }
    } else if current.is_some() {
        let servers = document["mcp_servers"]
            .as_table_mut()
            .context("Codex MCP settings must be a TOML table")?;
        servers.remove(SERVER);
    } else {
        return Ok(());
    }
    let parent = path.parent().context("Invalid Codex settings path")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(document.to_string().as_bytes())?;
    file.as_file().sync_all()?;
    if read(path)? != before {
        bail!("Codex settings changed while updating Bazza");
    }
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn round_trip_preserves_unrelated_mcp_settings_and_comments() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let install = Install {
            version: 1,
            library_id: Uuid::new_v4(),
            installed: true,
            enabled: true,
            connection_id: None,
            adapter: root.join("Bazza folder").join("bazza-mcp.exe"),
            config: root.join("config.toml"),
            claude_code: None,
            claude_desktop: None,
        };
        let original = "# Codex settings\nmodel = \"test\"\n\n[mcp_servers.example]\nargs = []\nenabled = true # preserve\n";
        fs::write(&install.config, original).unwrap();
        update(&install, root, true).unwrap();
        assert!(status(&install, root).unwrap());
        update(&install, root, false).unwrap();
        assert_eq!(fs::read_to_string(&install.config).unwrap(), original);
        let other = "[mcp_servers.bazza]\ncommand = \"custom\"\n";
        fs::write(&install.config, other).unwrap();
        assert!(update(&install, root, true).is_err());
        update(&install, root, false).unwrap();
        assert_eq!(fs::read_to_string(&install.config).unwrap(), other);
    }
}

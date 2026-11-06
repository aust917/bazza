use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

const SERVER: &str = "bazza";
const DESKTOP_FILE: &str = "claude_desktop_config.json";
const DESKTOP_FAMILIES: [&str; 2] = ["Claude_pzs8sxrjxfjjc", "AnthropicPBC.Claude_fnn82j28hfe8t"];
const MAX_CONFIG: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum Host {
    Code,
    Desktop,
}

impl Host {
    fn label(self) -> &'static str {
        match self {
            Self::Code => "Claude Code",
            Self::Desktop => "Claude Desktop",
        }
    }

    pub fn config_path(self) -> Result<PathBuf> {
        let path = match self {
            Self::Code => code_path(
                std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
                std::env::home_dir(),
            ),
            Self::Desktop => desktop_path(
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .as_deref(),
                std::env::var_os("APPDATA").map(PathBuf::from).as_deref(),
            ),
        };
        path.filter(|path| path.is_absolute())
            .with_context(|| format!("{} settings path is unavailable", self.label()))
    }

    pub fn check_available(self, path: &Path, adapter: &Path, args: &[String]) -> Result<()> {
        let before = self.read(path)?;
        let document = self.parse(before.as_deref())?;
        if document
            .get("mcpServers")
            .is_some_and(|servers| !servers.is_object())
        {
            bail!("{} MCP settings must be a JSON object", self.label());
        }
        if entry(&document).is_some_and(|entry| !owns(entry, adapter, args)) {
            self.conflict()?;
        }
        Ok(())
    }

    pub fn update(self, path: &Path, adapter: &Path, args: &[String], enabled: bool) -> Result<()> {
        let before = self.read(path)?;
        let mut document = self.parse(before.as_deref())?;
        if let Some(entry) = entry(&document) {
            if !owns(entry, adapter, args) {
                return if enabled { self.conflict() } else { Ok(()) };
            }
            if enabled {
                return Ok(());
            }
        } else if !enabled {
            return Ok(());
        }

        let servers = document
            .entry("mcpServers")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .with_context(|| format!("{} MCP settings must be a JSON object", self.label()))?;
        if enabled {
            let command = adapter.to_str().context("Invalid Bazza adapter path")?;
            let mut entry = json!({ "command": command, "args": args });
            if matches!(self, Self::Code) {
                entry["type"] = json!("stdio");
            }
            servers.insert(SERVER.into(), entry);
        } else {
            servers.remove(SERVER);
        }

        let parent = path.parent().context("Invalid Claude settings path")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, &document)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        if self.read(path)? != before {
            bail!("{} settings changed while updating Bazza", self.label());
        }
        let result = if before.is_some() {
            file.persist(path)
        } else {
            file.persist_noclobber(path)
        };
        result.map_err(|error| error.error)?;
        Ok(())
    }

    fn conflict(self) -> Result<()> {
        bail!(
            "A different Bazza server is already configured in {}",
            self.label()
        );
    }

    fn read(self, path: &Path) -> Result<Option<Vec<u8>>> {
        if !path.is_absolute() {
            bail!("{} settings path must be absolute", self.label());
        }
        match path.symlink_metadata() {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_file() || metadata.is_symlink() => {
                bail!("{} settings must be a regular file", self.label())
            }
            Ok(_) => {}
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_CONFIG + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CONFIG {
            bail!("{} settings are too large", self.label());
        }
        Ok(Some(bytes))
    }

    fn parse(self, bytes: Option<&[u8]>) -> Result<Map<String, Value>> {
        match bytes {
            None => Ok(Map::new()),
            Some(bytes) => {
                let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
                serde_json::from_slice(bytes)
                    .map_err(|_| anyhow::anyhow!("Could not parse {} settings", self.label()))
            }
        }
    }
}

fn code_path(config_dir: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    config_dir.or(home).map(|dir| dir.join(".claude.json"))
}

fn desktop_path(local: Option<&Path>, roaming: Option<&Path>) -> Option<PathBuf> {
    if let Some(local) = local.filter(|dir| dir.is_absolute()) {
        let locations = DESKTOP_FAMILIES.map(|family| {
            let packaged = local.join("Packages").join(family);
            let data = local.join("Claude-Data").join(family.to_ascii_lowercase());
            let legacy = packaged
                .join("LocalCache/Roaming/Claude")
                .join(DESKTOP_FILE);
            (packaged, data, legacy)
        });
        // Prefer existing configs to profile folders left by an older install.
        // Current MSIX uses Claude-Data. Older builds virtualize roaming AppData.
        if let Some(path) = locations
            .iter()
            .flat_map(|(_, data, legacy)| [data.join(DESKTOP_FILE), legacy.clone()])
            .chain([local.join("Claude-Data").join(DESKTOP_FILE)])
            .find(|path| path.is_file())
        {
            return Some(path);
        }
        if let Some((_, data, _)) = locations
            .iter()
            .find(|(_, data, _)| data.is_dir())
            .or_else(|| locations.iter().find(|(packaged, _, _)| packaged.is_dir()))
        {
            return Some(data.join(DESKTOP_FILE));
        }
    }
    roaming.map(|dir| dir.join("Claude").join(DESKTOP_FILE))
}

fn entry(document: &Map<String, Value>) -> Option<&Value> {
    document.get("mcpServers")?.get(SERVER)
}

fn owns(entry: &Value, adapter: &Path, args: &[String]) -> bool {
    entry
        .get("type")
        .is_none_or(|kind| kind.as_str() == Some("stdio"))
        && entry
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| Path::new(command) == adapter)
        && entry
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|current| {
                current.len() == args.len()
                    && current
                        .iter()
                        .zip(args)
                        .all(|(value, arg)| value.as_str() == Some(arg))
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owns_only_exact_entry_and_preserves_other_settings() {
        let fixture = tempfile::tempdir().unwrap();
        let adapter = fixture.path().join("Bazza files").join("bazza-mcp.exe");
        let args = vec!["serve-claude".into(), "Library path".into()];
        let original = json!({
            "mcpServers": { "example": { "command": "keep", "args": [] } },
            "preferences": { "name": "Example" }
        });
        for host in [Host::Code, Host::Desktop] {
            let path = fixture.path().join("settings.json");
            fs::write(&path, original.to_string()).unwrap();
            host.check_available(&path, &adapter, &args).unwrap();
            host.update(&path, &adapter, &args, true).unwrap();
            let added = host.parse(host.read(&path).unwrap().as_deref()).unwrap();
            let server = entry(&added).unwrap();
            assert!(owns(server, &adapter, &args));
            assert_eq!(server.get("type").is_some(), matches!(host, Host::Code));
            host.update(&path, &adapter, &args, true).unwrap();
            host.update(&path, &adapter, &["another".into()], false)
                .unwrap();
            assert_eq!(
                host.parse(host.read(&path).unwrap().as_deref()).unwrap(),
                added
            );
            assert!(host
                .update(&path, &adapter, &["another".into()], true)
                .is_err());
            host.update(&path, &adapter, &args, false).unwrap();
            let final_doc = host.parse(host.read(&path).unwrap().as_deref()).unwrap();
            assert_eq!(Value::Object(final_doc), original);

            let malformed = "{\"mcpServers\":42}";
            fs::write(&path, malformed).unwrap();
            assert!(host.check_available(&path, &adapter, &args).is_err());
            assert!(host.update(&path, &adapter, &args, true).is_err());
            host.update(&path, &adapter, &args, false).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), malformed);
        }
    }

    #[test]
    fn chooses_code_profile_and_desktop_packaging_locations() {
        let fixture = tempfile::tempdir().unwrap();
        let roaming = fixture.path().join("Roaming");
        let local = fixture.path().join("Local");
        let profile = fixture.path().join("Alternate Claude profile");
        assert_eq!(
            code_path(Some(profile.clone()), Some(fixture.path().into())),
            Some(profile.join(".claude.json"))
        );
        assert_eq!(
            desktop_path(Some(&local), Some(&roaming)),
            Some(roaming.join("Claude").join(DESKTOP_FILE))
        );
        let family = DESKTOP_FAMILIES[0];
        let legacy = local
            .join("Packages")
            .join(family)
            .join("LocalCache/Roaming/Claude");
        let packaged = local.join("Claude-Data").join(family.to_ascii_lowercase());
        fs::create_dir_all(&legacy).unwrap();
        assert_eq!(
            desktop_path(Some(&local), Some(&roaming)),
            Some(packaged.join(DESKTOP_FILE))
        );
        fs::write(legacy.join(DESKTOP_FILE), "{}").unwrap();
        assert_eq!(
            desktop_path(Some(&local), Some(&roaming)),
            Some(legacy.join(DESKTOP_FILE))
        );
        fs::create_dir_all(&packaged).unwrap();
        fs::write(packaged.join(DESKTOP_FILE), "{}").unwrap();
        assert_eq!(
            desktop_path(Some(&local), Some(&roaming)),
            Some(packaged.join(DESKTOP_FILE))
        );
    }
}

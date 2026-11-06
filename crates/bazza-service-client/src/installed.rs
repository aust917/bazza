//! Per-user installer state shared by the desktop and the MCP launcher.

use crate::{regular_file, validate_managed_root, ClientError};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const FILE: &str = ".bazza-codex-install.json";

pub fn default_library() -> Option<PathBuf> {
    std::env::home_dir()
        .filter(|home| home.is_absolute())
        .map(|home| home.join("Bazza Library"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub version: u16,
    pub library_id: Uuid,
    pub installed: bool,
    pub enabled: bool,
    pub connection_id: Option<Uuid>,
    pub adapter: PathBuf,
    pub config: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<ClientInstall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_desktop: Option<ClientInstall>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ClientInstall {
    pub enabled: bool,
    pub connection_id: Option<Uuid>,
    pub config: PathBuf,
}

impl Install {
    pub fn load(root: &Path) -> Result<Option<Self>, ClientError> {
        let path = root.join(FILE);
        match path.symlink_metadata() {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => regular_file(&path)?,
        }
        let (_, library_id) = validate_managed_root(root)?;
        let mut bytes = Vec::new();
        File::open(&path)?.take(8193).read_to_end(&mut bytes)?;
        if bytes.len() > 8192 {
            return Err(ClientError::Discovery);
        }
        let install: Self = serde_json::from_slice(&bytes).map_err(|_| ClientError::Discovery)?;
        if install.version != 1
            || install.library_id != library_id
            || !install.adapter.is_absolute()
            || !install.config.is_absolute()
            || install.connection_id.is_some_and(|id| id.is_nil())
            || [&install.claude_code, &install.claude_desktop]
                .into_iter()
                .flatten()
                .any(|client| {
                    !client.config.is_absolute()
                        || client.connection_id.is_some_and(|id| id.is_nil())
                })
        {
            return Err(ClientError::Discovery);
        }
        Ok(Some(install))
    }
}

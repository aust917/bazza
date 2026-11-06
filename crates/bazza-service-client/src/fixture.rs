//! Validation for explicit synthetic-only application entry points. It never
//! creates a library, opens a database or substitutes a default directory.
use crate::{regular_file, ClientError};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MARKER_FILE: &str = ".bazza-synthetic-fixture";
const MARKER: &[u8] = b"bazza-mcp-v8-fixture-v1\n";

/// Require an absolute existing directory and its exact, bounded regular-file
/// fixture marker before discovering or connecting to even an existing service.
pub fn validate_fixture_root(root: &Path) -> Result<PathBuf, ClientError> {
    let invalid = || ClientError::FixtureUnavailable;
    if !root.is_absolute() {
        return Err(invalid());
    }
    let root = root.canonicalize().map_err(|_| invalid())?;
    if !root.is_dir() {
        return Err(invalid());
    }
    let marker = root.join(MARKER_FILE);
    regular_file(&marker).map_err(|_| invalid())?;
    let mut value = Vec::new();
    File::open(marker)
        .map_err(|_| invalid())?
        .take(129)
        .read_to_end(&mut value)
        .map_err(|_| invalid())?;
    if value != MARKER {
        return Err(invalid());
    }
    Ok(root)
}

/// Read the closed managed-library identity without touching SQLite.
pub fn validate_managed_root(root: &Path) -> Result<(PathBuf, Uuid), ClientError> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Identity {
        version: u16,
        library_id: Uuid,
    }
    let invalid = || ClientError::ManagedLibraryUnavailable;
    if !root.is_absolute() {
        return Err(invalid());
    }
    let root = root.canonicalize().map_err(|_| invalid())?;
    if !root.is_dir() || !matches!(
        root.join(MARKER_FILE).symlink_metadata(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Err(invalid());
    }
    let path = root.join(".bazza-library-identity.json");
    regular_file(&path).map_err(|_| invalid())?;
    let mut body = Vec::new();
    File::open(path).map_err(|_| invalid())?.take(257)
        .read_to_end(&mut body).map_err(|_| invalid())?;
    if body.len() > 256 {
        return Err(invalid());
    }
    let identity: Identity = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if identity.version != 2 || identity.library_id.is_nil() {
        return Err(invalid());
    }
    Ok((root, identity.library_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_fixture_validation_requires_exact_bounded_regular_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let marker = root.join(MARKER_FILE);
        assert!(matches!(
            validate_fixture_root(root),
            Err(ClientError::FixtureUnavailable)
        ));
        for value in [
            b"wrong fixture\n".to_vec(),
            vec![b'x'; 129],
            [MARKER, b"extra"].concat(),
        ] {
            std::fs::write(&marker, value).unwrap();
            assert!(matches!(
                validate_fixture_root(root),
                Err(ClientError::FixtureUnavailable)
            ));
        }
        std::fs::remove_file(&marker).unwrap();
        std::fs::create_dir(&marker).unwrap();
        assert!(validate_fixture_root(root).is_err());
        std::fs::remove_dir(&marker).unwrap();
        std::fs::write(&marker, MARKER).unwrap();
        assert_eq!(
            validate_fixture_root(root).unwrap(),
            root.canonicalize().unwrap()
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
        assert!(!root.join("bazza.db").exists());
    }

    #[test]
    fn relative_and_missing_fixture_roots_are_rejected_without_creation() {
        assert!(matches!(
            validate_fixture_root(Path::new(".")),
            Err(ClientError::FixtureUnavailable)
        ));
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing");
        assert!(matches!(
            validate_fixture_root(&missing),
            Err(ClientError::FixtureUnavailable)
        ));
        assert!(!missing.exists());
        let file = temporary.path().join("regular-file");
        std::fs::write(&file, MARKER).unwrap();
        assert!(validate_fixture_root(&file).is_err());
    }
}

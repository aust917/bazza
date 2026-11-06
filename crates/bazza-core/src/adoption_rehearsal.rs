//! Exercise adoption on a database snapshot without enabling a service host.

use crate::{initialize_service_projections, load_or_create_identity, CoreError, FIXTURE_MARKER};
use bazza_store::{database_path, LibraryOwnerGuard, MigrationPolicy, Store, SUPPORTED_SCHEMA};
use serde::Serialize;
use std::{
    fs,
    io::ErrorKind,
    path::{Component, Path},
};

#[derive(Debug, Serialize)]
pub struct AdoptionRehearsal {
    pub source_schema: i64,
    pub candidate_schema: i64,
}

/// Back up a stopped legacy library into a new destination and migrate a copy.
/// The published snapshot contains a legacy rollback database and a separate
/// migrated candidate. This rehearsal never opens an endpoint or copies media.
pub fn rehearse_adoption(
    source: &Path,
    destination: &Path,
) -> Result<AdoptionRehearsal, CoreError> {
    let valid_path = |path: &Path| {
        path.is_absolute() && !path.components().any(|part| part == Component::ParentDir)
    };
    if !valid_path(source) || !valid_path(destination) {
        return Err(CoreError::InvalidRehearsalPath);
    }
    let destination_parent = destination
        .parent()
        .ok_or(CoreError::InvalidRehearsalPath)?
        .canonicalize()?;
    let destination = destination_parent.join(
        destination
            .file_name()
            .ok_or(CoreError::InvalidRehearsalPath)?,
    );
    let source = source.canonicalize()?;
    if inside(&destination, &source) || inside(&source, &destination) {
        return Err(CoreError::InvalidRehearsalPath);
    }
    reject_fixture(&source)?;
    let guard = LibraryOwnerGuard::acquire_existing(&source)?;
    reject_fixture(guard.root())?;
    // Reserve a new directory before writing. Never reuse an existing folder.
    fs::create_dir(&destination)?;
    let result = build_rehearsal(guard, &destination);
    if result.is_err() {
        // TempDir cleans its own generated staging tree. Remove only an empty
        // destination root, so an unexpected entry is never removed here.
        let _ = fs::remove_dir(&destination);
    }
    result
}

fn inside(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut parts = path.components();
        root.components().all(|parent| {
            parts
                .next()
                .is_some_and(|child| child.as_os_str().eq_ignore_ascii_case(parent.as_os_str()))
        })
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

fn reject_fixture(source: &Path) -> Result<(), CoreError> {
    match fs::symlink_metadata(source.join(FIXTURE_MARKER)) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(CoreError::InvalidRehearsalPath),
    }
}

fn build_rehearsal(
    guard: LibraryOwnerGuard,
    destination: &Path,
) -> Result<AdoptionRehearsal, CoreError> {
    let staging = tempfile::Builder::new()
        .prefix("adoption-")
        .tempdir_in(destination)?;
    let rollback = staging.path().join("rollback");
    let candidate = staging.path().join("candidate");
    fs::create_dir(&rollback)?;
    let source_schema = Store::snapshot_legacy_owned(&guard, &database_path(&rollback))?;
    drop(guard);
    fs::create_dir(&candidate)?;
    fs::copy(database_path(&rollback), database_path(&candidate))?;

    let guard = LibraryOwnerGuard::acquire_existing(&candidate)?;
    let store = Store::open_existing_owned(guard, MigrationPolicy::Upgrade)?;
    initialize_service_projections(&store)?;
    load_or_create_identity(store.root())?;
    store.checkpoint()?;
    drop(store);

    // Publish the pair together after the candidate has migrated and closed.
    // The destination root was created for this rehearsal and contains only
    // this private staging directory.
    fs::rename(staging.path(), destination.join("snapshot"))?;
    let _ = staging.keep();
    Ok(AdoptionRehearsal {
        source_schema,
        candidate_schema: SUPPORTED_SCHEMA,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_service_test_support::create_v8;
    use bazza_store::StoreError;

    #[test]
    fn rehearsal_requires_the_current_legacy_owner_to_stop() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let fixture = create_v8(&source).unwrap();
        fs::remove_file(source.join(FIXTURE_MARKER)).unwrap();
        let owner = Store::open_legacy(&fixture.database_path).unwrap();
        let destination = temporary.path().join("rehearsal");
        assert!(matches!(
            rehearse_adoption(&source, &destination),
            Err(CoreError::Store(StoreError::OwnerBusy))
        ));
        assert!(!destination.exists());

        drop(owner);
        let result = rehearse_adoption(&source, &destination).unwrap();
        assert_eq!(result.source_schema, 9);
        assert_eq!(result.candidate_schema, SUPPORTED_SCHEMA);
        assert!(!source.join(crate::IDENTITY_FILE).exists());
    }
}

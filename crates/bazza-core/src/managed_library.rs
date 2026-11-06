//! Fresh-only admission for a service-owned working library.
use super::{
    initialize_service_projections, persist_identity, read_identity_if_present, CoreError,
    Identity, MigrationPolicy, Repository, RepositoryMode, FIXTURE_MARKER, IDENTITY_FILE,
    MANAGED_IDENTITY_VERSION,
};
use bazza_store::{database_path, LibraryOwnerGuard, Store};
use std::{
    fs,
    io::ErrorKind,
    path::{Component, Path},
};

impl Repository {
    /// Reserve a new directory and initialize it without reading an old library
    /// or publishing a service endpoint. Failed initialization leaves evidence
    /// in the reserved directory for explicit inspection.
    pub fn create_managed(path: &Path) -> Result<Self, CoreError> {
        validate_absolute(path)?;
        let parent = path
            .parent()
            .ok_or(CoreError::InvalidManagedPath)?
            .canonicalize()?;
        for ancestor in parent.ancestors() {
            if entry_exists(&database_path(ancestor))?
                || entry_exists(&ancestor.join(IDENTITY_FILE))?
                || entry_exists(&ancestor.join(FIXTURE_MARKER))?
            {
                return Err(CoreError::InvalidManagedPath);
            }
        }
        let root = parent.join(path.file_name().ok_or(CoreError::InvalidManagedPath)?);
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Err(CoreError::InvalidManagedPath);
            }
            Err(error) => return Err(error.into()),
        }
        let guard = LibraryOwnerGuard::acquire_for_create(&root)?;
        // Presence of the identity blocks legacy before a database can exist.
        let identity = persist_identity(guard.root(), MANAGED_IDENTITY_VERSION)?;
        let store = Store::create_owned(guard)?;
        initialize_service_projections(&store)?;
        Ok(Self::from_owned_store(
            store,
            identity,
            RepositoryMode::Managed,
        ))
    }

    /// Open only an existing working library with its original identity.
    /// Reject mixed fixture/managed markers before opening SQLite.
    pub fn open_managed(path: &Path) -> Result<Self, CoreError> {
        Self::open_managed_with_policy(path, MigrationPolicy::RequireCurrent)
    }

    /// Explicit schema upgrade of an already managed library. The owner
    /// lock requires the service to be stopped. Legacy roots are never opened.
    pub fn upgrade_managed(path: &Path) -> Result<Self, CoreError> {
        Self::open_managed_with_policy(path, MigrationPolicy::Upgrade)
    }

    fn open_managed_with_policy(path: &Path, migration: MigrationPolicy) -> Result<Self, CoreError> {
        validate_absolute(path)?;
        let root = path.canonicalize()?;
        require_managed_identity(&root)?;
        let guard = LibraryOwnerGuard::acquire_existing(&root)?;
        let identity = require_managed_identity(guard.root())?;
        let store = Store::open_existing_owned(guard, migration)?;
        initialize_service_projections(&store)?;
        Ok(Self::from_owned_store(
            store,
            identity,
            RepositoryMode::Managed,
        ))
    }
}

fn validate_absolute(path: &Path) -> Result<(), CoreError> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(CoreError::InvalidManagedPath);
    }
    Ok(())
}

fn entry_exists(path: &Path) -> Result<bool, CoreError> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn require_managed_identity(root: &Path) -> Result<Identity, CoreError> {
    if entry_exists(&root.join(FIXTURE_MARKER))? {
        return Err(CoreError::ManagedLibraryRequired);
    }
    read_identity_if_present(root)?
        .filter(|identity| identity.version == MANAGED_IDENTITY_VERSION)
        .ok_or(CoreError::ManagedLibraryRequired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_store::StoreError;

    #[test]
    fn fresh_library_has_one_persistent_identity_and_excludes_legacy_writers() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("New library");
        let owner = Repository::create_managed(&root).unwrap();
        assert_eq!(owner.mode(), RepositoryMode::Managed);
        let initial = owner.status();
        assert!(!initial.development_fixture_only);
        assert_eq!(
            owner
                .store
                .service_desktop_meetings(&Default::default())
                .unwrap()
                .items
                .len(),
            0
        );
        assert!(!root.join(FIXTURE_MARKER).exists());
        assert!(!root.join(".bazza-service-endpoint.json").exists());
        assert!(matches!(
            Repository::open_managed(&root),
            Err(CoreError::Store(StoreError::OwnerBusy))
        ));
        drop(owner);
        assert!(matches!(
            Store::open_legacy(&database_path(&root)),
            Err(StoreError::ServiceRequired)
        ));
        let owner = Repository::open_managed(&root).unwrap();
        assert_eq!(owner.status().library_id, initial.library_id);
        assert_ne!(owner.status().library_epoch, initial.library_epoch);
    }

    #[test]
    fn refuses_existing_and_nested_legacy_paths_without_changing_the_database() {
        let base = tempfile::tempdir().unwrap();
        let legacy = base.path().join("legacy");
        let path = database_path(&legacy);
        drop(Store::open_legacy(&path).unwrap());
        let before = fs::read(&path).unwrap();
        let nested = legacy.join("nested");
        let empty = base.path().join("existing empty");
        fs::create_dir(&empty).unwrap();
        assert!(Repository::create_managed(Path::new("relative")).is_err());
        assert!(Repository::create_managed(&legacy).is_err());
        assert!(Repository::create_managed(&nested).is_err());
        assert!(Repository::create_managed(&empty).is_err());
        assert!(Repository::open_managed(&legacy).is_err());
        assert!(!nested.exists());
        assert!(!legacy.join(IDENTITY_FILE).exists());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(empty).unwrap().count(), 0);
    }

    #[test]
    fn managed_identity_with_a_synthetic_marker_admits_neither_mode() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("working");
        drop(Repository::create_managed(&root).unwrap());
        let db = database_path(&root);
        let before = fs::read(&db).unwrap();
        fs::write(root.join(FIXTURE_MARKER), super::super::FIXTURE_VALUE).unwrap();
        assert!(matches!(
            Repository::open_fixture(&root, MigrationPolicy::Upgrade),
            Err(CoreError::SyntheticFixtureRequired)
        ));
        assert!(matches!(
            Repository::open_managed(&root),
            Err(CoreError::ManagedLibraryRequired)
        ));
        assert_eq!(fs::read(db).unwrap(), before);
    }

    #[test]
    fn a_managed_open_never_upgrades_an_older_database() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("old schema with forged marker");
        let fixture = bazza_service_test_support::create_v8(&root).unwrap();
        fs::remove_file(root.join(FIXTURE_MARKER)).unwrap();
        persist_identity(&root, MANAGED_IDENTITY_VERSION).unwrap();
        let before = fs::read(&fixture.database_path).unwrap();
        assert!(matches!(
            Repository::open_managed(&root),
            Err(CoreError::Store(StoreError::MigrationRequired {
                found: 8,
                ..
            }))
        ));
        assert_eq!(fs::read(fixture.database_path).unwrap(), before);
    }
}

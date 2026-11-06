//! Current-build library ownership. Standard legacy and service constructors
//! share this guard; historical binaries that ignore it remain unsupported.

use crate::{database_path, Result, StoreError};
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

/// Persistent OS coordination file, never library data or a copyable live lock.
pub const SERVICE_OWNER_LOCK_FILE: &str = ".bazza-service-owner.lock";
/// Persistent service adoption marker. Any entry blocks current legacy opens;
/// its contents are validated only by the service-aware application boundary.
pub const SERVICE_IDENTITY_FILE: &str = ".bazza-library-identity.json";

/// Exclusive OS lock for one canonical library directory.
///
/// The guard cannot be cloned. Pass it by value into an owned Store constructor,
/// which retains it until after SQLite closes. The persistent lock file is never
/// unlinked: replacing it would let concurrent launchers lock different inodes.
pub struct LibraryOwnerGuard {
    root: PathBuf,
    _file: File,
}

impl LibraryOwnerGuard {
    /// Legacy normal-library admission. Directory creation remains compatible
    /// with Store::open; the standard database itself is reserved only after
    /// acquiring the same canonical lock as service constructors.
    pub(crate) fn acquire_legacy_root(root: &Path) -> Result<Self> {
        refuse_service_identity(root)?;
        std::fs::create_dir_all(root)?;
        let root = canonical_directory(root)?;
        refuse_service_identity(&root)?;
        match std::fs::symlink_metadata(database_path(&root)) {
            Ok(_) => validate_existing_database(&root)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let guard = Self::lock(root)?;
        // A service may have published its marker after our first check and
        // released its lock before ours succeeded. Never open SQLite in that gap.
        refuse_service_identity(guard.root())?;
        Ok(guard)
    }

    /// Acquire an existing library. Missing roots/databases create nothing.
    pub fn acquire_existing(root: &Path) -> Result<Self> {
        let root = canonical_directory(root)?;
        validate_existing_database(&root)?;
        Self::lock(root)
    }

    /// Prepare explicit creation in an already existing directory. An existing
    /// database, including an invalid file or a link, is never overwritten.
    pub fn acquire_for_create(root: &Path) -> Result<Self> {
        let root = canonical_directory(root)?;
        validate_database_absent(&root)?;
        Self::lock(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(root: PathBuf) -> Result<Self> {
        let path = root.join(SERVICE_OWNER_LOCK_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => validate_regular_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { root, _file: file }),
            Err(TryLockError::WouldBlock) => Err(StoreError::OwnerBusy),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

pub(crate) fn refuse_service_identity(root: &Path) -> Result<()> {
    match std::fs::symlink_metadata(root.join(SERVICE_IDENTITY_FILE)) {
        // Presence is the barrier: malformed bytes, directories and dangling
        // links must not be interpreted as an unmarked legacy library.
        Ok(_) => Err(StoreError::ServiceRequired),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn canonical_directory(root: &Path) -> Result<PathBuf> {
    let root = root.canonicalize().map_err(missing_as_library)?;
    if !root.is_dir() {
        return Err(StoreError::InvalidLibraryPath);
    }
    Ok(root)
}

pub(crate) fn validate_existing_database(root: &Path) -> Result<()> {
    validate_regular_file(&database_path(root))
}

pub(crate) fn validate_database_absent(root: &Path) -> Result<()> {
    match std::fs::symlink_metadata(database_path(root)) {
        Ok(_) => Err(StoreError::LibraryExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_regular_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(missing_as_library)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StoreError::InvalidLibraryPath);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(StoreError::InvalidLibraryPath);
        }
    }
    Ok(())
}

pub(crate) fn missing_as_library(error: std::io::Error) -> StoreError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StoreError::LibraryMissing
    } else {
        error.into()
    }
}

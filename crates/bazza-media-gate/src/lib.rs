//! Persistent, process-lifetime media gates, separate from recording contents.
//!
//! Readers share one meeting gate; a purge owner must acquire it exclusively
//! before touching that meeting's media. Gates are never truncated or unlinked.
//! This crate performs no authorization, lease expiry, media access or deletion.
//! The canonical library and gate directory must not be replaced by callers.
//!
//! A cloned/inherited read file retains the gate after its original guard drops.
//! No guard explicitly unlocks on Drop: the final handle/file-description close
//! releases ownership. This is not a proof that unrelated capture writers drained.

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io,
    path::{Path, PathBuf},
};

mod playback;
pub use playback::{BotTrackSource, PlaybackMediaKind};

pub const GATE_DIRECTORY: &str = ".bazza-service-media-gates";
pub const MAX_MEETING_ID_BYTES: usize = 128;
const KEY_DOMAIN: &[u8] = b"bazza-media-gate-v1\0";

#[derive(Debug, thiserror::Error)]
pub enum MediaGateError {
    #[error("media gate is busy")]
    Busy,
    #[error("invalid media gate: {0}")]
    Invalid(&'static str),
    #[error("media gate I/O failed: {0}")]
    Io(#[source] io::Error),
}
pub type Result<T> = std::result::Result<T, MediaGateError>;

/// A read-only lifetime pin. The contained file is intentionally not public:
/// callers may clone it for explicit child inheritance, but must not unlock it.
pub struct MediaReadGuard {
    file: File,
    library_root: PathBuf,
    meeting_id: String,
}

impl MediaReadGuard {
    pub fn acquire(library_root: &Path, meeting_id: &str) -> Result<Self> {
        let path = prepare_gate(library_root, meeting_id)?;
        Ok(Self {
            file: acquire_file(&path, false)?,
            library_root: path
                .ancestors()
                .nth(2)
                .expect("gate path has a library root")
                .to_path_buf(),
            meeting_id: meeting_id.to_owned(),
        })
    }

    /// The clone retains the exact read gate. Move it into a child's explicit
    /// stdio/handle list, then close every parent copy when no longer needed.
    /// Do not call File::unlock on this file or any clone of it.
    pub fn try_clone_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }
}

impl std::fmt::Debug for MediaReadGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaReadGuard").finish_non_exhaustive()
    }
}

/// Exclusive ownership for a future cooperating purge executor. Acquiring it
/// neither deletes media nor establishes that a capture has stopped writing.
pub struct MediaPurgeGuard {
    _file: File,
}

impl MediaPurgeGuard {
    pub fn acquire(library_root: &Path, meeting_id: &str) -> Result<Self> {
        let path = prepare_gate(library_root, meeting_id)?;
        Ok(Self {
            _file: acquire_file(&path, true)?,
        })
    }
}

impl std::fmt::Debug for MediaPurgeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaPurgeGuard").finish_non_exhaustive()
    }
}

fn gate_name(meeting_id: &str) -> Result<String> {
    if meeting_id.is_empty() || meeting_id.len() > MAX_MEETING_ID_BYTES || meeting_id.contains('\0')
    {
        return Err(MediaGateError::Invalid("meeting identifier"));
    }
    let mut hash = Sha256::new();
    hash.update(KEY_DOMAIN);
    hash.update(meeting_id.as_bytes());
    Ok(format!("{:x}.gate", hash.finalize()))
}

fn metadata_is_ordinary(metadata: &Metadata, directory: bool) -> Result<()> {
    let invalid = metadata.file_type().is_symlink()
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.len() != 0
        };
    #[cfg(windows)]
    let invalid = {
        use std::os::windows::fs::MetadataExt;
        invalid || metadata.file_attributes() & 0x400 != 0
    };
    if invalid {
        Err(MediaGateError::Invalid(
            "ordinary empty gate or directory required",
        ))
    } else {
        Ok(())
    }
}

fn ordinary_path(path: &Path, directory: bool) -> Result<()> {
    metadata_is_ordinary(&fs::symlink_metadata(path).map_err(io_error)?, directory)
}

fn prepare_gate(library_root: &Path, meeting_id: &str) -> Result<PathBuf> {
    let name = gate_name(meeting_id)?;
    if !library_root.is_absolute() {
        return Err(MediaGateError::Invalid(
            "absolute existing library root required",
        ));
    }
    ordinary_path(library_root, true)?;
    let root = library_root.canonicalize().map_err(io_error)?;
    ordinary_path(&root, true)?;
    let directory = root.join(GATE_DIRECTORY);
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error(error)),
    }
    ordinary_path(&directory, true)?;
    if directory.canonicalize().map_err(io_error)? != directory {
        return Err(MediaGateError::Invalid("gate directory identity"));
    }
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata_is_ordinary(&metadata, false)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // create_new never truncates an existing file. A concurrent creator
            // wins safely; the actual open below rechecks its ordinary format.
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            no_follow(&mut options);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(0);
            }
            match options.open(&path) {
                Ok(file) => metadata_is_ordinary(&file.metadata().map_err(io_error)?, false)?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        Err(error) => return Err(io_error(error)),
    }
    Ok(path)
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Nonblocking open also avoids waiting on a special file substituted
        // between the ordinary-entry check and open; handle metadata rejects it.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(any(windows, unix)))]
    let _ = options;
}

fn acquire_file(path: &Path, exclusive: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(exclusive);
    no_follow(&mut options);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        options.share_mode(if exclusive { 0 } else { FILE_SHARE_READ });
    }
    let file = options.open(path).map_err(io_error)?;
    metadata_is_ordinary(&file.metadata().map_err(io_error)?, false)?;
    #[cfg(unix)]
    {
        use std::fs::TryLockError;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match result {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(MediaGateError::Busy),
            Err(TryLockError::Error(error)) => return Err(io_error(error)),
        }
    }
    #[cfg(not(any(windows, unix)))]
    return Err(MediaGateError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "media gate platform is unsupported",
    )));

    // Check the named leaf again after obtaining ownership. On supported
    // platforms compare file identity too, rather than only its empty size.
    same_entry(&file, path)?;
    ordinary_path(
        path.parent()
            .ok_or(MediaGateError::Invalid("gate parent"))?,
        true,
    )?;
    Ok(file)
}

#[cfg(unix)]
fn same_entry(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata().map_err(io_error)?;
    let named = fs::symlink_metadata(path).map_err(io_error)?;
    metadata_is_ordinary(&named, false)?;
    if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
        return Err(MediaGateError::Invalid("gate file identity"));
    }
    Ok(())
}

#[cfg(windows)]
fn same_entry(file: &File, path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    // Metadata-only access is compatible with this process's exclusive purge
    // open. It never adds a second read/write owner of the gate.
    options
        .read(true)
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    no_follow(&mut options);
    let named = options.open(path).map_err(io_error)?;
    metadata_is_ordinary(&named.metadata().map_err(io_error)?, false)?;
    if file_identity(file).map_err(io_error)? != file_identity(&named).map_err(io_error)? {
        return Err(MediaGateError::Invalid("gate file identity"));
    }
    Ok(())
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<(u32, u32, u32)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // The file handle is live for this call; the successful API initializes
    // every field of its fixed-size output before it is read below.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let information = unsafe { information.assume_init() };
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

#[cfg(not(any(windows, unix)))]
fn same_entry(_file: &File, _path: &Path) -> Result<()> {
    Err(MediaGateError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "media gate platform is unsupported",
    )))
}

fn io_error(error: io::Error) -> MediaGateError {
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return MediaGateError::Busy;
    }
    MediaGateError::Io(error)
}

#[cfg(all(test, any(windows, unix)))]
mod tests;

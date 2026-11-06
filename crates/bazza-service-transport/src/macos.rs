use super::{endpoint_digest, identity_error, PeerIdentity, PeerStream};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

pub(crate) type Stream = UnixStream;

fn uid() -> u32 {
    // SAFETY: geteuid has no arguments, pointers or failure condition.
    unsafe { libc::geteuid() }
}

pub fn current_user_id() -> io::Result<String> {
    Ok(uid().to_string())
}

fn private_directory() -> PathBuf {
    PathBuf::from(format!("/tmp/bazza-service-{}", uid()))
}

fn ensure_private_directory() -> io::Result<()> {
    let path = private_directory();
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o777 != 0o700 {
        return Err(identity_error());
    }
    Ok(())
}

pub fn endpoint_name(canonical_library: &Path) -> io::Result<String> {
    let path = std::fs::canonicalize(canonical_library)?;
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "library is not a directory",
        ));
    }
    let digest = endpoint_digest(&current_user_id()?, path.as_os_str().as_bytes());
    Ok(socket_name(&digest))
}

pub(crate) fn external_endpoint_name(canonical_library: &Path) -> io::Result<String> {
    let private = endpoint_name(canonical_library)?;
    let mut hash = Sha256::new();
    hash.update(b"bazza-external-read-socket-v1\0");
    hash.update(private.as_bytes());
    Ok(socket_name(&format!("{:x}", hash.finalize())))
}

fn socket_name(digest: &str) -> String {
    // Keep well below Darwin sockaddr_un's 104-byte sun_path, even for long UIDs.
    private_directory()
        .join(format!("{}.sock", &digest[..48]))
        .to_string_lossy()
        .into_owned()
}

fn validate_endpoint(endpoint: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(endpoint);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(identity_error)?;
    let digest = name.strip_suffix(".sock").ok_or_else(identity_error)?;
    if path.parent() != Some(private_directory().as_path())
        || digest.len() != 48
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(identity_error());
    }
    Ok(path)
}

fn lock_endpoint(path: &Path) -> io::Result<File> {
    let lock_path = path.with_extension("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != uid() || metadata.mode() & 0o777 != 0o600 {
        return Err(identity_error());
    }
    // SAFETY: file owns this descriptor. The advisory lock is retained for the
    // Listener lifetime; another conforming listener cannot race stale cleanup.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "local endpoint is owned",
        ));
    }
    let named = std::fs::symlink_metadata(lock_path)?;
    if named.dev() != metadata.dev() || named.ino() != metadata.ino() {
        return Err(identity_error());
    }
    // Keep the file (and lock) until after the listener is closed and unlinked.
    // The lock file itself is never removed, avoiding an unlink/recreate race.
    Ok(file)
}

fn connection_refused(path: &Path) -> io::Result<bool> {
    // A nonblocking local probe must never wait for a full active listen backlog.
    // SAFETY: socket has no pointer arguments and returns an owned fd on success.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned this owned fd exactly once.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: fcntl applies flags to our live fd; no pointer parameters are used.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sockaddr_un is a plain C record for which all-zero is valid.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid local endpoint",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    address.sun_len = length as u8;
    // SAFETY: address includes a NUL-terminated path, its length is within the
    // initialized sockaddr_un, and fd is an owned AF_UNIX stream socket.
    let result = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if result == 0 {
        return Ok(false);
    }
    Ok(io::Error::last_os_error().raw_os_error() == Some(libc::ECONNREFUSED))
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !before.file_type().is_socket() || before.uid() != uid() {
        return Err(identity_error());
    }
    if !connection_refused(path)? {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "local endpoint is active",
        ));
    }
    let after = std::fs::symlink_metadata(path)?;
    if before.dev() != after.dev() || before.ino() != after.ino() || after.uid() != uid() {
        return Err(identity_error());
    }
    // The endpoint flock is held, the socket is owned and a nonblocking connect
    // was refused. An active conforming Listener cannot be unlinked here.
    std::fs::remove_file(path)
}

fn identity(stream: &UnixStream) -> io::Result<PeerIdentity> {
    let credentials = stream.peer_cred().map_err(|_| identity_error())?;
    let process_id = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(identity_error)?;
    if credentials.uid() != uid() {
        return Err(identity_error());
    }
    Ok(PeerIdentity {
        process_id,
        os_user: credentials.uid().to_string(),
    })
}

pub struct Listener {
    listener: Option<UnixListener>,
    path: PathBuf,
    socket_identity: (u64, u64),
    _lock: File,
}

impl Listener {
    /// Requires an entered Tokio runtime with I/O enabled.
    pub fn bind(endpoint: &str) -> io::Result<Self> {
        let path = validate_endpoint(endpoint)?;
        ensure_private_directory()?;
        let lock = lock_endpoint(&path)?;
        remove_stale_socket(&path)?;
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            listener: Some(UnixListener::from_std(listener)?),
            path,
            socket_identity: (metadata.dev(), metadata.ino()),
            _lock: lock,
        })
    }

    pub async fn accept(&mut self) -> io::Result<(PeerStream, PeerIdentity)> {
        let (stream, _) = self
            .listener
            .as_ref()
            .expect("listener exists until Drop")
            .accept()
            .await?;
        let peer = identity(&stream)?;
        Ok((PeerStream(stream), peer))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        drop(self.listener.take());
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_socket()
                && metadata.uid() == uid()
                && (metadata.dev(), metadata.ino()) == self.socket_identity
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
        // _lock is dropped only after this cleanup; never unlink the lock file.
    }
}

pub async fn connect(endpoint: &str, expected_server_pid: u32) -> io::Result<PeerStream> {
    let path = validate_endpoint(endpoint)?;
    if expected_server_pid == 0 {
        return Err(identity_error());
    }
    // Validate without creating a private directory on a read/connect path.
    let directory = std::fs::symlink_metadata(private_directory())?;
    if !directory.is_dir() || directory.uid() != uid() || directory.mode() & 0o777 != 0o700 {
        return Err(identity_error());
    }
    let socket = std::fs::symlink_metadata(&path)?;
    if !socket.file_type().is_socket() || socket.uid() != uid() || socket.mode() & 0o777 != 0o600 {
        return Err(identity_error());
    }
    let stream = UnixStream::connect(path).await?;
    if identity(&stream)?.process_id != expected_server_pid {
        return Err(identity_error());
    }
    Ok(PeerStream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_owned_socket_is_recovered_but_active_and_non_socket_paths_are_preserved() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        ensure_private_directory().unwrap();
        let root = tempfile::tempdir().unwrap();
        let endpoint = endpoint_name(root.path()).unwrap();
        let path = Path::new(&endpoint);
        let stale = std::os::unix::net::UnixListener::bind(path).unwrap();
        drop(stale);
        assert!(path.exists());
        let listener = Listener::bind(&endpoint).unwrap();
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        drop(listener);
        assert!(!path.exists());

        let active = std::os::unix::net::UnixListener::bind(path).unwrap();
        let before = std::fs::symlink_metadata(path).unwrap();
        assert_eq!(
            Listener::bind(&endpoint).err().unwrap().kind(),
            io::ErrorKind::AddrInUse
        );
        let after = std::fs::symlink_metadata(path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        drop(active);
        std::fs::remove_file(path).unwrap();

        std::fs::write(path, b"synthetic unrelated file").unwrap();
        assert_eq!(
            Listener::bind(&endpoint).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read(path).unwrap(), b"synthetic unrelated file");
        std::fs::remove_file(path).unwrap();
    }
}

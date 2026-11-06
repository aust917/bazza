//! Guard-bound, kernel-authenticated media-reader rendezvous. No media access,
//! repository authority, lease refresh or capture-owner metadata is involved.
use super::{HandoffError, Result};
use bazza_media_gate::{MediaGateError, MediaReadGuard, GATE_DIRECTORY};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const FRAME_LIMIT: usize = 4096;
const VERSION: u16 = 2;
const CALL_TIMEOUT: Duration = Duration::from_millis(300);
const PEER_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MediaReadIdentity {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub worker_session_id: Uuid,
    pub claim_id: Uuid,
    pub job_id: String,
    pub meeting_id: Uuid,
    pub capture_id: Uuid,
    pub capture_generation: Uuid,
    pub checkpoint_revision: String,
    pub lifecycle_revision: String,
    pub transcript_revision: String,
    pub source_id: Option<String>,
}
impl MediaReadIdentity {
    pub fn validate(&self) -> Result<()> {
        let id = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        };
        let revision = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        };
        if [
            self.library_id,
            self.library_epoch,
            self.worker_session_id,
            self.claim_id,
            self.meeting_id,
            self.capture_id,
            self.capture_generation,
        ]
        .iter()
        .any(Uuid::is_nil)
            || !id(&self.job_id)
            || !revision(&self.checkpoint_revision)
            || !revision(&self.lifecycle_revision)
            || !revision(&self.transcript_revision)
            || self.source_id.as_deref().is_some_and(|source| !id(source))
        {
            return Err(HandoffError::Invalid);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u16,
    identity: MediaReadIdentity,
    nonce: Uuid,
    endpoint: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u16,
    identity: MediaReadIdentity,
    nonce: Uuid,
    challenge: Uuid,
}

/// Owns a media read gate before publishing its unique rendezvous. Drop stops
/// and joins the proof server before metadata cleanup and parent gate release.
/// A retained cloned file may keep media pinned after this proof disappears.
pub struct MediaReadOwner {
    identity: MediaReadIdentity,
    nonce: Uuid,
    proof: Option<ProofServer>,
    metadata: Option<OwnedMetadata>,
    gate: MediaReadGuard,
}
impl std::fmt::Debug for MediaReadOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaReadOwner")
            .field("identity", &self.identity)
            .field("nonce", &self.nonce)
            .finish_non_exhaustive()
    }
}
impl MediaReadOwner {
    pub fn acquire(root: &Path, identity: &MediaReadIdentity) -> Result<Self> {
        identity.validate()?;
        let root = checked_root(root)?;
        let gate =
            MediaReadGuard::acquire(&root, &identity.meeting_id.to_string()).map_err(gate_error)?;
        let directory = coordination_directory(&root)?;
        let nonce = Uuid::new_v4();
        let endpoint = endpoint(&root, nonce)?;
        let metadata = OwnedMetadata::publish(
            metadata_path(&directory, nonce),
            &Metadata {
                version: VERSION,
                identity: identity.clone(),
                nonce,
                endpoint: endpoint.clone(),
            },
        )?;
        let mut owner = Self {
            identity: identity.clone(),
            nonce,
            proof: None,
            metadata: Some(metadata),
            gate,
        };
        owner.proof = Some(ProofServer::start(endpoint, identity.clone(), nonce)?);
        Ok(owner)
    }
    pub fn nonce(&self) -> Uuid {
        self.nonce
    }
    pub fn identity(&self) -> &MediaReadIdentity {
        &self.identity
    }
    pub fn try_clone_file(&self) -> io::Result<File> {
        self.gate.try_clone_file()
    }
}
impl Drop for MediaReadOwner {
    fn drop(&mut self) {
        drop(self.proof.take());
        drop(self.metadata.take());
        // gate is dropped only after this body and the joined proof server.
    }
}

/// Read-only verification. Metadata is an untrusted rendezvous, never authority.
/// The later repository caller also owns its own shared gate and rechecks DB
/// authority; this proof neither reads media nor refreshes a job/worker lease.
pub fn verify_owner(
    root: &Path,
    identity: &MediaReadIdentity,
    nonce: Uuid,
    expected_pid: u32,
) -> Result<()> {
    identity.validate()?;
    if nonce.is_nil() || expected_pid == 0 {
        return Err(HandoffError::Invalid);
    }
    let root = checked_root(root)?;
    let directory = coordination_directory(&root)?;
    let file = open_metadata(&metadata_path(&directory, nonce))?;
    let mut bytes = Vec::new();
    file.take(FRAME_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > FRAME_LIMIT {
        return Err(HandoffError::TooLarge);
    }
    let metadata: Metadata = serde_json::from_slice(&bytes).map_err(|_| HandoffError::Invalid)?;
    if metadata.version != VERSION || metadata.nonce != nonce || metadata.identity != *identity {
        return Err(HandoffError::IdentityMismatch);
    }
    let derived = endpoint(&root, nonce)?;
    if metadata.endpoint != derived {
        return Err(HandoffError::Invalid);
    }
    let identity = identity.clone();
    thread::Builder::new()
        .name("bazza-media-verifier".into())
        .spawn(move || {
            runtime()?.block_on(async {
                tokio::time::timeout(CALL_TIMEOUT, async {
                    let mut stream =
                        bazza_service_transport::connect(&derived, expected_pid).await?;
                    let request = Proof {
                        version: VERSION,
                        identity,
                        nonce,
                        challenge: Uuid::new_v4(),
                    };
                    write(&mut stream, &request).await?;
                    let reply: Proof = read(&mut stream).await?;
                    if reply.version != VERSION
                        || reply.identity != request.identity
                        || reply.nonce != nonce
                        || reply.challenge != request.challenge
                    {
                        return Err(invalid());
                    }
                    Ok(())
                })
                .await
                .map_err(|_| invalid())?
            })
        })?
        .join()
        .map_err(|_| HandoffError::Invalid)??;
    Ok(())
}

fn gate_error(error: MediaGateError) -> HandoffError {
    match error {
        MediaGateError::Busy => HandoffError::Busy,
        MediaGateError::Invalid(_) => HandoffError::Invalid,
        MediaGateError::Io(error) => HandoffError::Io(error),
    }
}
fn checked_root(root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() {
        return Err(HandoffError::UnsafePath);
    }
    if !super::regular_metadata(root)?.is_dir() {
        return Err(HandoffError::UnsafePath);
    }
    let root = root.canonicalize()?;
    if !super::regular_metadata(&root)?.is_dir() {
        return Err(HandoffError::UnsafePath);
    }
    Ok(root)
}
fn coordination_directory(root: &Path) -> Result<PathBuf> {
    let directory = root.join(GATE_DIRECTORY);
    if !super::regular_metadata(&directory)?.is_dir() || directory.canonicalize()? != directory {
        return Err(HandoffError::UnsafePath);
    }
    Ok(directory)
}
fn metadata_path(directory: &Path, nonce: Uuid) -> PathBuf {
    directory.join(format!("media-read-owner-{nonce}.json"))
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
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(any(windows, unix)))]
    let _ = options;
}
fn open_metadata(path: &Path) -> Result<File> {
    super::regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > FRAME_LIMIT as u64 {
        return Err(HandoffError::TooLarge);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(HandoffError::UnsafePath);
        }
    }
    Ok(file)
}
struct OwnedMetadata {
    path: PathBuf,
    file: File,
}
impl OwnedMetadata {
    fn publish(path: PathBuf, value: &Metadata) -> Result<Self> {
        let bytes = super::bounded_json(value, FRAME_LIMIT)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        no_follow(&mut options);
        let file = options.open(&path)?;
        let mut owned = Self { path, file };
        owned.file.write_all(&bytes)?;
        owned.file.flush()?;
        Ok(owned)
    }
}
impl Drop for OwnedMetadata {
    fn drop(&mut self) {
        // Unique nonce path, ordinary entry, and matching file identity only.
        // Never remove another owner's metadata or a persistent gate file.
        if let Ok(named) = open_metadata(&self.path) {
            if same_file(&self.file, &named).unwrap_or(false) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}
#[cfg(unix)]
fn same_file(first: &File, second: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let first = first.metadata()?;
    let second = second.metadata()?;
    Ok((first.dev(), first.ino()) == (second.dev(), second.ino()))
}
#[cfg(windows)]
fn same_file(first: &File, second: &File) -> io::Result<bool> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let identity = |file: &File| -> io::Result<(u32, u32, u32)> {
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: live file handle and writable fixed-size API output.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful GetFileInformationByHandle initializes all fields.
        let info = unsafe { info.assume_init() };
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    };
    Ok(identity(first)? == identity(second)?)
}
#[cfg(not(any(windows, unix)))]
fn same_file(_first: &File, _second: &File) -> io::Result<bool> {
    Ok(false)
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "media reader proof failed")
}
fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}
fn endpoint(root: &Path, nonce: Uuid) -> io::Result<String> {
    if nonce.is_nil() {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    hash.update(b"bazza-media-read-owner-proof-v2\0");
    hash.update(nonce.as_bytes());
    let hash = hash.finalize();
    let mut namespaced = [0; 16];
    namespaced.copy_from_slice(&hash[..16]);
    let base = bazza_service_transport::session_endpoint_name(root, &namespaced)?;
    #[cfg(target_os = "macos")]
    {
        let mut hash = Sha256::new();
        hash.update(b"bazza-media-read-owner-proof-v2\0");
        hash.update(base.as_bytes());
        hash.update(nonce.as_bytes());
        let digest = format!("{:x}", hash.finalize());
        return Ok(Path::new(&base)
            .parent()
            .ok_or_else(invalid)?
            .join(format!("{}.sock", &digest[..48]))
            .to_string_lossy()
            .into_owned());
    }
    #[cfg(not(target_os = "macos"))]
    Ok(base)
}
async fn read<T: DeserializeOwned>(
    stream: &mut bazza_service_transport::PeerStream,
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > FRAME_LIMIT {
        return Err(invalid());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
async fn write<T: Serialize>(
    stream: &mut bazza_service_transport::PeerStream,
    value: &T,
) -> io::Result<()> {
    let bytes = super::bounded_json(value, FRAME_LIMIT).map_err(|_| invalid())?;
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}
struct ProofServer {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl ProofServer {
    fn start(endpoint: String, identity: MediaReadIdentity, nonce: Uuid) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let thread = thread::Builder::new().name("bazza-media-read-owner".into()).spawn(move || {
            let runtime = match runtime() { Ok(runtime) => runtime, Err(error) => { let _ = ready_tx.send(Err(error)); return; } };
            runtime.block_on(async {
                let mut listener = match bazza_service_transport::Listener::bind(&endpoint) {
                    Ok(listener) => listener,
                    Err(error) => { let _ = ready_tx.send(Err(error)); return; }
                };
                if ready_tx.send(Ok(())).is_err() { return; }
                loop {
                    let accepted = tokio::select! { biased; _ = &mut stop_rx => break, value = listener.accept() => value };
                    let Ok((mut stream, _peer)) = accepted else {
                        tokio::select! { _ = &mut stop_rx => break, _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
                        continue;
                    };
                    let exchange = async {
                        let request: Proof = read(&mut stream).await?;
                        if request.version != VERSION || request.identity != identity || request.nonce != nonce || request.challenge.is_nil() { return Err(invalid()); }
                        write(&mut stream, &request).await
                    };
                    tokio::select! { biased; _ = &mut stop_rx => break, _ = tokio::time::timeout(PEER_TIMEOUT, exchange) => {} }
                }
            });
        })?;
        let server = Self {
            stop: Some(stop_tx),
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(server),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(HandoffError::Invalid),
        }
    }
}
impl Drop for ProofServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(all(test, any(windows, target_os = "macos")))]
#[path = "media_read_tests.rs"]
mod tests;

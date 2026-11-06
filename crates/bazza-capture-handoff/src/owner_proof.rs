//! A live guard proves its process identity over OS-authenticated local IPC.
//! Owner JSON is only an untrusted rendezvous. No server survives its OS lock.

use super::{CaptureIdentity, HandoffError, Result, TERMINAL_FILE};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{io, path::Path, thread, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const FRAME_LIMIT: usize = 2048;
const CALL_TIMEOUT: Duration = Duration::from_millis(300);
const PEER_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u16,
    identity: CaptureIdentity,
    nonce: Uuid,
    challenge: Uuid,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofReply {
    version: u16,
    identity: CaptureIdentity,
    nonce: Uuid,
    challenge: Uuid,
    // Server-observed state, never a request field or an owner-metadata claim.
    capture_unsealed: bool,
}

pub(super) struct ProofServer {
    pub(super) endpoint: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "capture owner proof failed",
    )
}

/// Derive the entire rendezvous from the checked canonical capture leaf and
/// fresh nonce. A metadata file cannot redirect verification to another root.
pub(super) fn endpoint(directory: &Path, nonce: Uuid) -> io::Result<String> {
    if nonce.is_nil() {
        return Err(invalid());
    }
    let base = bazza_service_transport::session_endpoint_name(directory, nonce.as_bytes())?;
    #[cfg(target_os = "macos")]
    {
        // The service transport deliberately keeps a stable macOS session
        // name. This proof needs a fresh one, in the same private UID directory
        // and accepted socket-name grammar, without creating capture artifacts.
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"bazza-capture-owner-proof-v1\0");
        hash.update(base.as_bytes());
        hash.update(nonce.as_bytes());
        let digest = format!("{:x}", hash.finalize());
        let parent = Path::new(&base).parent().ok_or_else(invalid)?;
        return Ok(parent
            .join(format!("{}.sock", &digest[..48]))
            .to_string_lossy()
            .into_owned());
    }
    #[cfg(not(target_os = "macos"))]
    Ok(base)
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
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
    // The closed proof contains only fixed-size UUIDs/integers/boolean; bound the
    // serialized result before writing any frame header.
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    if bytes.is_empty() || bytes.len() > FRAME_LIMIT {
        return Err(invalid());
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

impl ProofServer {
    pub(super) fn start(directory: &Path, identity: &CaptureIdentity, nonce: Uuid) -> Result<Self> {
        let endpoint = endpoint(directory, nonce)?;
        let server_endpoint = endpoint.clone();
        let directory = directory.to_path_buf();
        let identity = identity.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let worker = thread::Builder::new()
            .name("bazza-capture-owner".into())
            .spawn(move || {
                let runtime = match runtime() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let _: io::Result<()> = runtime.block_on(async {
                    let mut listener =
                        match bazza_service_transport::Listener::bind(&server_endpoint) {
                            Ok(listener) => listener,
                            Err(error) => {
                                let _ = ready_tx.send(Err(error));
                                return Ok(());
                            }
                        };
                    if ready_tx.send(Ok(())).is_err() {
                        return Ok(());
                    }
                    loop {
                        let accepted = tokio::select! {
                            biased;
                            _ = &mut stop_rx => break,
                            accepted = listener.accept() => accepted,
                        };
                        let Ok((mut stream, _peer)) = accepted else {
                            // An unauthenticated peer can fail accept; avoid a
                            // tight retry loop and retain cancellable shutdown.
                            tokio::select! {
                                _ = &mut stop_rx => break,
                                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                            }
                            continue;
                        };
                        // Exactly one peer at a time, no detached tasks, 2 KiB
                        // before allocation, and cancellation even mid-frame.
                        let exchange = async {
                            let request: Proof = read(&mut stream).await?;
                            if request.version != 1
                                || request.identity != identity
                                || request.nonce != nonce
                                || request.challenge.is_nil()
                            {
                                return Err(invalid());
                            }
                            // Existing files (including malformed manifests,
                            // directories and links) or uncertain I/O all deny
                            // begin. Control reattach remains separately valid.
                            let capture_unsealed=matches!(std::fs::symlink_metadata(directory.join(TERMINAL_FILE)),Err(error) if error.kind()==io::ErrorKind::NotFound);
                            let reply=ProofReply{version:request.version,identity:request.identity,nonce:request.nonce,challenge:request.challenge,capture_unsealed};
                            write(&mut stream, &reply).await
                        };
                        tokio::select! {
                            biased;
                            _ = &mut stop_rx => break,
                            _ = tokio::time::timeout(PEER_TIMEOUT, exchange) => {},
                        }
                    }
                    // listener and any connected stream are dropped on this
                    // thread before its join allows the artifact lock to drop.
                    Ok(())
                });
            })?;
        let server = Self {
            endpoint,
            stop: Some(stop_tx),
            thread: Some(worker),
        };
        match ready_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(server),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(invalid().into()),
        }
    }
}

impl Drop for ProofServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The current-thread runtime has no blocking jobs or spawned tasks.
        // Every wait above selects this stop signal, including partial frames;
        // join therefore cannot wait for a peer, its frame, or its deadline.
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn verify(
    endpoint: String,
    identity: CaptureIdentity,
    nonce: Uuid,
    expected_pid: u32,
    require_unsealed: bool,
) -> Result<()> {
    let challenge = Uuid::new_v4();
    // A separate bounded runtime also permits this synchronous API to be used
    // from tests or orchestration that already has an entered Tokio runtime.
    thread::Builder::new()
        .name("bazza-owner-verifier".into())
        .spawn(move || {
            runtime()?.block_on(async {
                tokio::time::timeout(CALL_TIMEOUT, async {
                    // The transport checks both current OS user and the actual
                    // kernel server PID, never the PID read from owner metadata.
                    let mut stream =
                        bazza_service_transport::connect(&endpoint, expected_pid).await?;
                    let request = Proof {
                        version: 1,
                        identity,
                        nonce,
                        challenge,
                    };
                    write(&mut stream, &request).await?;
                    let reply: ProofReply = read(&mut stream).await?;
                    if reply.version != 1
                        || reply.identity != request.identity
                        || reply.nonce != nonce
                        || reply.challenge != challenge
                        || (require_unsealed && !reply.capture_unsealed)
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

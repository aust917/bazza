//! Same-OS-user private IPC for application clients, without Store or Tauri.
//!
//! This authenticates the local process boundary, not an external MCP grant.
//! A public endpoint or discovery PID is untrusted until OS peer checks pass.
//! Callers enforce deadlines and discard a stream after a cancelled/failed frame.

use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "macos")))]
use unsupported as platform;

pub use platform::{connect, current_user_id, endpoint_name, Listener};

/// Discovery names the live service session. Windows pipe instances can outlive
/// their server while clients retain handles, so each epoch uses a fresh name.
/// macOS keeps its locked, stale-unlinked Unix socket; the stable endpoint name
/// also remains available for broker identity and transport-level tests.
pub fn session_endpoint_name(
    canonical_library: &std::path::Path,
    library_epoch: &[u8; 16],
) -> io::Result<String> {
    #[cfg(windows)]
    {
        platform::session_endpoint_name(canonical_library, library_epoch)
    }
    #[cfg(not(windows))]
    {
        let _ = library_epoch;
        endpoint_name(canonical_library)
    }
}

/// The external read role uses its own local listener. It is never reached
/// by choosing a method on the private application endpoint.
pub fn external_session_endpoint_name(
    canonical_library: &std::path::Path,
    library_epoch: &[u8; 16],
) -> io::Result<String> {
    #[cfg(windows)]
    {
        platform::external_session_endpoint_name(canonical_library, library_epoch)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = library_epoch;
        platform::external_endpoint_name(canonical_library)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = library_epoch;
        endpoint_name(canonical_library)
    }
}

pub const MAX_FRAME_BYTES: usize = 131_072;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub process_id: u32,
    pub os_user: String,
}

#[derive(Debug)]
pub struct PeerStream(platform::Stream);

impl AsyncRead for PeerStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for PeerStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

fn identity_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "local peer identity could not be verified",
    )
}

fn endpoint_digest(user: &str, path_bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"bazza-private-service-v1\0");
    hash.update(user.as_bytes());
    hash.update([0]);
    hash.update(path_bytes);
    format!("{:x}", hash.finalize())
}

/// A four-byte big-endian length followed by bounded UTF-8 JSON.
pub async fn read_frame<T: DeserializeOwned>(stream: &mut PeerStream) -> io::Result<T> {
    read_bounded(stream).await
}

pub async fn write_frame<T: Serialize>(stream: &mut PeerStream, value: &T) -> io::Result<()> {
    write_bounded(stream, value).await
}

async fn read_bounded<T: DeserializeOwned, R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<T> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    // The untrusted length is checked before allocating a body buffer.
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid JSON frame"))
}

struct BoundedBuffer {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl io::Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame exceeds transport limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn write_bounded<T: Serialize, W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &T,
) -> io::Result<()> {
    let mut buffer = BoundedBuffer {
        bytes: Vec::with_capacity(1024),
        exceeded: false,
    };
    if serde_json::to_writer(&mut buffer, value).is_err() {
        return Err(if buffer.exceeded {
            io::Error::new(io::ErrorKind::InvalidInput, "frame exceeds transport limit")
        } else {
            io::Error::new(io::ErrorKind::InvalidData, "frame serialization failed")
        });
    }
    writer.write_u32(buffer.bytes.len() as u32).await?;
    writer.write_all(&buffer.bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn frames_roundtrip_at_limit_and_reject_oversized_output_before_header() {
        runtime().block_on(async {
            let (mut writer, mut reader) = tokio::io::duplex(MAX_FRAME_BYTES + 8);
            let value = "x".repeat(MAX_FRAME_BYTES - 2); // JSON string quotes count too.
            write_bounded(&mut writer, &value).await.unwrap();
            assert_eq!(read_bounded::<String, _>(&mut reader).await.unwrap(), value);
            let error = write_bounded(&mut writer, &"x".repeat(MAX_FRAME_BYTES - 1))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), reader.read_u32())
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn untrusted_lengths_are_rejected_without_waiting_for_a_body() {
        runtime().block_on(async {
            for length in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
                let (mut writer, mut reader) = tokio::io::duplex(4);
                writer.write_u32(length).await.unwrap();
                let error = tokio::time::timeout(
                    Duration::from_secs(1),
                    read_bounded::<Value, _>(&mut reader),
                )
                .await
                .unwrap()
                .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                assert_eq!(error.to_string(), "invalid frame length");
            }
        });
    }

    #[test]
    fn bad_json_and_truncation_do_not_echo_payload() {
        runtime().block_on(async {
            let (mut writer, mut reader) = tokio::io::duplex(64);
            let private_payload = b"private synthetic payload";
            writer
                .write_u32(private_payload.len() as u32)
                .await
                .unwrap();
            writer.write_all(private_payload).await.unwrap();
            let error = read_bounded::<Value, _>(&mut reader).await.unwrap_err();
            assert_eq!(error.to_string(), "invalid JSON frame");
            writer.write_u32(20).await.unwrap();
            writer.write_all(b"{}").await.unwrap();
            drop(writer);
            assert_eq!(
                read_bounded::<Value, _>(&mut reader)
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::UnexpectedEof
            );
        });
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn endpoints_use_canonical_library_identity_without_creating_missing_paths() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            endpoint_name(root.path()).unwrap(),
            endpoint_name(&root.path().join(".")).unwrap()
        );
        let other = tempfile::tempdir().unwrap();
        assert_ne!(
            endpoint_name(root.path()).unwrap(),
            endpoint_name(other.path()).unwrap()
        );
        let missing = root.path().join("missing");
        assert!(endpoint_name(&missing).is_err());
        assert!(!missing.exists());
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn session_endpoints_bind_canonical_library_and_windows_epoch() {
        let root = tempfile::tempdir().unwrap();
        let epoch = [1; 16];
        let endpoint = session_endpoint_name(root.path(), &epoch).unwrap();
        assert_eq!(
            endpoint,
            session_endpoint_name(&root.path().join("."), &epoch).unwrap()
        );
        let other = tempfile::tempdir().unwrap();
        assert_ne!(
            endpoint,
            session_endpoint_name(other.path(), &epoch).unwrap()
        );
        #[cfg(windows)]
        {
            assert_ne!(endpoint, endpoint_name(root.path()).unwrap());
            assert_ne!(
                endpoint,
                session_endpoint_name(root.path(), &[2; 16]).unwrap()
            );
        }
        #[cfg(target_os = "macos")]
        assert_eq!(endpoint, endpoint_name(root.path()).unwrap());
        let missing = root.path().join("missing");
        assert!(session_endpoint_name(&missing, &epoch).is_err());
        assert!(!missing.exists());
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn external_endpoint_binds_library_and_has_an_independent_namespace() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let epoch = [1; 16];
        let endpoint = external_session_endpoint_name(root.path(), &epoch).unwrap();
        assert_ne!(
            endpoint,
            session_endpoint_name(root.path(), &epoch).unwrap()
        );
        assert_eq!(
            endpoint,
            external_session_endpoint_name(&root.path().join("."), &epoch).unwrap()
        );
        assert_ne!(
            endpoint,
            external_session_endpoint_name(other.path(), &epoch).unwrap()
        );
        let restarted = external_session_endpoint_name(root.path(), &[2; 16]).unwrap();
        #[cfg(windows)]
        assert_ne!(endpoint, restarted);
        #[cfg(target_os = "macos")]
        assert_eq!(endpoint, restarted);
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn actual_local_stream_rejects_an_oversized_header_before_any_body() {
        runtime().block_on(async {
            let root = tempfile::tempdir().unwrap();
            let endpoint = endpoint_name(root.path()).unwrap();
            let mut listener = Listener::bind(&endpoint).unwrap();
            let mut client = connect(&endpoint, std::process::id()).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            client.write_u32(MAX_FRAME_BYTES as u32 + 1).await.unwrap();
            let error =
                tokio::time::timeout(Duration::from_secs(1), read_frame::<Value>(&mut server))
                    .await
                    .unwrap()
                    .unwrap_err();
            assert_eq!(error.to_string(), "invalid frame length");
        });
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn local_peers_are_authenticated_and_listener_keeps_an_available_instance() {
        runtime().block_on(async {
            let root = tempfile::tempdir().unwrap();
            let endpoint = endpoint_name(root.path()).unwrap();
            let mut listener = Listener::bind(&endpoint).unwrap();
            assert!(Listener::bind(&endpoint).is_err());
            for number in 0..2 {
                let mut client = connect(&endpoint, std::process::id()).await.unwrap();
                let (mut server, peer) = listener.accept().await.unwrap();
                assert_eq!(peer.process_id, std::process::id());
                assert_eq!(peer.os_user, current_user_id().unwrap());
                write_frame(&mut client, &json!({"number":number}))
                    .await
                    .unwrap();
                assert_eq!(
                    read_frame::<Value>(&mut server).await.unwrap(),
                    json!({"number":number})
                );
                write_frame(&mut server, &json!({"accepted":true}))
                    .await
                    .unwrap();
                assert_eq!(
                    read_frame::<Value>(&mut client).await.unwrap(),
                    json!({"accepted":true})
                );
            }
            assert!(connect(&endpoint, 0).await.is_err());
            let error = connect(&endpoint, std::process::id().wrapping_add(1))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        });
    }
}

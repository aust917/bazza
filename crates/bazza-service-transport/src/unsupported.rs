use super::{PeerIdentity, PeerStream};
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "private service IPC requires Windows or macOS",
    )
}

#[derive(Debug)]
pub(crate) struct Stream;
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(unsupported()))
    }
}
impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(unsupported()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(unsupported()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(unsupported()))
    }
}
pub struct Listener;
impl Listener {
    pub fn bind(_: &str) -> io::Result<Self> {
        Err(unsupported())
    }
    pub async fn accept(&mut self) -> io::Result<(PeerStream, PeerIdentity)> {
        Err(unsupported())
    }
}
pub fn current_user_id() -> io::Result<String> {
    Err(unsupported())
}
pub fn endpoint_name(_: &Path) -> io::Result<String> {
    Err(unsupported())
}
pub async fn connect(_: &str, _: u32) -> io::Result<PeerStream> {
    Err(unsupported())
}

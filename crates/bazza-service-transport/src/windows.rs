use super::{endpoint_digest, identity_error, PeerIdentity, PeerStream};
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::pin::Pin;
use std::ptr;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{LocalFree, ERROR_PIPE_BUSY, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const PREFIX: &str = r"\\.\pipe\bazza-service-";

#[derive(Debug)]
pub(crate) enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Client(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Client(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_flush(cx),
            Self::Client(stream) => Pin::new(stream).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Client(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

// Convert* APIs allocate with LocalAlloc. Keep the allocation alive until the
// synchronous OS call/copy finishes, then release it exactly once with LocalFree.
struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: the pointer comes only from a successful Convert* allocation.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn process_user(process: HANDLE) -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: process is live for this call, and token points to writable storage.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(identity_error());
    }
    // SAFETY: OpenProcessToken returned an owned real handle, never a pseudo handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut length = 0;
    // SAFETY: querying required size with a null buffer is the documented API.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut length,
        );
    }
    if length < std::mem::size_of::<TOKEN_USER>() as u32 || length > 65_536 {
        return Err(identity_error());
    }
    // usize storage gives TOKEN_USER its required pointer alignment.
    let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: the allocation is aligned and covers length bytes; token remains owned.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(identity_error());
    }
    // SAFETY: successful TokenUser output initializes TOKEN_USER in this aligned buffer.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

fn sid_string(sid: windows_sys::Win32::Security::PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: callers keep the OS-supplied SID allocation alive for this call;
    // text receives a LocalAlloc buffer.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(identity_error());
    }
    let text_owner = LocalAllocation(text.cast());
    let mut len = 0;
    // SAFETY: Windows returns a NUL-terminated SID string. The bound also avoids
    // an unbounded scan if an unexpected OS result ever violates that contract.
    while len < 512 && unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    if len == 512 {
        return Err(identity_error());
    }
    // SAFETY: the NUL scan established this initialized UTF-16 range.
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) })
        .map_err(|_| identity_error())?;
    drop(text_owner);
    Ok(value)
}

pub fn current_user_id() -> io::Result<String> {
    // SAFETY: the current-process pseudo handle is borrowed, never closed.
    process_user(unsafe { GetCurrentProcess() })
}

fn peer_user(pid: u32) -> io::Result<String> {
    if pid == 0 {
        return Err(identity_error());
    }
    // SAFETY: no pointer parameters; ask only for limited process-query access.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(identity_error());
    }
    // SAFETY: OpenProcess returned an owned handle, kept alive through token lookup.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    process_user(process.as_raw_handle())
}

pub fn endpoint_name(canonical_library: &Path) -> io::Result<String> {
    let path = std::fs::canonicalize(canonical_library)?;
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "library is not a directory",
        ));
    }
    let bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    Ok(format!(
        "{PREFIX}{}",
        endpoint_digest(&current_user_id()?, &bytes)
    ))
}

pub fn session_endpoint_name(
    canonical_library: &Path,
    library_epoch: &[u8; 16],
) -> io::Result<String> {
    endpoint_for_session(
        canonical_library,
        library_epoch,
        b"bazza-private-service-session-v1\0",
    )
}

pub(crate) fn external_session_endpoint_name(
    canonical_library: &Path,
    library_epoch: &[u8; 16],
) -> io::Result<String> {
    endpoint_for_session(
        canonical_library,
        library_epoch,
        b"bazza-external-read-session-v1\0",
    )
}

fn endpoint_for_session(
    canonical_library: &Path,
    library_epoch: &[u8; 16],
    domain: &[u8],
) -> io::Result<String> {
    let stable = endpoint_name(canonical_library)?;
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(stable.as_bytes());
    hash.update([0]);
    hash.update(library_epoch);
    Ok(format!("{PREFIX}{:x}", hash.finalize()))
}

fn validate_endpoint(endpoint: &str) -> io::Result<()> {
    let suffix = endpoint.strip_prefix(PREFIX).ok_or_else(identity_error)?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(identity_error());
    }
    Ok(())
}

fn create_pipe(endpoint: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sid = current_user_id()?;
    // Protected DACL: only SYSTEM and this user, without inherited/world access.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})\0")
        .encode_utf16()
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: sddl is NUL-terminated, output is writable, optional size is null.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let descriptor = LocalAllocation(descriptor);
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: attrs and its self-relative descriptor remain alive through CreateNamedPipe.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .in_buffer_size(super::MAX_FRAME_BYTES as u32 + 4)
            .out_buffer_size(super::MAX_FRAME_BYTES as u32 + 4)
            .create_with_security_attributes_raw(
                endpoint,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

pub struct Listener {
    endpoint: String,
    pending: NamedPipeServer,
}

impl Listener {
    /// Requires an entered Tokio runtime with I/O enabled.
    pub fn bind(endpoint: &str) -> io::Result<Self> {
        validate_endpoint(endpoint)?;
        Ok(Self {
            endpoint: endpoint.into(),
            pending: create_pipe(endpoint, true)?,
        })
    }

    pub async fn accept(&mut self) -> io::Result<(PeerStream, PeerIdentity)> {
        self.pending.connect().await?;
        // Create the next listening instance before handing the connected one
        // away, so simultaneous adapters never see a gap in pipe ownership.
        let next = create_pipe(&self.endpoint, false)?;
        let connected = std::mem::replace(&mut self.pending, next);
        let mut pid = 0;
        // SAFETY: connected owns a live server-side named-pipe handle; pid is writable.
        if unsafe { GetNamedPipeClientProcessId(connected.as_raw_handle(), &mut pid) } == 0 {
            return Err(identity_error());
        }
        let os_user = peer_user(pid)?;
        if os_user != current_user_id()? {
            return Err(identity_error());
        }
        Ok((
            PeerStream(Stream::Server(connected)),
            PeerIdentity {
                process_id: pid,
                os_user,
            },
        ))
    }
}

pub async fn connect(endpoint: &str, expected_server_pid: u32) -> io::Result<PeerStream> {
    validate_endpoint(endpoint)?;
    if expected_server_pid == 0 {
        return Err(identity_error());
    }
    let client = loop {
        // Preserve Tokio's default identification-only QoS explicitly, preventing
        // a pipe server from impersonating this client's stronger privileges.
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(endpoint)
        {
            Ok(client) => break client,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    };
    let mut pid = 0;
    // SAFETY: client owns a live pipe handle; pid is writable.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) } == 0
        || pid != expected_server_pid
        || peer_user(pid)? != current_user_id()?
    {
        return Err(identity_error());
    }
    Ok(PeerStream(Stream::Client(client)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        GetAce, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION,
        SE_DACL_PROTECTED,
    };

    #[test]
    fn actual_pipe_dacl_is_protected_and_only_grants_system_and_current_user() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let root = tempfile::tempdir().unwrap();
        let endpoint = endpoint_name(root.path()).unwrap();
        let pipe = create_pipe(&endpoint, true).unwrap();
        let mut dacl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        // SAFETY: pipe owns the live kernel handle; outputs are writable and
        // unrequested owner/group/SACL outputs are null as the API permits.
        let status = unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0);
        let descriptor = LocalAllocation(descriptor);
        assert!(!dacl.is_null(), "a null DACL would grant everyone access");
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: descriptor remains owned; output pointers are valid.
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) },
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        let mut trustees = Vec::new();
        // SAFETY: GetSecurityInfo returned this ACL inside descriptor's lifetime.
        let count = unsafe { (*dacl).AceCount };
        assert_eq!(count, 2);
        for index in 0..count {
            let mut ace = ptr::null_mut();
            // SAFETY: index is within the returned ACL's AceCount.
            assert_ne!(unsafe { GetAce(dacl, u32::from(index), &mut ace) }, 0);
            // SAFETY: the OS descriptor was created with ACCESS_ALLOWED ACEs;
            // check the common header before interpreting SID/mask layout.
            let header = unsafe { &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
            assert_eq!(header.AceType, 0); // ACCESS_ALLOWED_ACE_TYPE.
                                           // No inherited entries.
            assert_eq!(header.AceFlags, 0);
            // SAFETY: validated ACCESS_ALLOWED_ACE type; SID is embedded in this ACE.
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = (&allowed.SidStart as *const u32).cast_mut().cast();
            trustees.push(sid_string(sid).unwrap());
        }
        trustees.sort();
        let mut expected = vec!["S-1-5-18".to_string(), current_user_id().unwrap()];
        expected.sort();
        assert_eq!(trustees, expected);
    }
}

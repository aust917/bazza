# Private service transport

This crate provides bounded JSON frames over authenticated local IPC. It has no
Store, Tauri, capture or inference dependency. It authenticates a process running
as the same OS user; it does not grant external MCP permissions or sandbox a
compromised process of that user. Public endpoint names and discovery PIDs are
untrusted until the OS checks succeed.

Windows uses named pipes derived from the current process-token SID, canonical
library path and service epoch. `session_endpoint_name` isolates a restarted
service from old client handles that can retain the previous kernel pipe instance.
Discovery readers/publishers recompute this name from the selected root and epoch.
Stable `endpoint_name` remains available for broker identity and transport tests;
the macOS session helper uses its existing stable, guarded Unix socket.
The first bind requires `FILE_FLAG_FIRST_PIPE_INSTANCE`; every pipe
has a protected DACL granting only SYSTEM and the current SID, and rejects remote
clients. Accept creates the next listening instance before returning a connection.
Both sides query the pipe peer PID and that process's token SID. Client additionally
checks the expected server PID, using Tokio's default SECURITY_IDENTIFICATION
quality of service. Process/token handles and LocalAlloc results have scoped owners.

macOS uses a uid-owned mode-0700 `/tmp/bazza-service-UID` directory and mode-0600
Unix socket, with uid and PID from Tokio `peer_cred`. Missing/unverifiable PID is
denied. A private mode-0600 `.lock` file is held with nonblocking `flock` for the
Listener lifetime. Stale socket removal requires ownership, a refused nonblocking
connect, and unchanged inode, while holding that lock; active endpoints are never
removed. Socket cleanup checks inode on Drop; lock files remain to prevent lock
unlink/recreate races. These files contain no credentials. Other OSes return
`Unsupported`; no TCP or SQLite fallback exists.

`Listener::bind` requires an entered Tokio runtime with I/O enabled. `accept` and
`connect` return `PeerStream`, which implements `AsyncRead`, `AsyncWrite`, and
`Unpin`. `read_frame`/`write_frame` use a four-byte big-endian length and at most
131072 bytes of JSON. Zero/oversized lengths are rejected before body allocation;
serialization is also bounded. JSON/error messages never echo payloads. The caller
must enforce deadlines and connection/queue limits and discard a stream after
frame failure or cancellation; retrying a half-read frame is unsupported.

Tests exercise bounds, malformed/truncated input, actual local peer identity,
server PID mismatch, endpoint canonicalization, listener continuity, singleton bind
and actual Windows DACL trustees/protection. Run
`cargo test -p bazza-service-transport` through the coordinated workspace build.
Windows and macOS acceptance must be reported separately; source implementation
does not assert a macOS runtime result.

Primary API references:
[Tokio ServerOptions](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ServerOptions.html),
[pipe instance lifetime](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createnamedpipea),
[Tokio client quality of service](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ClientOptions.html#method.security_qos_flags),
[client PID](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeclientprocessid),
[server PID](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeserverprocessid),
[security descriptors and LocalFree](https://learn.microsoft.com/en-us/windows/win32/api/sddl/nf-sddl-convertstringsecuritydescriptortosecuritydescriptorw),
[TokenUser](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation),
[Unix peer credentials](https://docs.rs/tokio/latest/tokio/net/struct.UnixStream.html#method.peer_cred).

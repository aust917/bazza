//! Keep an unattended screen recording from idling the display to black.
//!
//! A power request is scoped to the capture, not a change to the user's power
//! plan. It does not unlock Windows, switch desktops, or defeat manual sleep.

use std::io;

/// A transferable, capture-scoped display/system power request on Windows.
/// Other platforms keep their existing capture behavior.
pub struct CaptureEnvironmentGuard {
    #[cfg(windows)]
    _requests: RequestLease<native::PowerRequest>,
}

impl CaptureEnvironmentGuard {
    pub fn acquire() -> io::Result<Self> {
        check_desktop_available()?;
        #[cfg(windows)]
        {
            Ok(Self { _requests: RequestLease::acquire(native::PowerRequest::create()?)? })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }
}

/// A locked/disconnected/secure desktop cannot be made recordable by a codec.
/// This is also callable by a recording watchdog after capture has started.
pub fn check_desktop_available() -> io::Result<()> {
    #[cfg(windows)]
    native::check_desktop_available()?;
    Ok(())
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestKind { System, Display }

#[cfg(any(windows, test))]
trait PowerRequestApi {
    fn set(&self, kind: RequestKind) -> io::Result<()>;
    fn clear(&self, kind: RequestKind) -> io::Result<()>;
}

#[cfg(any(windows, test))]
struct RequestLease<T: PowerRequestApi> {
    request: T,
    system: bool,
    display: bool,
}

#[cfg(any(windows, test))]
impl<T: PowerRequestApi> RequestLease<T> {
    fn acquire(request: T) -> io::Result<Self> {
        let mut lease = Self { request, system: false, display: false };
        lease.request.set(RequestKind::System)?;
        lease.system = true;
        lease.request.set(RequestKind::Display)?;
        lease.display = true;
        Ok(lease)
    }
}

#[cfg(any(windows, test))]
impl<T: PowerRequestApi> Drop for RequestLease<T> {
    fn drop(&mut self) {
        for (active, kind) in [(self.display, RequestKind::Display), (self.system, RequestKind::System)] {
            if active {
                if let Err(error) = self.request.clear(kind) {
                    // The owned handle is still closed, including on clear failure.
                    tracing::warn!("could not clear screen-capture power request ({kind:?}): {error}");
                }
            }
        }
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
enum SessionState { ActiveUnlocked, Locked, Disconnected, Unknown }

#[cfg(any(windows, test))]
fn validate_desktop(state: SessionState, current: &[u16], input: &[u16]) -> io::Result<()> {
    let reason = match state {
        SessionState::Locked => Some("Windows is locked. Unlock the desktop and retry screen recording."),
        SessionState::Disconnected => Some("The Windows session is disconnected. Reconnect it and retry screen recording."),
        SessionState::Unknown => Some("Windows could not confirm an unlocked interactive session for screen recording."),
        SessionState::ActiveUnlocked if current.is_empty() || input.is_empty() || current != input =>
            Some("The input desktop is unavailable or differs from the recording desktop. Return to the normal desktop and retry screen recording."),
        SessionState::ActiveUnlocked => None,
    };
    match reason {
        Some(reason) => Err(io::Error::new(io::ErrorKind::PermissionDenied, reason)),
        None => Ok(()),
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Power::{
        PowerClearRequest, PowerCreateRequest, PowerSetRequest,
        PowerRequestDisplayRequired, PowerRequestSystemRequired,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSActive, WTSDisconnected, WTSFreeMemory, WTSINFOEXW,
        WTSQuerySessionInformationW, WTSSessionInfoEx, WTS_CURRENT_SESSION,
        WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK,
    };
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, GetThreadDesktop,
        GetUserObjectInformationW, HDESK, OpenInputDesktop, UOI_NAME,
    };
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
    };

    fn failure(action: &str, error: windows::core::Error) -> io::Error {
        io::Error::other(format!("{action}: {error}"))
    }

    pub(super) struct PowerRequest {
        handle: OwnedHandle,
        _reason: Vec<u16>,
    }

    impl PowerRequest {
        pub(super) fn create() -> io::Result<Self> {
            let mut reason: Vec<u16> = "Hark is recording the screen".encode_utf16().chain(Some(0)).collect();
            let context = REASON_CONTEXT {
                Version: 0, // The documented REASON_CONTEXT version.
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(reason.as_mut_ptr()) },
            };
            let handle = unsafe { PowerCreateRequest(&context) }
                .map_err(|e| failure("Could not keep the display awake for screen recording", e))?;
            // Own the kernel handle, so the lease can safely cross worker threads.
            let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
            Ok(Self { handle, _reason: reason })
        }
    }

    impl PowerRequestApi for PowerRequest {
        fn set(&self, kind: RequestKind) -> io::Result<()> {
            let kind = match kind { RequestKind::System => PowerRequestSystemRequired, RequestKind::Display => PowerRequestDisplayRequired };
            unsafe { PowerSetRequest(HANDLE(self.handle.as_raw_handle()), kind) }
                .map_err(|e| failure("Could not keep Windows/display awake during screen recording", e))
        }

        fn clear(&self, kind: RequestKind) -> io::Result<()> {
            let kind = match kind { RequestKind::System => PowerRequestSystemRequired, RequestKind::Display => PowerRequestDisplayRequired };
            unsafe { PowerClearRequest(HANDLE(self.handle.as_raw_handle()), kind) }
                .map_err(|e| failure("Could not release screen-recording power request", e))
        }
    }

    fn session_state() -> io::Result<SessionState> {
        let mut buffer = PWSTR::null();
        let mut bytes = 0;
        unsafe { WTSQuerySessionInformationW(None, WTS_CURRENT_SESSION, WTSSessionInfoEx, &mut buffer, &mut bytes) }
            .map_err(|e| failure("Could not check the Windows session before screen recording", e))?;
        let result = (|| {
            if buffer.is_null() || (bytes as usize) < std::mem::size_of::<WTSINFOEXW>() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows returned incomplete screen-recording session information"));
            }
            // WTS owns this allocation; copy the fixed-layout value before freeing it.
            let info = unsafe { std::ptr::read_unaligned(buffer.0.cast::<WTSINFOEXW>()) };
            if info.Level != 1 { return Ok(SessionState::Unknown) }
            let level = unsafe { info.Data.WTSInfoExLevel1 };
            if level.SessionState == WTSDisconnected { return Ok(SessionState::Disconnected) }
            if level.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK { return Ok(SessionState::Locked) }
            if level.SessionState == WTSActive && level.SessionFlags as u32 == WTS_SESSIONSTATE_UNLOCK {
                Ok(SessionState::ActiveUnlocked)
            } else {
                Ok(SessionState::Unknown)
            }
        })();
        unsafe { WTSFreeMemory(buffer.0.cast()) };
        result
    }

    struct InputDesktop(HDESK);
    impl Drop for InputDesktop {
        fn drop(&mut self) { let _ = unsafe { CloseDesktop(self.0) }; }
    }

    fn desktop_name(desktop: HDESK) -> io::Result<Vec<u16>> {
        let mut bytes = 0;
        let _ = unsafe { GetUserObjectInformationW(HANDLE(desktop.0), UOI_NAME, None, 0, Some(&mut bytes)) };
        if bytes == 0 || bytes % 2 != 0 || bytes > 65_536 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Could not read the Windows desktop identity for screen recording"));
        }
        let mut name = vec![0u16; bytes as usize / 2];
        unsafe { GetUserObjectInformationW(HANDLE(desktop.0), UOI_NAME, Some(name.as_mut_ptr().cast()), bytes, None) }
            .map_err(|e| failure("Could not inspect the Windows screen-recording desktop", e))?;
        name.truncate(name.iter().position(|&c| c == 0).unwrap_or(name.len()));
        Ok(name)
    }

    pub(super) fn check_desktop_available() -> io::Result<()> {
        let state = session_state()?;
        // Report lock/disconnection directly even if the secure desktop cannot be opened.
        if !matches!(state, SessionState::ActiveUnlocked) {
            return validate_desktop(state, &[], &[]);
        }
        let current = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
            .map_err(|e| failure("Could not access the recording thread's desktop", e))?;
        let input = InputDesktop(unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) }
            .map_err(|e| failure("Windows input desktop is not accessible for screen recording; return to the normal desktop and retry", e))?);
        validate_desktop(state, &desktop_name(current)?, &desktop_name(input.0)?)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::Foundation::GetHandleInformation;

        #[test]
        #[ignore = "requires an unlocked interactive Windows desktop; changes no power-plan settings"]
        fn native_request_is_transferable_and_released_on_drop() {
            let guard = CaptureEnvironmentGuard::acquire().unwrap();
            let handle = guard._requests.request.handle.as_raw_handle() as usize;
            let mut flags = 0;
            assert!(unsafe { GetHandleInformation(HANDLE(handle as _), &mut flags) }.is_ok());
            std::thread::spawn(move || drop(guard)).join().unwrap();
            assert!(unsafe { GetHandleInformation(HANDLE(handle as _), &mut flags) }.is_err());
            check_desktop_available().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockRequest { events: Arc<Mutex<Vec<String>>>, fail_display: bool }
    impl PowerRequestApi for MockRequest {
        fn set(&self, kind: RequestKind) -> io::Result<()> {
            self.events.lock().unwrap().push(format!("set {kind:?}"));
            if self.fail_display && kind == RequestKind::Display { return Err(io::Error::other("display request failed")) }
            Ok(())
        }
        fn clear(&self, kind: RequestKind) -> io::Result<()> {
            self.events.lock().unwrap().push(format!("clear {kind:?}"));
            Ok(())
        }
    }
    impl Drop for MockRequest {
        fn drop(&mut self) { self.events.lock().unwrap().push("close".into()); }
    }

    #[test]
    fn requests_are_cleared_before_the_handle_is_closed_on_another_thread() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lease = RequestLease::acquire(MockRequest { events: events.clone(), fail_display: false }).unwrap();
        std::thread::spawn(move || drop(lease)).join().unwrap();
        assert_eq!(*events.lock().unwrap(), ["set System", "set Display", "clear Display", "clear System", "close"]);
    }

    #[test]
    fn partial_activation_failure_releases_the_successful_request() {
        let events = Arc::new(Mutex::new(Vec::new()));
        assert!(RequestLease::acquire(MockRequest { events: events.clone(), fail_display: true }).is_err());
        assert_eq!(*events.lock().unwrap(), ["set System", "set Display", "clear System", "close"]);
    }

    #[test]
    fn unavailable_or_different_desktops_are_rejected_without_a_black_pixel_heuristic() {
        let desktop: Vec<_> = "Default".encode_utf16().collect();
        let secure: Vec<_> = "Winlogon".encode_utf16().collect();
        assert!(validate_desktop(SessionState::ActiveUnlocked, &desktop, &desktop).is_ok());
        for state in [SessionState::Locked, SessionState::Disconnected, SessionState::Unknown] {
            assert!(validate_desktop(state, &desktop, &desktop).is_err());
        }
        assert!(validate_desktop(SessionState::ActiveUnlocked, &desktop, &secure).is_err());
        assert!(validate_desktop(SessionState::ActiveUnlocked, &[], &[]).is_err());
    }
}

//! Windows raw-video child ownership. Assignment to the job happens inside
//! CreateProcessW, before the child's first instruction, rather than afterward.
//! This is process containment, not a drained-writer or artifact-recovery proof.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    DuplicateHandle, GetHandleInformation, SetHandleInformation, DUPLICATE_SAME_ACCESS,
    ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

const COMMAND_LINE_LIMIT: usize = 32_767;
const ATTRIBUTE_LIST_LIMIT: usize = 64 * 1024;
const STDERR_FINAL_LIMIT: usize = 64 * 1024;
const MAX_LIFETIME_FILES: usize = 16;

pub(crate) struct OwnedCaptureChild {
    // Never inherited or published. Parent death closes the last job handle.
    job: Option<OwnedHandle>,
    process: Arc<OwnedHandle>,
    pub(crate) stdin: Option<File>,
    pub(crate) stdout: Option<CapturedPipe>,
    pub(crate) stderr: Option<CapturedStderr>,
    status: Option<ExitStatus>,
}

impl OwnedCaptureChild {
    pub(crate) fn spawn(executable: &Path, args: &[String], cwd: &Path) -> io::Result<Self> {
        Self::spawn_checked(executable, args, cwd, |_, _| Ok(()))
    }

    fn spawn_checked(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        checkpoint: impl FnMut(SpawnPhase, Option<&OwnedHandle>) -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::spawn_with_files_checked(executable, args, cwd, &[], checkpoint)
    }

    pub(crate) fn spawn_with_lifetime_files(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        lifetime_files: &[&File],
    ) -> io::Result<Self> {
        Self::spawn_with_files_checked(executable, args, cwd, lifetime_files, |_, _| Ok(()))
    }

    pub(crate) fn spawn_piped_stdout(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        lifetime_files: &[&File],
    ) -> io::Result<Self> {
        Self::spawn_with_output_checked(executable, args, cwd, lifetime_files, true, |_, _| Ok(()))
    }

    fn spawn_with_files_checked(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        lifetime_files: &[&File],
        checkpoint: impl FnMut(SpawnPhase, Option<&OwnedHandle>) -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::spawn_with_output_checked(executable, args, cwd, lifetime_files, false, checkpoint)
    }

    fn spawn_with_output_checked(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        lifetime_files: &[&File],
        piped_stdout: bool,
        mut checkpoint: impl FnMut(SpawnPhase, Option<&OwnedHandle>) -> io::Result<()>,
    ) -> io::Result<Self> {
        validate_lifetime_files(lifetime_files)?;
        // Resolve before changing the child's cwd, including Command's bare-name
        // search order. There is no shell, environment override, or batch wrapper.
        let executable = resolve_executable(executable)?;
        let program = wide_z(executable.as_os_str())?;
        let directory = working_directory(cwd)?;
        let mut command_line = command_line(&executable, args)?;

        // SAFETY: None security attributes make a non-inheritable unnamed job;
        // each successful Win32 handle is immediately transferred to RAII.
        let job = owned(unsafe { CreateJobObjectW(None, None) }.map_err(win32_error)?);
        non_inheritable(&job)?;
        checkpoint(SpawnPhase::JobCreated, None)?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the pointer and exact structure size remain valid for the call.
        unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(win32_error)?;
        checkpoint(SpawnPhase::JobConfigured, None)?;

        let (input_read, input_write) = pipe()?;
        let (error_read, error_write) = pipe()?;
        inheritable(&input_read)?;
        inheritable(&error_write)?;
        let (output_read, output_write) = if piped_stdout {
            let (read, write) = pipe()?;
            inheritable(&write)?;
            (Some(read), write)
        } else {
            let security = windows::Win32::Security::SECURITY_ATTRIBUTES {
                nLength: size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
                bInheritHandle: true.into(),
                ..Default::default()
            };
            let null_name = wide_z(OsStr::new("NUL"))?;
            // SAFETY: NUL is a fixed device name; its sole child handle is allowlisted.
            let output_null = owned(
                unsafe {
                    CreateFileW(
                        PCWSTR(null_name.as_ptr()),
                        FILE_GENERIC_WRITE.0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        Some(&security),
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        None,
                    )
                }
                .map_err(win32_error)?,
            );
            (None, output_null)
        };
        let jobs = [raw(&job)];
        let handles = [raw(&input_read), raw(&output_write), raw(&error_write)];
        let attributes = AttributeList::new()?;
        attributes.update(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        attributes.update(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        // HANDLE_LIST protects this child's incoming handles. The short-lived
        // inheritable stream ends can still leak to a concurrent foreign spawn
        // that inherits everything; they carry no ownership authority. Neither
        // stream reader depends on every writer handle reaching EOF.
        checkpoint(SpawnPhase::AttributesReady, Some(&error_write))?;

        let startup = STARTUPINFOEXW {
            StartupInfo: windows::Win32::System::Threading::STARTUPINFOW {
                cb: size_of::<STARTUPINFOEXW>() as u32,
                dwFlags: STARTF_USESTDHANDLES,
                hStdInput: handles[0],
                hStdOutput: handles[1],
                hStdError: handles[2],
                ..Default::default()
            },
            lpAttributeList: attributes.list,
        };
        let mut information = PROCESS_INFORMATION::default();
        let mut creation_flags =
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
        if !lifetime_files.is_empty() {
            creation_flags |= CREATE_SUSPENDED;
        }
        // SAFETY: all UTF-16 strings are terminated and contain no internal NUL;
        // attribute backing arrays and inherited handles live through creation.
        // JOB_LIST assignment is part of creation; failure has no unbound retry.
        unsafe {
            CreateProcessW(
                PCWSTR(program.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                creation_flags,
                None,
                PCWSTR(directory.as_ptr()),
                &startup.StartupInfo,
                &mut information,
            )
        }
        .map_err(win32_error)?;
        let process = Arc::new(owned(information.hProcess));
        let thread = owned(information.hThread);
        let mut child = Self {
            job: Some(job),
            process: Arc::clone(&process),
            stdin: Some(input_write.into()),
            stdout: output_read
                .map(|pipe| CapturedPipe::new(pipe.into(), Arc::clone(&process), usize::MAX)),
            stderr: Some(CapturedPipe::new(
                error_read.into(),
                process,
                STDERR_FINAL_LIMIT,
            )),
            status: None,
        };
        if !lifetime_files.is_empty() {
            let setup = (|| {
                checkpoint(SpawnPhase::CreatedSuspended, Some(&child.process))?;
                for file in lifetime_files {
                    let mut remote = HANDLE::default();
                    // SAFETY: source is a borrowed live read-gate file, and the
                    // target is exactly our newly created suspended process.
                    // The result belongs ONLY to the child handle table. Never
                    // wrap it in a parent OwnedHandle or mark a parent pin
                    // inheritable: a concurrent inherit-all spawn must not pin it.
                    unsafe {
                        DuplicateHandle(
                            GetCurrentProcess(),
                            HANDLE(file.as_raw_handle()),
                            raw(&child.process),
                            &mut remote,
                            0,
                            false,
                            DUPLICATE_SAME_ACCESS,
                        )
                    }
                    .map_err(win32_error)?;
                    checkpoint(SpawnPhase::LifetimeFileDuplicated, Some(&child.process))?;
                }
                checkpoint(SpawnPhase::BeforeResume, Some(&child.process))?;
                // SAFETY: this owned thread is the initial thread returned by
                // CREATE_SUSPENDED. All lifetime refs are already in its process.
                match unsafe { ResumeThread(raw(&thread)) } {
                    1 => {}
                    u32::MAX => return Err(io::Error::last_os_error()),
                    _ => return Err(io::Error::other("media child suspend state changed")),
                }
                checkpoint(SpawnPhase::Resumed, Some(&child.process))
            })();
            if let Err(error) = setup {
                // Both before and after resume, terminate only this owned job,
                // then reap its exact root before returning an error. Closing
                // the job also covers a failed explicit terminate operation.
                child.terminate_and_wait(INFINITE)?;
                return Err(error);
            }
        }
        drop(thread);
        // The no-files capture path still has no fallible post-create step.
        // Readers share the exact root handle, never a looked-up reusable PID.
        Ok(child)
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_for(0)
    }

    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.wait_for(INFINITE)?
            .ok_or_else(|| io::Error::other("capture process wait did not complete"))
    }

    fn wait_for(&mut self, timeout_ms: u32) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        // SAFETY: the process handle is owned and valid for synchronization.
        match unsafe { WaitForSingleObject(raw(&self.process), timeout_ms) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: a signaled process has exited; 259 remains a valid
                // exit code and is not confused with a running process here.
                unsafe { GetExitCodeProcess(raw(&self.process), &mut code) }
                    .map_err(win32_error)?;
                let status = ExitStatus::from_raw(code);
                self.status = Some(status);
                // Descendants may not outlive the capture root and keep its
                // stderr pipe open. This requests termination, not a drain proof.
                drop(self.job.take());
                Ok(Some(status))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        if let Some(job) = &self.job {
            // SAFETY: this job contains only the process tree created above.
            unsafe { TerminateJobObject(raw(job), 1) }.map_err(win32_error)?;
        }
        Ok(())
    }

    pub(crate) fn terminate_and_wait(&mut self, timeout_ms: u32) -> io::Result<ExitStatus> {
        let termination = self.kill();
        // Also request termination by closing the last job handle if the
        // explicit call failed. No process is rediscovered or selected by PID.
        drop(self.job.take());
        drop(self.stdin.take());
        let status = self.wait_for(timeout_ms)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "owned media child termination is pending",
            )
        })?;
        termination?;
        Ok(status)
    }
}

pub(crate) type CapturedStderr = CapturedPipe;

pub(crate) struct CapturedPipe {
    // Sole non-inheritable read handle; available-byte reads cannot wait for a
    // competing reader. A leaked writer must not extend the root's lifetime.
    pipe: File,
    process: Arc<OwnedHandle>,
    final_remaining: Option<usize>,
    final_snapshot_limit: usize,
}

impl CapturedPipe {
    fn new(pipe: File, process: Arc<OwnedHandle>, final_snapshot_limit: usize) -> Self {
        Self {
            pipe,
            process,
            final_remaining: None,
            final_snapshot_limit,
        }
    }
}

impl Read for CapturedPipe {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.final_remaining == Some(0) {
            return Ok(0);
        }
        loop {
            // SAFETY: this is the same owned root process handle as the child.
            let exited = match unsafe { WaitForSingleObject(raw(&self.process), 0) } {
                WAIT_OBJECT_0 => true,
                WAIT_TIMEOUT => false,
                _ => return Err(io::Error::last_os_error()),
            };
            let mut available = 0;
            // SAFETY: this synchronous pipe has only this reader. Peek returns
            // immediately and the following read requests only bytes present.
            if let Err(error) = unsafe {
                PeekNamedPipe(
                    HANDLE(self.pipe.as_raw_handle()),
                    None,
                    0,
                    None,
                    Some(&mut available),
                    None,
                )
            } {
                let error = win32_error(error);
                if pipe_eof(&error) {
                    self.final_remaining = Some(0);
                    return Ok(0);
                }
                return Err(error);
            }
            if exited && self.final_remaining.is_none() {
                // Preserve the buffered final tail. Snapshot the remaining
                // byte count, so an outside writer cannot append indefinitely.
                // stdout must preserve the complete available snapshot, even
                // when it exceeds the diagnostic stderr tail limit.
                self.final_remaining = Some((available as usize).min(self.final_snapshot_limit));
            }
            let count = (available as usize)
                .min(output.len())
                .min(self.final_remaining.unwrap_or(usize::MAX));
            if count != 0 {
                let read = match self.pipe.read(&mut output[..count]) {
                    Ok(read) => read,
                    Err(error) if pipe_eof(&error) => {
                        self.final_remaining = Some(0);
                        return Ok(0);
                    }
                    Err(error) => return Err(error),
                };
                if let Some(remaining) = &mut self.final_remaining {
                    *remaining -= read;
                }
                return Ok(read);
            }
            if exited {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpawnPhase {
    JobCreated,
    JobConfigured,
    AttributesReady,
    CreatedSuspended,
    LifetimeFileDuplicated,
    BeforeResume,
    Resumed,
}

fn validate_lifetime_files(files: &[&File]) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    if files.len() > MAX_LIFETIME_FILES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many media lifetime files",
        ));
    }
    for file in files {
        let metadata = file.metadata()?;
        let mut flags = 0;
        // SAFETY: the caller borrows this live File for the entire spawn.
        unsafe { GetHandleInformation(HANDLE(file.as_raw_handle()), &mut flags) }
            .map_err(win32_error)?;
        if !metadata.is_file()
            || metadata.len() != 0
            || metadata.file_attributes() & 0x400 != 0
            || flags & HANDLE_FLAG_INHERIT.0 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ordinary non-inheritable empty media gate required",
            ));
        }
    }
    Ok(())
}

fn win32_error(error: windows::core::Error) -> io::Error {
    let hresult = error.code().0 as u32;
    // windows-result's io conversion preserves the signed HRESULT, whereas
    // std File errors use raw Win32 codes. Normalize only HRESULT_FROM_WIN32;
    // other facilities remain errors and cannot masquerade as pipe EOF.
    if hresult & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((hresult & 0xffff) as i32)
    } else {
        io::Error::other(error)
    }
}

fn pipe_eof(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(code)
        if code == ERROR_BROKEN_PIPE.0 as i32 || code == ERROR_PIPE_NOT_CONNECTED.0 as i32)
}

fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a fresh, valid, uniquely owned Win32 handle.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

fn inheritable(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the valid owned handle is one of the three explicit child streams.
    unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
        .map_err(win32_error)
}

fn non_inheritable(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: clear inheritance on our valid owned handle.
    unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(win32_error)
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: no security attributes means both handles start non-inheritable.
    unsafe { CreatePipe(&mut read, &mut write, None, 0) }.map_err(win32_error)?;
    Ok((owned(read), owned(write)))
}

struct AttributeList {
    memory: NonNull<u8>,
    layout: Layout,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttributeList {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: a null list queries the allocation size for exactly two attrs.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut bytes) };
        if bytes == 0 || bytes > ATTRIBUTE_LIST_LIMIT {
            return Err(io::Error::other("invalid capture process attribute size"));
        }
        let layout = Layout::from_size_align(bytes, 16)
            .map_err(|_| io::Error::other("invalid capture process attribute layout"))?;
        // SAFETY: layout has a nonzero bounded size and Windows heap alignment.
        let memory = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(memory.as_ptr().cast());
        // SAFETY: allocated, aligned backing storage has the requested size.
        if let Err(error) =
            unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes) }
        {
            // SAFETY: initialization failed, so only free the allocation.
            unsafe { dealloc(memory.as_ptr(), layout) };
            return Err(win32_error(error));
        }
        Ok(Self {
            memory,
            layout,
            list,
        })
    }

    fn update<const N: usize>(&self, key: u32, handles: &[HANDLE; N]) -> io::Result<()> {
        // SAFETY: each array remains live until after CreateProcessW and list
        // destruction; the pointer/size describe that exact handle array.
        unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                key as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(win32_error)
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: exactly one successful initialization owns this allocation.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
            dealloc(self.memory.as_ptr(), self.layout);
        }
    }
}

fn wide_z(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().take(COMMAND_LINE_LIMIT).collect();
    if value.len() >= COMMAND_LINE_LIMIT || value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid capture process string",
        ));
    }
    value.push(0);
    Ok(value)
}

fn working_directory(directory: &Path) -> io::Result<Vec<u16>> {
    let original = wide_z(directory.as_os_str())?;
    let unc: Vec<u16> = r"\\?\UNC".encode_utf16().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    let candidate = if let Some(tail) = original.strip_prefix(unc.as_slice()) {
        let mut candidate = vec![b'\\' as u16];
        candidate.extend_from_slice(tail);
        candidate
    } else if let Some(tail) = original.strip_prefix(verbatim.as_slice()) {
        tail.to_vec()
    } else {
        return Ok(original);
    };
    // Like std Command's make_dirp: strip only when GetFullPathNameW leaves
    // the resulting path unchanged. std::path::absolute performs that check
    // without normalizing the original verbatim path or inventing a new cwd.
    let unprefixed = OsString::from_wide(&candidate[..candidate.len() - 1]);
    if std::path::absolute(Path::new(&unprefixed)).is_ok_and(|absolute| {
        absolute
            .as_os_str()
            .encode_wide()
            .eq(candidate[..candidate.len() - 1].iter().copied())
    }) {
        Ok(candidate)
    } else {
        Ok(original)
    }
}

fn command_line(executable: &Path, args: &[String]) -> io::Result<Vec<u16>> {
    let mut result = Vec::new();
    append_quoted(&mut result, executable.as_os_str())?;
    for arg in args {
        append_units(&mut result, &[b' ' as u16])?;
        append_quoted(&mut result, OsStr::new(arg))?;
    }
    result.push(0);
    Ok(result)
}

fn append_units(result: &mut Vec<u16>, units: &[u16]) -> io::Result<()> {
    if units.len() >= COMMAND_LINE_LIMIT - result.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture command line is too long",
        ));
    }
    result.extend_from_slice(units);
    Ok(())
}

fn append_slashes(result: &mut Vec<u16>, count: usize) -> io::Result<()> {
    if count >= COMMAND_LINE_LIMIT - result.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture command line is too long",
        ));
    }
    result.resize(result.len() + count, b'\\' as u16);
    Ok(())
}

fn append_quoted(result: &mut Vec<u16>, value: &OsStr) -> io::Result<()> {
    let units = wide_z(value)?;
    append_units(result, &[b'"' as u16])?;
    let mut slashes = 0;
    for unit in &units[..units.len() - 1] {
        match *unit {
            92 => slashes += 1,
            34 => {
                append_slashes(result, 2 * slashes + 1)?;
                append_units(result, &[*unit])?;
                slashes = 0;
            }
            _ => {
                append_slashes(result, slashes)?;
                append_units(result, &[*unit])?;
                slashes = 0;
            }
        }
    }
    append_slashes(result, 2 * slashes)?;
    append_units(result, &[b'"' as u16])
}

fn resolve_executable(executable: &Path) -> io::Result<PathBuf> {
    wide_z(executable.as_os_str())?;
    if executable.as_os_str().is_empty() || executable.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture executable is missing",
        ));
    }
    if executable.file_name() != Some(executable.as_os_str()) {
        let with_suffix = executable.with_added_extension("exe");
        let selected = if executable
            .extension()
            .is_some_and(|ext| ext.as_encoded_bytes().eq_ignore_ascii_case(b"exe"))
            || !with_suffix.is_file()
        {
            executable
        } else {
            &with_suffix
        };
        return std::path::absolute(selected);
    }
    let filename = if executable.extension().is_none() {
        executable.with_extension("exe")
    } else {
        executable.to_path_buf()
    };
    let mut paths = Vec::new();
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            paths.push(parent.to_path_buf());
        }
    }
    for get_directory in [GetSystemDirectoryW, GetWindowsDirectoryW] {
        let mut buffer = vec![0; COMMAND_LINE_LIMIT];
        // SAFETY: the bounded UTF-16 buffer is writable for the duration.
        let count = unsafe { get_directory(Some(&mut buffer)) } as usize;
        if count != 0 && count < buffer.len() {
            paths.push(PathBuf::from(OsString::from_wide(&buffer[..count])));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()));
    }
    paths
        .into_iter()
        .map(|path| path.join(&filename))
        .find(|path| path.is_file())
        .map(std::path::absolute)
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "capture executable was not found"))
}

#[cfg(test)]
#[path = "owned_capture_child_tests.rs"]
mod tests;

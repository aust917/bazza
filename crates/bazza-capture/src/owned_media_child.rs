//! Windows media-reader process lifetime pins. No engine or pipeline activation.
//!
//! Nonempty read-gate lists are duplicated directly into the exact suspended
//! child, after atomic job assignment and before its first instruction. No pin
//! becomes inheritable in the parent. Each actual reader must use this spawn;
//! unknown grandchildren do not automatically retain these noninheritable pins.
//! `spawn` keeps stdout NUL, matching capture. `spawn_piped_stdout` exposes a
//! separate reader without a collector, JSON parser, output limit or deadline.
//! This is not a capture-writer drain proof or an output commit seam.
use crate::owned_capture_child::{CapturedPipe, CapturedStderr, OwnedCaptureChild};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::ExitStatus,
};

pub struct OwnedMediaChild {
    child: OwnedCaptureChild,
}
impl OwnedMediaChild {
    /// The at-most-16 files must be readonly SDK media-gate clones. Ordinary
    /// zero-byte format and noninheritance are checked; filesystem readonly
    /// attributes do not establish the handle's granted access. SAME_ACCESS is
    /// preserved, so callers must not supply writable or payload file handles.
    pub fn spawn(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        lifetime_files: &[&File],
    ) -> io::Result<Self> {
        Ok(Self {
            child: OwnedCaptureChild::spawn_with_lifetime_files(
                executable,
                args,
                cwd,
                lifetime_files,
            )?,
        })
    }
    /// Starts the same guarded process with piped stdout. The lifetime-file
    /// requirements are identical to `spawn`. Callers must drain stdout and
    /// stderr concurrently before waiting, and supply their own output budgets
    /// and deadline policy. Waiting before draining can block a full pipe.
    pub fn spawn_piped_stdout(
        executable: &Path,
        args: &[String],
        cwd: &Path,
        lifetime_files: &[&File],
    ) -> io::Result<Self> {
        Ok(Self {
            child: OwnedCaptureChild::spawn_piped_stdout(executable, args, cwd, lifetime_files)?,
        })
    }
    pub fn take_stdin(&mut self) -> Option<File> {
        self.child.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<OwnedMediaStdout> {
        self.child
            .stdout
            .take()
            .map(|inner| OwnedMediaStdout { inner })
    }
    pub fn take_stderr(&mut self) -> Option<OwnedMediaStderr> {
        self.child
            .stderr
            .take()
            .map(|inner| OwnedMediaStderr { inner })
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}
impl Drop for OwnedMediaChild {
    fn drop(&mut self) {
        // Bounded best-effort Drop. A kernel-delayed exit retains its own pins;
        // explicit kill/wait is available when the caller needs an exit result.
        let _ = self.child.terminate_and_wait(5_000);
    }
}

/// Bounded pipe reader with exact-root exit tracking, including final stderr.
pub struct OwnedMediaStderr {
    inner: CapturedStderr,
}
impl Read for OwnedMediaStderr {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.inner.read(output)
    }
}

/// Sole stdout reader, tracked against the exact root process handle. Once the
/// root exits, all bytes then available are drained before EOF; outside leaked
/// writers cannot extend that snapshot. The caller owns collection limits and
/// must not assume unknown grandchildren retain media pins or complete output.
pub struct OwnedMediaStdout {
    inner: CapturedPipe,
}
impl Read for OwnedMediaStdout {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.inner.read(output)
    }
}

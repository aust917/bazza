//! Persist a caller-completed file without weakening atomic replacement.
//! Windows replacement errors may be transient; neither retry nor failure
//! removes, truncates or opens the destination for writing.
use std::{io, path::Path};
use tempfile::NamedTempFile;

pub(crate) fn persist_atomic(file: NamedTempFile, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        persist_windows(file, target, |_| {})
    }
    #[cfg(not(windows))]
    {
        file.persist(target)
            .map(|_| ())
            .map_err(|error| error.error)
    }
}

#[cfg(windows)]
const RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(windows)]
fn persist_windows(
    mut file: NamedTempFile,
    target: &Path,
    mut observe_retry: impl FnMut(&io::Error),
) -> io::Result<()> {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + RETRY_BUDGET;
    loop {
        match file.persist(target) {
            Ok(_) => return Ok(()),
            Err(error) => {
                // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
                if !matches!(error.error.raw_os_error(), Some(5 | 32 | 33))
                    || Instant::now() >= deadline
                {
                    return Err(error.error);
                }
                observe_retry(&error.error);
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error.error);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
                if Instant::now() >= deadline {
                    return Err(error.error);
                }
                // PersistError returns the same completed file. Keep its path
                // and bytes intact for the next atomic same-directory replace.
                file = error.file;
            }
        }
    }
}

#[cfg(all(test, windows))]
#[path = "atomic_publish_tests.rs"]
mod tests;

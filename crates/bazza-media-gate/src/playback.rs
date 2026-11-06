//! Open only published media leaves under an already pinned meeting.
use super::{io_error, no_follow, ordinary_path, MediaGateError, MediaReadGuard, Result};
use std::{
    fs::{File, OpenOptions},
    io,
    path::PathBuf,
};

#[derive(Clone, Copy)]
pub enum PlaybackMediaKind {
    Audio,
    Video,
}

#[derive(Clone, Copy)]
pub enum BotTrackSource {
    Meet,
    Discord,
}

impl MediaReadGuard {
    /// Fixed local inputs for post-call recognition; never accepts an arbitrary path.
    pub fn open_source_audio(&self, microphone: bool) -> Result<File> {
        open_media_file(self.recording_directory()?.join(if microphone { "mic.wav" } else { "sys.wav" }))
    }

    pub fn open_bot_live_policy(&self) -> Result<File> {
        open_media_file(self.recording_directory()?.join("bot-live-policy.json"))
    }

    pub fn open_audio_clock(&self) -> Result<File> {
        open_media_file(self.recording_directory()?.join("mix.clock.json"))
    }

    pub fn open_playback_file(&self, kind: PlaybackMediaKind) -> Result<File> {
        let path = self.recording_directory()?.join(match kind {
            PlaybackMediaKind::Audio => "mix.wav",
            PlaybackMediaKind::Video => "screen.mp4",
        });
        open_media_file(path)
    }

    pub fn bot_tracks_directory(&self, source: BotTrackSource) -> Result<PathBuf> {
        let directory = self.recording_directory()?.join(match source {
            BotTrackSource::Meet => "attendee",
            BotTrackSource::Discord => "discord",
        });
        ordinary_directory(&directory)?;
        Ok(directory)
    }

    pub fn open_bot_track_file(&self, source: BotTrackSource, name: &str) -> Result<File> {
        let valid_wave = name.strip_suffix(".wav").is_some_and(|stem| {
            let number = match source {
                BotTrackSource::Meet => stem.strip_prefix("participant-").unwrap_or(""),
                BotTrackSource::Discord => stem,
            };
            !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
        });
        if !matches!(name, "tracks.json" | "participant-transcript.json") && !valid_wave {
            return Err(MediaGateError::Invalid("bot track file name"));
        }
        open_media_file(self.bot_tracks_directory(source)?.join(name))
    }

    fn recording_directory(&self) -> Result<PathBuf> {
        if !self
            .meeting_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(MediaGateError::Invalid("playback meeting directory"));
        }
        let recordings = self.library_root.join("recordings");
        let directory = recordings.join(&self.meeting_id);
        ordinary_directory(&recordings)?;
        ordinary_directory(&directory)?;
        Ok(directory)
    }
}

fn ordinary_directory(path: &std::path::Path) -> Result<()> {
    ordinary_path(path, true)?;
    if path.canonicalize().map_err(io_error)? != path {
        return Err(MediaGateError::Invalid("media directory identity"));
    }
    Ok(())
}

fn open_media_file(path: PathBuf) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = options.open(&path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file()
        || opened_path(&file).map_err(io_error)? != path
        || !has_single_link(&file).map_err(io_error)?
    {
        return Err(MediaGateError::Invalid("media file identity"));
    }
    Ok(file)
}

#[cfg(windows)]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::{
        ffi::OsString,
        os::windows::{ffi::OsStringExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut output = vec![0_u16; 32_768];
    // A live file handle and a writable buffer for the stated number of UTF-16 units.
    let len = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            output.as_mut_ptr(),
            output.len() as u32,
            0,
        )
    } as usize;
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    if len >= output.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "playback file path length",
        ));
    }
    Ok(PathBuf::from(OsString::from_wide(&output[..len])))
}

#[cfg(windows)]
fn has_single_link(file: &File) -> io::Result<bool> {
    use std::{
        mem::MaybeUninit,
        os::windows::{fs::MetadataExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    if file.metadata()?.file_attributes() & 0x400 != 0 {
        return Ok(false);
    }
    let mut output = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // The API initializes the complete fixed-size output on success.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), output.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { output.assume_init() }.nNumberOfLinks == 1)
}

#[cfg(target_os = "macos")]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::{
        ffi::OsStr,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    let mut output = [0_u8; libc::PATH_MAX as usize];
    // F_GETPATH writes a NUL-terminated path to a PATH_MAX sized buffer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, output.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let end = output
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(io::ErrorKind::InvalidData)?;
    Ok(PathBuf::from(OsStr::from_bytes(&output[..end])))
}

#[cfg(target_os = "linux")]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn opened_path(_file: &File) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn has_single_link(file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink() == 1)
}

#[cfg(not(any(unix, windows)))]
fn has_single_link(_file: &File) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

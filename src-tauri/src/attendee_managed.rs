//! Starts the desktop's private Attendee runtime on demand.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tauri::AppHandle;

use crate::{attendee_settings::AttendeeSettings, commands::CmdResult, ui_message::UiMessage};

static IN_USE: AtomicBool = AtomicBool::new(false);

pub(crate) struct Lease;

impl Lease {
    fn acquire() -> CmdResult<Self> {
        IN_USE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| {
                UiMessage::app(
                    "native.attendeeSetupBusy",
                    "Google Meet setup or recording is already in progress.",
                )
            })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        IN_USE.store(false, Ordering::Release);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Provisioned {
    #[serde(flatten)]
    pub settings: AttendeeSettings,
    pub api_key: String,
}

pub(crate) struct Runtime {
    #[cfg(all(windows, target_arch = "x86_64"))]
    _process: windows::KeepAlive,
    _lease: Lease,
}

pub(crate) const fn available() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}

pub(crate) fn setup(app: &AppHandle) -> CmdResult<(Provisioned, Lease)> {
    let lease = Lease::acquire()?;
    Ok((platform::setup(app)?, lease))
}

pub(crate) fn start(app: &AppHandle) -> CmdResult<(Provisioned, Runtime)> {
    platform::start(app, Lease::acquire()?)
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
mod platform {
    use super::*;

    fn unsupported() -> UiMessage {
        UiMessage::app(
            "native.attendeeSetupUnsupported",
            "Automatic Google Meet setup is available on Windows x64.",
        )
    }

    pub(super) fn setup(_: &AppHandle) -> CmdResult<Provisioned> {
        Err(unsupported())
    }
    pub(super) fn start(_: &AppHandle, _: Lease) -> CmdResult<(Provisioned, Runtime)> {
        Err(unsupported())
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
use windows as platform;

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows {
    use std::{
        env, fs,
        io::{BufRead, BufReader, Read},
        os::windows::process::CommandExt,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use serde::Serialize;
    use tauri::{Emitter, Manager};

    use super::*;

    const DISTRO: &str = "BazzaAttendeeManaged";
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const SETUP: &str = include_str!("../../scripts/attendee-managed/setup-windows.ps1");
    const PROVISION: &str = include_str!("../../scripts/attendee-managed/provision.sh");
    const RUN_SETUP: &str = r#"$run = [ScriptBlock]::Create([IO.File]::ReadAllText($env:BAZZA_MANAGED_SETUP)); & $run -Mode $env:BAZZA_MANAGED_MODE -StateDirectory $env:BAZZA_MANAGED_STATE -ProvisionScript $env:BAZZA_MANAGED_PROVISION; exit $LASTEXITCODE"#;
    const INSTALL_WSL: &str = r#"try { $p = Start-Process -FilePath $env:BAZZA_MANAGED_WSL -ArgumentList '--install','--no-distribution','--web-download' -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ErrorAction Stop; exit $p.ExitCode } catch { exit 1 }"#;

    pub(super) struct KeepAlive(Child);

    impl Drop for KeepAlive {
        fn drop(&mut self) {
            // EOF lets the Linux process exit and WSL suspend its managed guest.
            drop(self.0.stdin.take());
        }
    }

    #[derive(Clone, Serialize)]
    struct Progress {
        phase: &'static str,
        message: Option<String>,
    }

    fn progress(app: &AppHandle, phase: &'static str) {
        let _ = app.emit(
            "attendee_setup_progress",
            Progress {
                phase,
                message: None,
            },
        );
    }

    fn failure() -> UiMessage {
        UiMessage::app(
            "native.attendeeSetupFailed",
            "Bazza could not prepare the local Attendee runtime.",
        )
    }

    fn system32() -> CmdResult<PathBuf> {
        let root = env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(failure)?;
        Ok(root.join("System32"))
    }

    fn hidden(program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    fn powershell(command: &str) -> CmdResult<Command> {
        let mut process = hidden(&system32()?.join("WindowsPowerShell/v1.0/powershell.exe"));
        process.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            command,
        ]);
        Ok(process)
    }

    fn phase(line: &str) -> Option<&'static str> {
        match line.trim().strip_prefix("BAZZA_PROGRESS ")? {
            "preparing" => Some("preparing"),
            "downloading" => Some("downloading"),
            "installing" => Some("installing"),
            "starting" => Some("starting"),
            "checking" => Some("checking"),
            _ => None,
        }
    }

    fn run(app: &AppHandle, mode: &str, max_wait: Duration) -> CmdResult<Result<Provisioned, i32>> {
        let state = app
            .path()
            .app_local_data_dir()
            .map_err(|_| failure())?
            .join("attendee-managed");
        fs::create_dir_all(&state).map_err(|_| failure())?;
        let scripts = tempfile::tempdir_in(&state).map_err(|_| failure())?;
        let setup = scripts.path().join("setup-windows.ps1");
        let provision = scripts.path().join("provision.sh");
        fs::write(&setup, SETUP).map_err(|_| failure())?;
        fs::write(&provision, PROVISION.replace("\r\n", "\n")).map_err(|_| failure())?;
        let mut child = powershell(RUN_SETUP)?
            .env("BAZZA_MANAGED_SETUP", setup)
            .env("BAZZA_MANAGED_MODE", mode)
            .env("BAZZA_MANAGED_STATE", state)
            .env("BAZZA_MANAGED_PROVISION", provision)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| failure())?;
        let stderr = child.stderr.take().ok_or_else(failure)?;
        let stdout = child.stdout.take().ok_or_else(failure)?;
        let app = app.clone();
        let show_progress = mode == "Setup";
        let events = thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(phase) = phase(&line).filter(|_| show_progress) {
                    progress(&app, phase);
                }
            }
        });
        let result = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(64 * 1024).read_to_end(&mut bytes).ok()?;
            serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes)).ok()
        });
        let deadline = Instant::now() + max_wait;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(failure());
                }
            }
        };
        let _ = events.join();
        let result = result.join().map_err(|_| failure())?;
        if status.success() {
            Ok(Ok(result.ok_or_else(failure)?))
        } else {
            Ok(Err(status.code().unwrap_or(20)))
        }
    }

    fn error(code: i32) -> UiMessage {
        match code {
            10 => UiMessage::app(
                "native.attendeeSetupWslRequired",
                "Windows Subsystem for Linux 2 is required to start Google Meet recording.",
            ),
            11 => UiMessage::app(
                "native.attendeeSetupWslNetworking",
                "Bazza could not reach Windows from the local WSL 2 environment.",
            ),
            _ => failure(),
        }
    }

    pub(super) fn setup(app: &AppHandle) -> CmdResult<Provisioned> {
        progress(app, "preparing");
        let max_wait = Duration::from_secs(30 * 60);
        match run(app, "Setup", max_wait)? {
            Ok(provisioned) => return Ok(provisioned),
            Err(10) => {}
            Err(code) => return Err(error(code)),
        }
        progress(app, "enablingWsl");
        let status = powershell(INSTALL_WSL)?
            .env("BAZZA_MANAGED_WSL", system32()?.join("wsl.exe"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| failure())?;
        if !status.success() && status.code() != Some(3010) {
            return Err(UiMessage::app(
                "native.attendeeSetupWslInstallFailed",
                "Windows could not enable WSL 2.",
            ));
        }
        match run(app, "Setup", max_wait)? {
            Ok(provisioned) => Ok(provisioned),
            Err(10) => Err(UiMessage::app(
                "native.attendeeSetupWslRestart",
                "Restart Windows, then click Set up Google Meet again.",
            )),
            Err(code) => Err(error(code)),
        }
    }

    pub(super) fn start(app: &AppHandle, lease: Lease) -> CmdResult<(Provisioned, Runtime)> {
        let provisioned = run(app, "Start", Duration::from_secs(120))?.map_err(error)?;
        let process = hidden(&system32()?.join("wsl.exe"))
            .args(["--distribution", DISTRO, "--user", "root", "--exec", "cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(KeepAlive)
            .map_err(|_| failure())?;
        Ok((
            provisioned,
            Runtime {
                _process: process,
                _lease: lease,
            },
        ))
    }
}

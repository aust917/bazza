//! Explicit, text-only summary calls to an already installed subscription CLI.
//! No shell, project directory, model downloads, or automatic login is involved.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_INPUT: usize = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    Codex,
    Claude,
}

impl Agent {
    pub fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::Claude => "Claude Code",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Availability {
    pub agent: Agent,
    pub installed: bool,
}

pub fn availability() -> Vec<Availability> {
    [Agent::Codex, Agent::Claude]
        .into_iter()
        .map(|agent| Availability {
            agent,
            installed: executable(agent).is_some(),
        })
        .collect()
}

/// Executables only: never route a transcript through cmd.exe, a .cmd wrapper,
/// PowerShell, or a shell interpolating user-controlled text.
fn executable(agent: Agent) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{}.exe", agent.program())
    } else {
        agent.program().to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(&name))
        .find(|p| p.is_file())
}

fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    isolate_process(&mut cmd);
    // The requested path uses the user's subscription login, not inherited API keys.
    for key in [
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "OPENAI_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

fn isolate_process(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

#[cfg(windows)]
struct ProcessTree(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &std::process::Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        // A private kill-on-close job also owns descendants inheriting pipe
        // handles. Killing only the CLI itself would leave reader joins stuck.
        unsafe {
            let job = Self(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
            if job.0.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(job.0, child.as_raw_handle()) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(unix)]
struct ProcessTree(u32);
#[cfg(unix)]
impl ProcessTree {
    fn attach(child: &std::process::Child) -> std::io::Result<Self> {
        Ok(Self(child.id()))
    }
}
#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        // The command starts a new process group, never the application's one.
        unsafe {
            libc::killpg(self.0 as libc::pid_t, libc::SIGKILL);
        }
    }
}

fn configure(cmd: &mut Command, agent: Agent, output: &Path) {
    match agent {
        Agent::Codex => {
            cmd.args([
                "exec",
                "--ignore-user-config",
                "--ignore-rules",
                "--ephemeral",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--color",
                "never",
            ]);
            for setting in [
                "approval_policy=\"never\"",
                "features.shell_tool=false",
                "web_search=\"disabled\"",
                "mcp_servers={}",
                "project_doc_max_bytes=0",
            ] {
                cmd.args(["-c", setting]);
            }
            cmd.arg("--output-last-message").arg(output).arg("-");
        }
        Agent::Claude => {
            // --bare deliberately isn't used: it disables subscription auth.
            cmd.args([
                "--safe-mode",
                "--print",
                "--output-format",
                "json",
                "--tools",
                "",
                "--disallowedTools",
                "*",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--disable-slash-commands",
                "--no-session-persistence",
                "--permission-mode",
                "dontAsk",
            ]);
        }
    }
}

pub struct Output {
    pub markdown: String,
    pub model: String,
}

/// Validate local auth before the first model call. Diagnostic text is never
/// returned or logged because CLI output can contain account details or tokens.
fn subscription_login(
    agent: Agent,
    program: &Path,
    cwd: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let mut cmd = command(program);
    cmd.current_dir(cwd);
    match agent {
        Agent::Codex => {
            cmd.args(["login", "status"]);
        }
        Agent::Claude => {
            cmd.args(["auth", "status"]);
        }
    }
    let (stdout, stderr) =
        execute(cmd, "", cancel, Duration::from_secs(15)).with_context(|| {
            format!(
                "{} login check failed. Sign in with the CLI and check its version.",
                agent.label()
            )
        })?;
    let logged_in = match agent {
        Agent::Codex => format!("{stdout}\n{stderr}")
            .to_lowercase()
            .contains("logged in using chatgpt"),
        Agent::Claude => serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .is_some_and(|v| {
                v["loggedIn"].as_bool() == Some(true)
                    && matches!(v["authMethod"].as_str(), Some("claude.ai" | "oauth_token"))
            }),
    };
    if !logged_in {
        bail!(
            "{} needs an existing subscription login. Sign in using its own CLI first.",
            agent.label()
        );
    }
    Ok(())
}

pub fn run(
    agent: Agent,
    prompt: &str,
    cancel: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<Output> {
    if prompt.trim().is_empty() || prompt.len() > MAX_INPUT {
        bail!("Summary input must be non-empty and at most 2 MiB; use a shorter transcript or template.");
    }
    let program = executable(agent).with_context(|| {
        format!(
            "{} executable not found on PATH. Install and sign in to the CLI first.",
            agent.label()
        )
    })?;
    let work = tempfile::Builder::new().prefix("hark-summary-").tempdir()?;
    subscription_login(agent, &program, work.path(), &cancel)?;
    let output = work.path().join("summary.md");
    let mut cmd = command(&program);
    cmd.current_dir(work.path());
    configure(&mut cmd, agent, &output);
    let (stdout, _) = execute(cmd, prompt, &cancel, timeout)?;
    let (markdown, model) = match agent {
        Agent::Codex => {
            let file =
                std::fs::File::open(&output).context("Codex did not write a final summary")?;
            let mut markdown = String::new();
            file.take((MAX_OUTPUT + 1) as u64)
                .read_to_string(&mut markdown)?;
            (markdown, agent.label().to_string())
        }
        Agent::Claude => {
            let v: serde_json::Value =
                serde_json::from_str(&stdout).context("Claude returned an invalid result")?;
            if v["is_error"].as_bool() == Some(true) {
                bail!("Claude could not generate the summary. Check CLI login and subscription limits.");
            }
            let model = v["modelUsage"]
                .as_object()
                .and_then(|m| m.keys().next())
                .map(|m| format!("Claude Code / {m}"))
                .unwrap_or_else(|| agent.label().into());
            (
                v["result"]
                    .as_str()
                    .context("Claude returned no summary")?
                    .to_string(),
                model,
            )
        }
    };
    if cancel.load(Ordering::Relaxed) {
        bail!("Summary cancelled");
    }
    if markdown.trim().is_empty() || markdown.len() > MAX_OUTPUT {
        bail!("Agent returned an empty or oversized summary");
    }
    Ok(Output {
        markdown: markdown.trim().to_string(),
        model,
    })
}

fn drain(mut input: impl Read, overflow: Arc<AtomicBool>) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            return Ok(output);
        }
        let remaining = MAX_OUTPUT.saturating_sub(output.len());
        output.extend_from_slice(&buf[..n.min(remaining)]);
        if n > remaining {
            overflow.store(true, Ordering::Relaxed);
        }
    }
}

fn execute(
    mut cmd: Command,
    input: &str,
    cancel: &Arc<AtomicBool>,
    timeout: Duration,
) -> Result<(String, String)> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Summary cancelled");
    }
    isolate_process(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not start summary CLI")?;
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e).context("Could not isolate summary CLI process lifetime");
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let bytes = input.as_bytes().to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes));
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let limit = overflow.clone();
    let out = std::thread::spawn(move || drain(stdout, limit));
    let limit = overflow.clone();
    let err = std::thread::spawn(move || drain(stderr, limit));
    let start = Instant::now();
    let result = loop {
        let reason = if cancel.load(Ordering::Relaxed) {
            Some("Summary cancelled")
        } else if start.elapsed() >= timeout {
            Some("Summary timed out")
        } else if overflow.load(Ordering::Relaxed) {
            Some("Summary CLI exceeded the output limit")
        } else {
            None
        };
        if let Some(reason) = reason {
            let _ = child.kill();
            let _ = child.wait();
            break Err(anyhow::anyhow!(reason));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(e.into());
            }
        }
    };
    drop(tree); // close inherited pipes before joining the readers/writer
    let written = writer.join();
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("Summary output reader stopped"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("Summary diagnostic reader stopped"))??;
    let status = result?;
    if overflow.load(Ordering::Relaxed) {
        bail!("Summary CLI exceeded the output limit");
    }
    if !status.success() {
        bail!("Summary CLI failed (exit {}). Check CLI version, subscription login, and limits; the previous summary is preserved.", status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()));
    }
    written
        .map_err(|_| anyhow::anyhow!("Summary input writer stopped"))?
        .context("Could not send summary input")?;
    Ok((
        String::from_utf8(stdout).context("Summary output is not UTF-8")?,
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_boundaries_disable_tools_and_do_not_put_transcript_in_arguments() {
        for agent in [Agent::Codex, Agent::Claude] {
            let mut c = Command::new("fake-agent");
            configure(&mut c, agent, Path::new("summary.md"));
            let args = c
                .get_args()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if agent == Agent::Codex {
                assert!(args.contains(&"features.shell_tool=false".into()));
                assert!(args.contains(&"--ignore-user-config".into()));
                assert!(args.contains(&"read-only".into()));
            } else {
                assert!(args.windows(2).any(|w| w == ["--tools", ""]));
                assert!(args.contains(&"--safe-mode".into()));
                assert!(!args.contains(&"--bare".into()));
            }
        }
    }

    // The test binary itself is a fake CLI; no installed agent or network is used.
    #[test]
    fn fake_cli() {
        let Ok(mode) = std::env::var("HARK_FAKE_SUMMARY") else {
            return;
        };
        match mode.as_str() {
            "descendant" => {
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tests::fake_cli", "--nocapture"])
                    .env("HARK_FAKE_SUMMARY", "hang")
                    .spawn()
                    .unwrap();
                std::thread::sleep(Duration::from_secs(10));
                let _ = child.wait();
            }
            "hang" => std::thread::sleep(Duration::from_secs(10)),
            "flood" => {
                let _ = std::io::stdout().write_all(&vec![b'x'; MAX_OUTPUT + 8192]);
            }
            "fail" => std::process::exit(7),
            _ => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input).unwrap();
                print!("{input}");
            }
        }
    }

    fn fake(mode: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "tests::fake_cli", "--nocapture"])
            .env("HARK_FAKE_SUMMARY", mode);
        c
    }

    #[test]
    fn fake_process_roundtrip_failure_timeout_and_cancellation() {
        let flag = Arc::new(AtomicBool::new(false));
        let (out, _) = execute(
            fake("echo"),
            "$(untrusted) `text`\nПривет",
            &flag,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(out.contains("$(untrusted) `text`\nПривет"));
        assert!(execute(fake("fail"), "", &flag, Duration::from_secs(5))
            .unwrap_err()
            .to_string()
            .contains("exit 7"));
        assert!(execute(fake("hang"), "", &flag, Duration::from_millis(150))
            .unwrap_err()
            .to_string()
            .contains("timed out"));
        assert!(execute(fake("flood"), "", &flag, Duration::from_secs(5)).is_err());
        let started = Instant::now();
        assert!(execute(fake("descendant"), "", &flag, Duration::from_millis(200)).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "descendant holding pipes must be killed as well"
        );
        let cancelled = flag.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancelled.store(true, Ordering::Relaxed);
        });
        assert!(execute(fake("hang"), "", &flag, Duration::from_secs(5))
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        t.join().unwrap();
    }
}

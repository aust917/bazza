use super::*;
use crate::OwnedMediaChild;
use bazza_media_gate::{MediaGateError, MediaPurgeGuard, MediaReadGuard};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::GetHandleInformation;
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::JobObjects::{
    IsProcessInJob, JobObjectExtendedLimitInformation, QueryInformationJobObject,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessHandleCount, GetProcessId, OpenProcess, PROCESS_SYNCHRONIZE,
};

const CONFIG: &str = ".bazza-owned-child-fixture-v1.json";
const FIXTURE: &str = "owned_capture_child::tests::fixture_child";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const JSON_BEGIN: &[u8] = b"\nBAZZA-JSON-BEGIN\n";
const JSON_END: &[u8] = b"\nBAZZA-JSON-END\n";

fn fixture_json() -> Value {
    json!({"text": "русский 日本語 \0\n\"".repeat(6000), "complete": true})
}

fn assert_fixture_json(bytes: &[u8]) {
    let begin = bytes
        .windows(JSON_BEGIN.len())
        .position(|part| part == JSON_BEGIN)
        .expect("missing JSON start")
        + JSON_BEGIN.len();
    let end = bytes[begin..]
        .windows(JSON_END.len())
        .position(|part| part == JSON_END)
        .expect("missing JSON final tail")
        + begin;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes[begin..end]).unwrap(),
        fixture_json()
    );
}

fn fixture_args() -> Vec<String> {
    ["--exact", FIXTURE, "--nocapture", "--test-threads=1"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn configure(directory: &Path, mode: &str) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join(CONFIG),
        serde_json::to_vec(&json!({"mode": mode})).unwrap(),
    )
    .unwrap();
}

fn ready(directory: &Path) {
    std::fs::write(
        directory.join("ready.json"),
        serde_json::to_vec(&json!({"pid": std::process::id()})).unwrap(),
    )
    .unwrap();
}

fn wait_json(path: &Path) -> Value {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not publish {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn observed_process(directory: &Path) -> OwnedHandle {
    let pid = wait_json(&directory.join("ready.json"))["pid"]
        .as_u64()
        .unwrap() as u32;
    // This is only a synchronization handle for a PID just published by our
    // owned fixture. We never terminate a process found by PID or image name.
    owned(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.unwrap())
}

fn assert_exited(process: &OwnedHandle) {
    assert_eq!(
        unsafe { WaitForSingleObject(raw(process), 10_000) },
        WAIT_OBJECT_0
    );
}

struct TestChild(Child);

impl TestChild {
    fn assert_success(&mut self) {
        let status = self.0.wait().unwrap();
        let mut diagnostic = Vec::new();
        if let Some(stderr) = self.0.stderr.take() {
            stderr.take(8192).read_to_end(&mut diagnostic).unwrap();
        }
        assert!(
            status.success(),
            "owned fixture failed ({status}): {}",
            String::from_utf8_lossy(&diagnostic)
        );
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unowned_fixture(directory: &Path) -> TestChild {
    TestChild(
        Command::new(std::env::current_exe().unwrap())
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(fixture_args())
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

fn heartbeat(directory: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("heartbeat"))
        .unwrap();
    ready(directory);
    while Instant::now() < deadline {
        output.write_all(b".").unwrap();
        output.flush().unwrap();
        std::thread::sleep(Duration::from_millis(15));
    }
}

// The same test executable supplies all synthetic children. Invocation state is
// private per-temp cwd; normal test runs have no marker and return immediately.
// No global environment or current-directory mutations are needed.
#[test]
fn fixture_child() {
    let directory = std::env::current_dir().unwrap();
    let config = match std::fs::read(directory.join(CONFIG)) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).unwrap(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => panic!("read owned child fixture: {error}"),
    };
    match config["mode"].as_str().unwrap() {
        "echo" | "echo_tail" => {
            ready(&directory);
            let mut input = String::new();
            std::io::stdin().lock().read_line(&mut input).unwrap();
            if config["mode"] == "echo_tail" {
                std::io::stderr().write_all(&vec![b'z'; 96 * 1024]).unwrap();
                std::io::stderr()
                    .write_all("root-final-tail: русский 日本語\n".as_bytes())
                    .unwrap();
            } else {
                std::io::stderr()
                    .write_all("capture stderr: русский 日本語\n".as_bytes())
                    .unwrap();
            }
            std::fs::write(
                directory.join("observed.json"),
                serde_json::to_vec(&json!({
                    "args": std::env::args().collect::<Vec<_>>(),
                    "cwd": directory,
                    "input": input,
                    "has_console": !unsafe { GetConsoleWindow() }.0.is_null(),
                }))
                .unwrap(),
            )
            .unwrap();
        }
        "json_stdout" | "json_stdout_final" => {
            ready(&directory);
            let mut input = String::new();
            std::io::stdin().lock().read_line(&mut input).unwrap();
            let verbose = config["mode"] == "json_stdout";
            let diagnostic = std::thread::spawn(move || {
                if verbose {
                    std::io::stderr().write_all(&vec![b'e'; 96 * 1024]).unwrap();
                }
                std::io::stderr()
                    .write_all("media-final-stderr: русский 日本語\n".as_bytes())
                    .unwrap();
            });
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(JSON_BEGIN).unwrap();
            serde_json::to_writer(&mut stdout, &fixture_json()).unwrap();
            stdout.write_all(JSON_END).unwrap();
            stdout.flush().unwrap();
            diagnostic.join().unwrap();
            std::fs::write(
                directory.join("observed.json"),
                serde_json::to_vec(&json!({
                    "args": std::env::args().collect::<Vec<_>>(),
                    "input": input,
                    "has_console": !unsafe { GetConsoleWindow() }.0.is_null(),
                }))
                .unwrap(),
            )
            .unwrap();
            // Avoid test-harness suffix text after the synthetic JSON tail.
            std::process::exit(0);
        }
        "writer" => heartbeat(&directory),
        "exit_259" => std::process::exit(259),
        "failure_cleanup" => {
            let executable = std::env::current_exe().unwrap();
            let fail = |phase, _writer: Option<&OwnedHandle>| {
                if phase == SpawnPhase::AttributesReady {
                    Err(io::Error::other("synthetic attribute setup failure"))
                } else {
                    Ok(())
                }
            };
            // Warm up the same path before measuring isolated process handles.
            assert!(OwnedCaptureChild::spawn_checked(&executable, &[], &directory, fail).is_err());
            let mut before = 0;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut before) }.unwrap();
            for _ in 0..64 {
                assert!(
                    OwnedCaptureChild::spawn_checked(&executable, &[], &directory, fail).is_err()
                );
            }
            let mut after = 0;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut after) }.unwrap();
            assert_eq!(before, after, "pre-create failure leaked owned handles");
        }
        "media_failure_cleanup" => {
            let library = directory.join("library");
            std::fs::create_dir(&library).unwrap();
            let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
            let file = guard.try_clone_file().unwrap();
            let executable = std::env::current_exe().unwrap();
            let phases = [
                SpawnPhase::CreatedSuspended,
                SpawnPhase::LifetimeFileDuplicated,
                SpawnPhase::BeforeResume,
                SpawnPhase::Resumed,
            ];
            let exercise = |phase, piped_stdout| {
                let mut observed = None;
                let result = OwnedCaptureChild::spawn_with_output_checked(
                    &executable,
                    &fixture_args(),
                    &directory.join("reader"),
                    &[&file],
                    piped_stdout,
                    |current, process| {
                        if current == SpawnPhase::CreatedSuspended {
                            observed = Some(process.unwrap().try_clone()?);
                        }
                        if current == phase {
                            Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "synthetic post-create failure",
                            ))
                        } else {
                            Ok(())
                        }
                    },
                );
                assert!(
                    matches!(result, Err(error) if error.kind()==io::ErrorKind::PermissionDenied)
                );
                assert_exited(&observed.unwrap());
            };
            for piped_stdout in [false, true] {
                for phase in phases {
                    exercise(phase, piped_stdout);
                }
            }
            let mut before = 0;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut before) }.unwrap();
            for _ in 0..3 {
                for piped_stdout in [false, true] {
                    for phase in phases {
                        exercise(phase, piped_stdout);
                    }
                }
            }
            let mut after = 0;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut after) }.unwrap();
            assert_eq!(before, after, "post-create failure leaked a parent handle");
            drop(file);
            drop(guard);
            drop(MediaPurgeGuard::acquire(&library, "meeting").unwrap());
        }
        "media_supervisor" => {
            let library = directory.join("library");
            std::fs::create_dir(&library).unwrap();
            let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
            let file = guard.try_clone_file().unwrap();
            let mut child = OwnedMediaChild::spawn(
                &std::env::current_exe().unwrap(),
                &fixture_args(),
                &directory.join("reader"),
                &[&file],
            )
            .unwrap();
            drop(file);
            drop(guard);
            wait_json(&directory.join("reader/ready.json"));
            ready(&directory);
            heartbeat(&directory);
            let _ = child.kill();
            let _ = child.wait();
        }
        "suspended_media_supervisor" => {
            let library = directory.join("library");
            std::fs::create_dir(&library).unwrap();
            let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
            let file = guard.try_clone_file().unwrap();
            let _child = OwnedCaptureChild::spawn_with_files_checked(
                &std::env::current_exe().unwrap(),
                &fixture_args(),
                &directory.join("reader"),
                &[&file],
                |phase, process| {
                    if phase == SpawnPhase::BeforeResume {
                        let pid = unsafe { GetProcessId(raw(process.unwrap())) };
                        if pid == 0 {
                            return Err(io::Error::last_os_error());
                        }
                        std::fs::write(
                            directory.join("reader/ready.json"),
                            serde_json::to_vec(&json!({"pid":pid})).unwrap(),
                        )?;
                        ready(&directory);
                        // The outer owned fixture test kills this supervisor.
                        // If that assertion fails, its RAII cleanup still owns it.
                        std::thread::sleep(Duration::from_secs(30));
                    }
                    Ok(())
                },
            )
            .unwrap();
        }
        "branch" | "exit_with_descendant" => {
            let mut grandchild = TestChild(
                Command::new(std::env::current_exe().unwrap())
                    .creation_flags(CREATE_NO_WINDOW.0)
                    .args(fixture_args())
                    .current_dir(directory.join("grandchild"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    // Deliberately retain the root's error pipe in the descendant.
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            wait_json(&directory.join("grandchild/ready.json"));
            ready(&directory);
            if config["mode"] == "branch" {
                heartbeat(&directory);
                let _ = grandchild.0.kill();
                let _ = grandchild.0.wait();
            } else {
                // This owned fixture intentionally exits without joining its
                // descendant, which must be terminated by the parent's job.
                std::process::exit(0);
            }
        }
        "supervisor" => {
            let mut child = OwnedCaptureChild::spawn(
                &std::env::current_exe().unwrap(),
                &fixture_args(),
                &directory.join("branch"),
            )
            .unwrap();
            wait_json(&directory.join("branch/ready.json"));
            ready(&directory);
            heartbeat(&directory);
            let _ = child.kill();
            let _ = child.wait();
        }
        "lookup" => {
            let mut child = OwnedCaptureChild::spawn(
                Path::new("fixture-bare"),
                &fixture_args(),
                &directory.join("echo"),
            )
            .unwrap();
            child.stdin.take().unwrap().write_all(b"q\n").unwrap();
            assert!(child.wait().unwrap().success());
        }
        mode => panic!("unknown owned child fixture mode {mode}"),
    }
}

#[test]
fn windows_spawn_preserves_argv_cwd_stdin_stderr_and_private_job() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("рабочая папка 日本語");
    configure(&directory, "echo");
    let binary_directory = temporary.path().join("программа с пробелами");
    std::fs::create_dir(&binary_directory).unwrap();
    let executable = binary_directory.join("fixture child.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let mut args = fixture_args();
    for value in [
        "",
        "русский 日本語 👩🏽‍💻",
        "quote \" inside",
        r"C:\space path\\",
        "\\",
        "tab\tline\nend",
        "&|<>^%$`",
    ] {
        args.push("--skip".into());
        args.push(value.into());
    }
    let mut child = OwnedCaptureChild::spawn(&executable, &args, &directory).unwrap();
    wait_json(&directory.join("ready.json"));
    assert!(child.try_wait().unwrap().is_none());
    for handle in [child.job.as_ref().unwrap(), child.process.as_ref()] {
        let mut flags = 0;
        unsafe { GetHandleInformation(raw(handle), &mut flags) }.unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
    }
    for stream in [
        child.stdin.as_ref().unwrap(),
        &child.stderr.as_ref().unwrap().pipe,
    ] {
        let mut flags = 0;
        unsafe { GetHandleInformation(HANDLE(stream.as_raw_handle()), &mut flags) }.unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
    }
    let mut in_job = false.into();
    unsafe {
        IsProcessInJob(
            raw(&child.process),
            child.job.as_ref().map(raw),
            &mut in_job,
        )
    }
    .unwrap();
    assert!(in_job.as_bool());
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    unsafe {
        QueryInformationJobObject(
            child.job.as_ref().map(raw),
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as *mut _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            None,
        )
    }
    .unwrap();
    assert_eq!(
        limits.BasicLimitInformation.LimitFlags,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    );
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(child.wait().unwrap().success());
    assert!(child.job.is_none());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("capture stderr: русский 日本語"));
    let observed = wait_json(&directory.join("observed.json"));
    let actual = observed["args"].as_array().unwrap();
    assert_eq!(Path::new(actual[0].as_str().unwrap()), executable);
    assert_eq!(
        actual[1..],
        args.iter().map(|arg| json!(arg)).collect::<Vec<_>>()
    );
    assert_eq!(Path::new(observed["cwd"].as_str().unwrap()), directory);
    assert_eq!(observed["input"], "q\n");
    assert_eq!(observed["has_console"], false);
}

#[test]
fn windows_owner_crash_terminates_child_and_grandchild_only() {
    let temporary = tempfile::tempdir().unwrap();
    let supervisor_dir = temporary.path().join("supervisor");
    configure(&supervisor_dir, "supervisor");
    configure(&supervisor_dir.join("branch"), "branch");
    configure(&supervisor_dir.join("branch/grandchild"), "writer");
    let unrelated_dir = temporary.path().join("unrelated");
    configure(&unrelated_dir, "writer");
    let mut unrelated = unowned_fixture(&unrelated_dir);
    wait_json(&unrelated_dir.join("ready.json"));
    let mut supervisor = unowned_fixture(&supervisor_dir);
    wait_json(&supervisor_dir.join("ready.json"));
    let child = observed_process(&supervisor_dir.join("branch"));
    let grandchild = observed_process(&supervisor_dir.join("branch/grandchild"));
    assert_eq!(unsafe { WaitForSingleObject(raw(&child), 0) }, WAIT_TIMEOUT);
    assert_eq!(
        unsafe { WaitForSingleObject(raw(&grandchild), 0) },
        WAIT_TIMEOUT
    );
    // Kill only the parent we directly spawned. It cannot run Rust Drop.
    supervisor.0.kill().unwrap();
    supervisor.0.wait().unwrap();
    assert_exited(&child);
    assert_exited(&grandchild);
    assert!(unrelated.0.try_wait().unwrap().is_none());
    let before = std::fs::metadata(unrelated_dir.join("heartbeat"))
        .unwrap()
        .len();
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        std::fs::metadata(unrelated_dir.join("heartbeat"))
            .unwrap()
            .len()
            > before
    );
}

#[test]
fn windows_raii_drop_terminates_owned_tree() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "branch");
    configure(&temporary.path().join("grandchild"), "writer");
    let child = OwnedCaptureChild::spawn(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        temporary.path(),
    )
    .unwrap();
    let root = observed_process(temporary.path());
    let grandchild = observed_process(&temporary.path().join("grandchild"));
    drop(child);
    assert_exited(&root);
    assert_exited(&grandchild);
}

#[test]
fn windows_normal_root_exit_closes_descendant_stderr_before_join() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "exit_with_descendant");
    configure(&temporary.path().join("grandchild"), "writer");
    let mut child = OwnedCaptureChild::spawn(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        temporary.path(),
    )
    .unwrap();
    let grandchild = observed_process(&temporary.path().join("grandchild"));
    let mut stderr = child.stderr.take().unwrap();
    let (sent, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        sent.send(stderr.read_to_end(&mut bytes)).unwrap();
    });
    assert!(child.wait().unwrap().success());
    assert!(child.job.is_none());
    // This times out if the terminal path retains the job until Self Drop.
    received.recv_timeout(TEST_TIMEOUT).unwrap().unwrap();
    reader.join().unwrap();
    assert_exited(&grandchild);
}

#[test]
fn windows_stderr_finishes_with_final_tail_while_outside_writer_remains_alive() {
    let temporary = tempfile::tempdir().unwrap();
    let capture = temporary.path().join("capture");
    let outside = temporary.path().join("outside");
    configure(&capture, "echo_tail");
    configure(&outside, "writer");
    let mut holder = None;
    let mut child = OwnedCaptureChild::spawn_checked(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        &capture,
        |phase, writer| {
            if phase == SpawnPhase::AttributesReady {
                // Deterministically reproduce the resulting leaked pipe object
                // through std Command, without relying on its private spawn lock
                // or the scheduler hitting the temporary inheritance window.
                holder = Some(TestChild(
                    Command::new(std::env::current_exe()?)
                        .creation_flags(CREATE_NO_WINDOW.0)
                        .args(fixture_args())
                        .current_dir(&outside)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::from(writer.unwrap().try_clone()?))
                        .spawn()?,
                ));
                wait_json(&outside.join("ready.json"));
            }
            Ok(())
        },
    )
    .unwrap();
    let mut holder = holder.unwrap();
    let mut in_capture_job = false.into();
    unsafe {
        IsProcessInJob(
            HANDLE(holder.0.as_raw_handle()),
            child.job.as_ref().map(raw),
            &mut in_capture_job,
        )
    }
    .unwrap();
    assert!(!in_capture_job.as_bool());
    let mut stderr = child.stderr.take().unwrap();
    let (sent, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sent.send(result);
    });
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    let root_finished =
        unsafe { WaitForSingleObject(raw(&child.process), 10_000) } == WAIT_OBJECT_0;
    if !root_finished {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Intentionally do not call child.wait/try_wait yet: EOF comes from the
    // reader's owned process handle, not from closing the capture job first.
    let result = received.recv_timeout(TEST_TIMEOUT);
    let outside_alive = holder.0.try_wait().unwrap().is_none();
    drop(holder);
    // Join even on a failed assertion/timeout, after releasing the outside pipe.
    reader.join().unwrap();
    assert!(root_finished);
    assert!(outside_alive);
    let bytes = result
        .expect("outside inherited writer kept capture stderr open")
        .unwrap();
    let suffix = "root-final-tail: русский 日本語\n".as_bytes();
    assert_eq!(bytes.len(), 96 * 1024 + suffix.len());
    assert!(bytes[..96 * 1024].iter().all(|byte| *byte == b'z'));
    assert_eq!(&bytes[96 * 1024..], suffix);
    assert!(child.wait().unwrap().success());
}

#[test]
fn windows_canonical_cwd_matches_command_and_preserves_verbatim_semantics() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "echo");
    let canonical = std::fs::canonicalize(temporary.path()).unwrap();
    assert!(canonical
        .as_os_str()
        .as_encoded_bytes()
        .starts_with(br"\\?\"));
    let mut child = OwnedCaptureChild::spawn(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        &canonical,
    )
    .unwrap();
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    assert!(child.wait().unwrap().success());
    let mut diagnostics = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut diagnostics)
        .unwrap();
    let observed = wait_json(&canonical.join("observed.json"));
    assert_eq!(
        std::fs::canonicalize(observed["cwd"].as_str().unwrap()).unwrap(),
        canonical
    );
    for (input, expected) in [
        (r"\\?\C:\Temp\safe", r"C:\Temp\safe"),
        (r"\\?\UNC\server\share\folder", r"\\server\share\folder"),
        (r"\\?\C:\Temp\..\safe", r"\\?\C:\Temp\..\safe"),
        (r"\\?\C:/Temp/safe", r"\\?\C:/Temp/safe"),
        (
            r"\\?\Volume{12345678-1234-1234-1234-123456789012}\folder",
            r"\\?\Volume{12345678-1234-1234-1234-123456789012}\folder",
        ),
        (r"\??\C:\Temp\safe", r"\??\C:\Temp\safe"),
    ] {
        assert_eq!(
            working_directory(Path::new(input)).unwrap(),
            wide_z(OsStr::new(expected)).unwrap()
        );
    }
}

#[test]
fn windows_creation_errors_never_start_a_child_or_fallback() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "writer");
    let executable = std::env::current_exe().unwrap();
    for fail_at in [
        SpawnPhase::JobCreated,
        SpawnPhase::JobConfigured,
        SpawnPhase::AttributesReady,
    ] {
        let result = OwnedCaptureChild::spawn_checked(
            &executable,
            &fixture_args(),
            temporary.path(),
            |phase, _writer| {
                if phase == fail_at {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "synthetic setup failure",
                    ))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
        assert!(!temporary.path().join("ready.json").exists());
    }
    for args in [
        vec!["nul\0argument".into()],
        vec!["x".repeat(COMMAND_LINE_LIMIT)],
    ] {
        assert!(matches!(
            OwnedCaptureChild::spawn(&executable, &args, temporary.path()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
    assert!(OwnedCaptureChild::spawn(
        &temporary.path().join("missing.exe"),
        &fixture_args(),
        temporary.path(),
    )
    .is_err());
    assert!(OwnedCaptureChild::spawn(
        &executable,
        &fixture_args(),
        &temporary.path().join("missing cwd"),
    )
    .is_err());
    assert!(!temporary.path().join("ready.json").exists());
}

#[test]
fn windows_precreate_failure_releases_all_owned_handles() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "failure_cleanup");
    let mut fixture = unowned_fixture(temporary.path());
    fixture.assert_success();
    assert!(!temporary.path().join("ready.json").exists());
}

#[test]
fn windows_media_child_pins_both_gates_until_normal_exit_and_preserves_streams() {
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let directory = temporary.path().join("reader Орион");
    configure(&directory, "echo");
    let first = MediaReadGuard::acquire(&library, "first").unwrap();
    let second = MediaReadGuard::acquire(&library, "second").unwrap();
    let first_file = first.try_clone_file().unwrap();
    let second_file = second.try_clone_file().unwrap();
    let mut args = fixture_args();
    args.extend(["--skip".into(), "media русский 日本語".into()]);
    let mut child = OwnedMediaChild::spawn(
        &std::env::current_exe().unwrap(),
        &args,
        &directory,
        &[&first_file, &second_file],
    )
    .unwrap();
    assert!(
        child.take_stdout().is_none(),
        "default stdout must stay NUL"
    );
    drop(first_file);
    drop(second_file);
    drop(first);
    drop(second);
    wait_json(&directory.join("ready.json"));
    assert!(child.try_wait().unwrap().is_none());
    for id in ["first", "second"] {
        assert!(matches!(
            MediaPurgeGuard::acquire(&library, id),
            Err(MediaGateError::Busy)
        ));
    }
    child.take_stdin().unwrap().write_all(b"q\n").unwrap();
    assert!(child.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .take_stderr()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("capture stderr: русский 日本語"));
    assert!(child.take_stdin().is_none());
    assert!(child.take_stderr().is_none());
    let observed = wait_json(&directory.join("observed.json"));
    assert_eq!(observed["input"], "q\n");
    assert_eq!(observed["has_console"], false);
    assert_eq!(
        observed["args"].as_array().unwrap()[1..],
        args.iter().map(|arg| json!(arg)).collect::<Vec<_>>()
    );
    for id in ["first", "second"] {
        drop(MediaPurgeGuard::acquire(&library, id).unwrap());
    }
}

#[test]
fn windows_media_piped_stdout_preserves_large_utf8_json_and_concurrent_stderr() {
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let directory = temporary.path().join("JSON reader 日本語");
    configure(&directory, "json_stdout");
    let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
    let file = guard.try_clone_file().unwrap();
    let mut args = fixture_args();
    args.extend(["--skip".into(), "media русский 日本語".into()]);
    let mut child = OwnedMediaChild::spawn_piped_stdout(
        &std::env::current_exe().unwrap(),
        &args,
        &directory,
        &[&file],
    )
    .unwrap();
    drop(file);
    drop(guard);
    wait_json(&directory.join("ready.json"));
    assert!(matches!(
        MediaPurgeGuard::acquire(&library, "meeting"),
        Err(MediaGateError::Busy)
    ));
    let mut stdout = child.take_stdout().unwrap();
    let mut stderr = child.take_stderr().unwrap();
    assert!(child.take_stdout().is_none());
    assert!(child.take_stderr().is_none());
    let (output_sent, output_received) = mpsc::channel();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = output_sent.send(stdout.read_to_end(&mut bytes).map(|_| bytes));
    });
    let (error_sent, error_received) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = error_sent.send(stderr.read_to_end(&mut bytes).map(|_| bytes));
    });
    child.take_stdin().unwrap().write_all(b"q\n").unwrap();
    let output = output_received.recv_timeout(TEST_TIMEOUT);
    let diagnostic = error_received.recv_timeout(TEST_TIMEOUT);
    if output.is_err() || diagnostic.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    stdout_reader.join().unwrap();
    stderr_reader.join().unwrap();
    assert!(status.success());
    let output = output.expect("stdout drain did not finish").unwrap();
    assert!(output.len() > STDERR_FINAL_LIMIT);
    assert_fixture_json(&output);
    let diagnostic = diagnostic.expect("stderr drain did not finish").unwrap();
    let suffix = "media-final-stderr: русский 日本語\n".as_bytes();
    assert_eq!(diagnostic.len(), 96 * 1024 + suffix.len());
    assert!(diagnostic[..96 * 1024].iter().all(|byte| *byte == b'e'));
    assert_eq!(&diagnostic[96 * 1024..], suffix);
    let observed = wait_json(&directory.join("observed.json"));
    assert_eq!(observed["input"], "q\n");
    assert_eq!(observed["has_console"], false);
    assert_eq!(
        observed["args"].as_array().unwrap()[1..],
        args.iter().map(|arg| json!(arg)).collect::<Vec<_>>()
    );
    drop(MediaPurgeGuard::acquire(&library, "meeting").unwrap());
}

#[test]
fn windows_stdout_preserves_final_snapshot_larger_than_stderr_tail_limit() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "json_stdout_final");
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // A deliberately roomy test pipe lets the exact root exit with >64KiB
    // buffered before the first read. Production keeps its default pipe size.
    unsafe { CreatePipe(&mut read, &mut write, None, 512 * 1024) }.unwrap();
    let read = owned(read);
    let write = owned(write);
    let mut child = TestChild(
        Command::new(std::env::current_exe().unwrap())
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(fixture_args())
            .current_dir(temporary.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::from(File::from(write)))
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut exact_root = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            HANDLE(child.0.as_raw_handle()),
            GetCurrentProcess(),
            &mut exact_root,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .unwrap();
    let exact_root = Arc::new(owned(exact_root));
    child.0.stdin.take().unwrap().write_all(b"q\n").unwrap();
    assert_exited(&exact_root);
    let mut available = 0;
    unsafe { PeekNamedPipe(raw(&read), None, 0, None, Some(&mut available), None) }.unwrap();
    assert!(available as usize > STDERR_FINAL_LIMIT);
    let mut stdout = CapturedPipe::new(read.into(), exact_root, usize::MAX);
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.len(), available as usize);
    assert_eq!(stdout.final_remaining, Some(0));
    assert_fixture_json(&bytes);
    child.assert_success();
}

#[test]
fn windows_media_child_drop_reaps_root_before_final_pin_is_released() {
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let directory = temporary.path().join("reader");
    configure(&directory, "writer");
    let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
    let file = guard.try_clone_file().unwrap();
    let child = OwnedMediaChild::spawn(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        &directory,
        &[&file],
    )
    .unwrap();
    let process = observed_process(&directory);
    drop(file);
    drop(guard);
    assert!(matches!(
        MediaPurgeGuard::acquire(&library, "meeting"),
        Err(MediaGateError::Busy)
    ));
    drop(child);
    assert_exited(&process);
    drop(MediaPurgeGuard::acquire(&library, "meeting").unwrap());
}

#[test]
fn windows_media_postcreate_failures_reap_children_and_release_all_handles() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "media_failure_cleanup");
    configure(&temporary.path().join("reader"), "writer");
    unowned_fixture(temporary.path()).assert_success();
}

#[test]
fn windows_media_owner_crash_kills_only_owned_reader_and_releases_its_pin() {
    for mode in ["media_supervisor", "suspended_media_supervisor"] {
        let temporary = tempfile::tempdir().unwrap();
        let supervisor_dir = temporary.path().join("supervisor");
        configure(&supervisor_dir, mode);
        configure(&supervisor_dir.join("reader"), "writer");
        let unrelated_dir = temporary.path().join("unrelated");
        configure(&unrelated_dir, "writer");
        let mut unrelated = unowned_fixture(&unrelated_dir);
        wait_json(&unrelated_dir.join("ready.json"));
        let mut supervisor = unowned_fixture(&supervisor_dir);
        wait_json(&supervisor_dir.join("ready.json"));
        let reader = observed_process(&supervisor_dir.join("reader"));
        let library = supervisor_dir.join("library");
        assert!(matches!(
            MediaPurgeGuard::acquire(&library, "meeting"),
            Err(MediaGateError::Busy)
        ));
        if mode == "suspended_media_supervisor" {
            assert!(
                !supervisor_dir.join("reader/heartbeat").exists(),
                "suspended child's first instruction must not run"
            );
        }
        supervisor.0.kill().unwrap();
        supervisor.0.wait().unwrap();
        assert_exited(&reader);
        drop(MediaPurgeGuard::acquire(&library, "meeting").unwrap());
        assert!(unrelated.0.try_wait().unwrap().is_none());
    }
}

#[test]
fn windows_media_invalid_lifetime_files_fail_before_child_creation() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "writer");
    let payload = temporary.path().join("payload");
    std::fs::write(&payload, b"not a gate").unwrap();
    let payload = File::open(payload).unwrap();
    let executable = std::env::current_exe().unwrap();
    assert!(
        matches!(OwnedMediaChild::spawn(&executable, &fixture_args(), temporary.path(), &[&payload]), Err(error) if error.kind()==io::ErrorKind::InvalidInput)
    );
    let empty = temporary.path().join("empty");
    std::fs::write(&empty, []).unwrap();
    let empty = File::open(empty).unwrap();
    assert!(
        matches!(OwnedMediaChild::spawn(&executable, &fixture_args(), temporary.path(), &[&empty; MAX_LIFETIME_FILES+1]), Err(error) if error.kind()==io::ErrorKind::InvalidInput)
    );
    assert!(!temporary.path().join("ready.json").exists());
}

struct InheritAllChild {
    process: OwnedHandle,
}
impl Drop for InheritAllChild {
    fn drop(&mut self) {
        if unsafe { WaitForSingleObject(raw(&self.process), 0) } == WAIT_TIMEOUT {
            // SAFETY: this exact process handle came from our test CreateProcess,
            // never PID discovery. It owns no unrelated process or native input.
            let _ = unsafe {
                windows::Win32::System::Threading::TerminateProcess(raw(&self.process), 1)
            };
        }
        let _ = unsafe { WaitForSingleObject(raw(&self.process), 10_000) };
    }
}
fn inherit_all_fixture(directory: &Path) -> io::Result<InheritAllChild> {
    use windows::Win32::Storage::FileSystem::FILE_GENERIC_READ;
    use windows::Win32::System::Threading::STARTUPINFOW;
    let executable = std::env::current_exe()?;
    let program = wide_z(executable.as_os_str())?;
    let mut command = command_line(&executable, &fixture_args())?;
    let directory = working_directory(directory)?;
    let security = windows::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let null = wide_z(OsStr::new("NUL"))?;
    // This inheritable NUL is stdio only, never an ownership file or job handle.
    let null = owned(
        unsafe {
            CreateFileW(
                PCWSTR(null.as_ptr()),
                FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                Some(&security),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(win32_error)?,
    );
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: raw(&null),
        hStdOutput: raw(&null),
        hStdError: raw(&null),
        ..Default::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    // Deliberately inherit ALL inheritable handles with no HANDLE_LIST. This
    // runs at the ownership-duplication checkpoint, so any unsafe parent pin
    // would escape into this unrelated process and keep purge blocked below.
    unsafe {
        CreateProcessW(
            PCWSTR(program.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            None,
            PCWSTR(directory.as_ptr()),
            &startup,
            &mut information,
        )
    }
    .map_err(win32_error)?;
    drop(owned(information.hThread));
    Ok(InheritAllChild {
        process: owned(information.hProcess),
    })
}

#[test]
fn windows_media_pins_cannot_escape_to_concurrent_unrelated_inherit_all_spawn() {
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let reader_dir = temporary.path().join("reader");
    let outside_dir = temporary.path().join("outside");
    configure(&reader_dir, "echo");
    configure(&outside_dir, "writer");
    let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
    let file = guard.try_clone_file().unwrap();
    let mut outside = None;
    let mut child = OwnedCaptureChild::spawn_with_files_checked(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        &reader_dir,
        &[&file],
        |phase, _process| {
            if phase == SpawnPhase::LifetimeFileDuplicated {
                let mut flags = 0;
                unsafe { GetHandleInformation(HANDLE(file.as_raw_handle()), &mut flags) }
                    .map_err(win32_error)?;
                assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
                outside = Some(inherit_all_fixture(&outside_dir)?);
                wait_json(&outside_dir.join("ready.json"));
            }
            Ok(())
        },
    )
    .unwrap();
    drop(file);
    drop(guard);
    assert!(matches!(
        MediaPurgeGuard::acquire(&library, "meeting"),
        Err(MediaGateError::Busy)
    ));
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    assert!(child.wait().unwrap().success());
    let outside = outside.unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(raw(&outside.process), 0) },
        WAIT_TIMEOUT
    );
    // If a pin escaped into the unrelated live child, this acquisition fails.
    drop(MediaPurgeGuard::acquire(&library, "meeting").unwrap());
}

#[test]
fn windows_piped_stdout_eof_ignores_live_outside_inherit_all_writer() {
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let reader_directory = temporary.path().join("reader");
    let outside_directory = temporary.path().join("outside");
    configure(&reader_directory, "json_stdout");
    configure(&outside_directory, "writer");
    let guard = MediaReadGuard::acquire(&library, "meeting").unwrap();
    let file = guard.try_clone_file().unwrap();
    let mut outside = None;
    let mut child = OwnedCaptureChild::spawn_with_output_checked(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        &reader_directory,
        &[&file],
        true,
        |phase, _| {
            if phase == SpawnPhase::AttributesReady {
                // This unrelated exact-owned child inherits every temporary
                // stream writer. No stdio assignment is needed to keep them.
                outside = Some(inherit_all_fixture(&outside_directory)?);
                wait_json(&outside_directory.join("ready.json"));
            }
            Ok(())
        },
    )
    .unwrap();
    drop(file);
    drop(guard);
    assert!(matches!(
        MediaPurgeGuard::acquire(&library, "meeting"),
        Err(MediaGateError::Busy)
    ));
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (output_sent, output_received) = mpsc::channel();
    let output_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = output_sent.send(stdout.read_to_end(&mut bytes).map(|_| bytes));
    });
    let (error_sent, error_received) = mpsc::channel();
    let error_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = error_sent.send(stderr.read_to_end(&mut bytes).map(|_| bytes));
    });
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    let finished = unsafe { WaitForSingleObject(raw(&child.process), 10_000) } == WAIT_OBJECT_0;
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
    }
    // No wait/try_wait on the success path: readers observe exact-root exit
    // independently of the job handle and outside inherited writer lifetime.
    let output = output_received.recv_timeout(TEST_TIMEOUT);
    let diagnostic = error_received.recv_timeout(TEST_TIMEOUT);
    let outside = outside.unwrap();
    let outside_alive = unsafe { WaitForSingleObject(raw(&outside.process), 0) } == WAIT_TIMEOUT;
    let purge = MediaPurgeGuard::acquire(&library, "meeting");
    drop(outside);
    output_reader.join().unwrap();
    error_reader.join().unwrap();
    assert!(finished && outside_alive);
    assert_fixture_json(&output.expect("outside writer kept stdout open").unwrap());
    assert!(diagnostic
        .expect("outside writer kept stderr open")
        .unwrap()
        .ends_with("media-final-stderr: русский 日本語\n".as_bytes()));
    drop(purge.expect("outside inherited stdio must not inherit the media pin"));
    assert!(child.wait().unwrap().success());
}

#[test]
fn windows_exited_259_is_a_cached_exit_status() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "exit_259");
    let mut child = OwnedCaptureChild::spawn(
        &std::env::current_exe().unwrap(),
        &fixture_args(),
        temporary.path(),
    )
    .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(259));
    assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(259));
    assert!(child.job.is_none());
}

#[test]
fn windows_hresult_normalization_only_classifies_terminal_pipe_errors_as_eof() {
    for code in [109, 233] {
        let error = win32_error(windows::core::Error::from_hresult(windows::core::HRESULT(
            (0x8007_0000u32 | code) as i32,
        )));
        assert_eq!(error.raw_os_error(), Some(code as i32));
        assert!(pipe_eof(&error));
    }
    for code in [5, 6, 232] {
        let error = win32_error(windows::core::Error::from_hresult(windows::core::HRESULT(
            (0x8007_0000u32 | code) as i32,
        )));
        assert_eq!(error.raw_os_error(), Some(code as i32));
        assert!(!pipe_eof(&error));
    }
    let error = win32_error(windows::core::Error::from_hresult(windows::core::HRESULT(
        0x8000_4005u32 as i32,
    )));
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.raw_os_error(), None);
    assert!(!pipe_eof(&error));
}

#[test]
fn windows_bare_executable_lookup_precedes_child_cwd() {
    let temporary = tempfile::tempdir().unwrap();
    configure(temporary.path(), "lookup");
    let echo = temporary.path().join("echo");
    configure(&echo, "echo");
    let bin = temporary.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::copy(
        std::env::current_exe().unwrap(),
        bin.join("fixture-bare.exe"),
    )
    .unwrap();
    // A different child cwd must not redirect executable lookup to this file.
    std::fs::write(echo.join("fixture-bare.exe"), b"not an executable").unwrap();
    let mut fixture = TestChild(
        Command::new(std::env::current_exe().unwrap())
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(fixture_args())
            .current_dir(temporary.path())
            // Child-only PATH: parallel tests and the user's environment stay intact.
            .env("PATH", &bin)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    fixture.assert_success();
    assert_eq!(wait_json(&echo.join("observed.json"))["input"], "q\n");
}

#[test]
#[ignore = "requires explicit BAZZA_TEST_FFMPEG; synthetic lavfi only, no screen/audio devices"]
fn windows_owned_ffmpeg_synthetic_fragmented_video_stops_with_q_and_decodes() {
    use std::sync::atomic::{AtomicU32, Ordering};

    fn reader(
        mut stderr: CapturedStderr,
        frames: Arc<AtomicU32>,
    ) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut tail = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stderr.read(&mut buffer)?;
                if read == 0 {
                    return Ok(tail);
                }
                tail.extend_from_slice(&buffer[..read]);
                if tail.len() > STDERR_FINAL_LIMIT {
                    tail.drain(..tail.len() - STDERR_FINAL_LIMIT);
                }
                for frame in String::from_utf8_lossy(&tail)
                    .lines()
                    .filter_map(|line| line.strip_prefix("frame="))
                    .filter_map(|frame| frame.trim().parse::<u32>().ok())
                {
                    frames.fetch_max(frame, Ordering::Relaxed);
                }
            }
        })
    }

    fn finish(child: &mut OwnedCaptureChild, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }

    let executable = PathBuf::from(
        std::env::var_os("BAZZA_TEST_FFMPEG")
            .expect("set BAZZA_TEST_FFMPEG to the explicit locally prepared ffmpeg executable"),
    );
    assert!(executable.is_absolute() && executable.is_file());
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("synthetic.raw.mp4");
    let args = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-re",
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=64x64:r=15",
        "-an",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-movflags",
        "frag_keyframe+empty_moov+default_base_moof",
        "-progress",
        "pipe:2",
        "-stats_period",
        "0.05",
        "-nostats",
        "-y",
    ]
    .into_iter()
    .map(String::from)
    .chain(std::iter::once(output.to_string_lossy().into_owned()))
    .collect::<Vec<_>>();
    let mut child = OwnedCaptureChild::spawn(&executable, &args, temporary.path()).unwrap();
    let frames = Arc::new(AtomicU32::new(0));
    let capture_reader = reader(child.stderr.take().unwrap(), Arc::clone(&frames));
    let deadline = Instant::now() + TEST_TIMEOUT;
    let ready = loop {
        if frames.load(Ordering::Relaxed) >= 3
            && std::fs::metadata(&output).is_ok_and(|metadata| metadata.len() > 0)
        {
            break true;
        }
        if Instant::now() >= deadline || !matches!(child.try_wait(), Ok(None)) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let q_written = child.stdin.take().unwrap().write_all(b"q\n").is_ok();
    let capture_status = finish(&mut child, Duration::from_secs(5));
    let capture_tail = capture_reader.join().unwrap().unwrap();
    assert!(
        ready,
        "no synthetic frame3/header: {}",
        String::from_utf8_lossy(&capture_tail)
    );
    assert!(
        q_written && capture_status.is_some_and(|status| status.success()),
        "ffmpeg did not finish after q: {}",
        String::from_utf8_lossy(&capture_tail)
    );

    let args = ["-v", "error", "-i"]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(output.to_string_lossy().into_owned()))
        .chain(["-f", "null", "-"].into_iter().map(String::from))
        .collect::<Vec<_>>();
    let mut decoder = OwnedCaptureChild::spawn(&executable, &args, temporary.path()).unwrap();
    drop(decoder.stdin.take());
    let decode_reader = reader(decoder.stderr.take().unwrap(), Arc::new(AtomicU32::new(0)));
    let decode_status = finish(&mut decoder, TEST_TIMEOUT);
    let decode_tail = decode_reader.join().unwrap().unwrap();
    assert!(
        decode_status.is_some_and(|status| status.success()),
        "synthetic fragmented MP4 did not decode: {}",
        String::from_utf8_lossy(&decode_tail)
    );
}

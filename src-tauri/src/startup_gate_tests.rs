//! Windows-only native admission test. Every real Tauri runtime lives in an
//! owned child test process with a fresh identifier and explicitly marked root.
//! No normal app startup, default data directory, AppState or worker is used.
#![cfg(windows)]

use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::Manager;
use uuid::Uuid;

const ROOT_ENV: &str = "BAZZA_STARTUP_GATE_TEST_ROOT";
const ID_ENV: &str = "BAZZA_STARTUP_GATE_TEST_IDENTIFIER";
const ROLE_ENV: &str = "BAZZA_STARTUP_GATE_TEST_ROLE";
const MARKER: &str = ".bazza-startup-gate-fixture.json";
const CHILD_TEST: &str = "startup_gate_tests::child_single_instance_admission";
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);
const STDERR_BYTES: usize = 8192;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    format: String,
    nonce: Uuid,
    root: PathBuf,
    identifier: String,
}

impl Fixture {
    fn new(root: PathBuf) -> Self {
        let nonce = Uuid::new_v4();
        Self {
            format: "bazza-startup-gate-fixture-v1".into(),
            nonce,
            root,
            identifier: format!("app.bazza.test.startupgate.{}", nonce.simple()),
        }
    }

    fn admitted(&self, role: &str) -> PathBuf {
        self.root.join(format!("admitted-{role}.json"))
    }

    fn stop_path(&self, role: &str) -> PathBuf {
        self.root.join(format!("stop-{role}"))
    }

    fn stop_bytes(&self, role: &str) -> Vec<u8> {
        format!("stop:{}:{role}", self.nonce).into_bytes()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    nonce: Uuid,
    role: String,
    process_id: u32,
}

fn regular_file(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.file_attributes() & 0x400 == 0
    })
}

fn bounded_file(path: &Path, max: usize) -> Option<Vec<u8>> {
    if !regular_file(path) {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= max).then_some(bytes)
}

fn write_evidence(path: &Path, value: &Evidence) -> std::io::Result<()> {
    // Fixed, fresh fixture markers. Readers tolerate a not-yet-complete first
    // write, and a repeated focus callback must not replace an open marker.
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.sync_all()
}

fn child_fixture() -> Option<(Fixture, String)> {
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        // Normal unit-test discovery runs this child entry point inertly.
        assert!(std::env::var_os(ID_ENV).is_none());
        assert!(std::env::var_os(ROLE_ENV).is_none());
        return None;
    };
    let root = PathBuf::from(root);
    assert!(
        root.is_absolute(),
        "startup fixture root must be explicit and absolute"
    );
    assert_eq!(
        root.canonicalize().unwrap(),
        root,
        "startup fixture root must be canonical"
    );
    let marker =
        bounded_file(&root.join(MARKER), 4096).expect("exact startup fixture marker required");
    let fixture: Fixture = serde_json::from_slice(&marker).unwrap();
    assert_eq!(fixture.format, "bazza-startup-gate-fixture-v1");
    assert!(!fixture.nonce.is_nil());
    assert_eq!(fixture.root, root);
    assert_eq!(
        fixture.identifier,
        format!("app.bazza.test.startupgate.{}", fixture.nonce.simple())
    );
    assert_eq!(
        marker,
        serde_json::to_vec(&fixture).unwrap(),
        "fixture marker bytes must be exact"
    );
    assert_eq!(std::env::var(ID_ENV).unwrap(), fixture.identifier);
    assert!(
        regular_file(&bazza_store::database_path(&root)),
        "fixture database must already exist"
    );
    let role = std::env::var(ROLE_ENV).unwrap();
    assert!(matches!(role.as_str(), "first" | "second" | "third"));
    Some((fixture, role))
}

/// Invoked only through current_exe --exact and explicit, validated fixture env.
#[test]
fn child_single_instance_admission() {
    let Some((fixture, role)) = child_fixture() else {
        return;
    };
    let fixture = Arc::new(fixture);
    let mut context = tauri::generate_context!();
    let config = context.config_mut();
    config.identifier = fixture.identifier.clone();
    config.product_name = Some("Bazza Startup Gate Fixture".into());
    config.app.windows.clear();
    config.app.security.asset_protocol = Default::default();

    let callback_fixture = fixture.clone();
    let callback_role = role.clone();
    let initialize_fixture = fixture.clone();
    let initialize_role = role.clone();
    let setup_fixture = fixture.clone();
    let setup_role = role.clone();
    let watcher: Arc<Mutex<Option<JoinHandle<()>>>> = Arc::new(Mutex::new(None));
    let setup_watcher = watcher.clone();
    // Pinned Tauri 2.11.5 exposes any_thread() without a bool argument. Actual
    // libtest execution is off the process main thread, so this is required.
    let app = tauri::Builder::default()
        .any_thread()
        .plugin(tauri_plugin_single_instance::init(
            move |app, _args, _cwd| {
                assert!(app.try_state::<bazza_store::Store>().is_some());
                write_evidence(
                    &callback_fixture.root.join("focus.json"),
                    &Evidence {
                        nonce: callback_fixture.nonce,
                        role: callback_role.clone(),
                        process_id: std::process::id(),
                    },
                )
                .expect("write fixture focus evidence");
            },
        ))
        .plugin(crate::startup::library_state_plugin(move |app| {
            // This is the only child Store constructor. A rejected secondary
            // must never reach this plugin or its actual write. The shared
            // plugin initializes state before configured webviews, as normal
            // startup does; this fixture itself remains windowless.
            let store = bazza_store::Store::open_legacy(&bazza_store::database_path(
                &initialize_fixture.root,
            ))?;
            let evidence = Evidence {
                nonce: initialize_fixture.nonce,
                role: initialize_role.clone(),
                process_id: std::process::id(),
            };
            store.set_setting(
                &format!("startup_gate_setup:{}", initialize_role),
                &evidence,
            )?;
            assert!(
                app.manage(store),
                "fixture Store is managed for the runtime lifetime"
            );
            Ok(())
        }))
        .setup(move |app| {
            assert!(
                app.try_state::<bazza_store::Store>().is_some(),
                "library-state plugin must initialize Store before user setup"
            );
            let evidence = Evidence {
                nonce: setup_fixture.nonce,
                role: setup_role.clone(),
                process_id: std::process::id(),
            };
            write_evidence(&setup_fixture.admitted(&setup_role), &evidence)?;

            let stop = setup_fixture.stop_path(&setup_role);
            let expected = setup_fixture.stop_bytes(&setup_role);
            let handle = app.handle().clone();
            *setup_watcher.lock().unwrap() = Some(thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                loop {
                    if bounded_file(&stop, 256).as_deref() == Some(expected.as_slice()) {
                        handle.exit(0);
                        return;
                    }
                    if Instant::now() >= deadline {
                        handle.exit(91);
                        return;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }));
            Ok(())
        })
        .build(context)
        .expect("build isolated native Tauri fixture");
    let code = app.run_return(|_, event| {
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            // No user-facing windows exist. Only an explicit fixture stop or
            // watchdog may terminate the primary admission owner.
            api.prevent_exit();
        }
    });
    let watcher = watcher.lock().unwrap().take();
    if let Some(watcher) = watcher {
        watcher.join().unwrap();
    }
    assert_eq!(code, 0, "fixture watchdog or runtime failure");
}

struct OwnedChild {
    child: Child,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

impl OwnedChild {
    fn spawn(fixture: &Fixture, role: &str) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env(ROOT_ENV, &fixture.root)
            .env(ID_ENV, &fixture.identifier)
            .env(ROLE_ENV, role)
            .current_dir(&fixture.root)
            .creation_flags(0x08000000) // CREATE_NO_WINDOW, never a visible console.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn owned startup fixture child");
        let mut stderr = child.stderr.take().unwrap();
        let reader = thread::spawn(move || {
            let mut kept = Vec::new();
            let mut bytes = [0_u8; 1024];
            loop {
                match stderr.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        let retain = count.min(STDERR_BYTES.saturating_sub(kept.len()));
                        kept.extend_from_slice(&bytes[..retain]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            kept
        });
        Self {
            child,
            stderr: Some(reader),
        }
    }

    fn stderr(&mut self) -> String {
        let bytes = self
            .stderr
            .take()
            .map(|reader| reader.join().unwrap())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn wait(&mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + CHILD_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return (status, self.stderr());
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("owned startup child timed out: {}", self.stderr());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn admitted(&mut self, fixture: &Fixture, role: &str) {
        let deadline = Instant::now() + CHILD_TIMEOUT;
        loop {
            if let Some(bytes) = bounded_file(&fixture.admitted(role), 1024) {
                if let Ok(value) = serde_json::from_slice::<Evidence>(&bytes) {
                    assert_eq!(value.nonce, fixture.nonce);
                    assert_eq!(value.role, role);
                    assert_eq!(value.process_id, self.child.id());
                    return;
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "fixture child exited before setup admission ({status}): {}",
                    self.stderr()
                );
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("owned child admission timed out: {}", self.stderr());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn secondary_focuses_before_store_setup_and_a_new_primary_is_admitted_after_exit() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("single instance fixture Орион");
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture::new(root.canonicalize().unwrap());
    std::fs::write(
        fixture.root.join(MARKER),
        serde_json::to_vec(&fixture).unwrap(),
    )
    .unwrap();
    {
        // Create only this fresh local database. No Core/service identity,
        // default Bazza root, AppState, recovery, engines or other plugin.
        let store =
            bazza_store::Store::open_legacy(&bazza_store::database_path(&fixture.root)).unwrap();
        store
            .set_setting("startup_gate_fixture", &fixture.nonce)
            .unwrap();
    }

    assert_eq!(
        bazza_store::schema_version(&bazza_store::database_path(&fixture.root)).unwrap(),
        9
    );
    let mut first = OwnedChild::spawn(&fixture, "first");
    first.admitted(&fixture, "first");
    let mut second = OwnedChild::spawn(&fixture, "second");
    let (status, stderr) = second.wait();
    assert_eq!(status.code(), Some(0), "secondary failed: {stderr}");
    let focus: Evidence = serde_json::from_slice(
        &bounded_file(&fixture.root.join("focus.json"), 1024)
            .expect("real single-instance callback evidence"),
    )
    .unwrap();
    assert_eq!(focus.nonce, fixture.nonce);
    assert_eq!(focus.role, "first");
    assert_eq!(focus.process_id, first.child.id());
    assert!(
        !fixture.admitted("second").exists(),
        "secondary reached app setup"
    );
    assert!(first.child.try_wait().unwrap().is_none());
    std::fs::write(fixture.stop_path("first"), fixture.stop_bytes("first")).unwrap();
    let (status, stderr) = first.wait();
    assert_eq!(status.code(), Some(0), "primary failed: {stderr}");
    {
        let store =
            bazza_store::Store::open_legacy(&bazza_store::database_path(&fixture.root)).unwrap();
        assert!(store
            .get_setting::<Evidence>("startup_gate_setup:first")
            .unwrap()
            .is_some());
        assert!(store
            .get_setting::<Evidence>("startup_gate_setup:second")
            .unwrap()
            .is_none());
    }

    let mut third = OwnedChild::spawn(&fixture, "third");
    third.admitted(&fixture, "third");
    std::fs::write(fixture.stop_path("third"), fixture.stop_bytes("third")).unwrap();
    let (status, stderr) = third.wait();
    assert_eq!(
        status.code(),
        Some(0),
        "replacement primary failed: {stderr}"
    );
    let store =
        bazza_store::Store::open_legacy(&bazza_store::database_path(&fixture.root)).unwrap();
    assert!(store
        .get_setting::<Evidence>("startup_gate_setup:third")
        .unwrap()
        .is_some());
    assert!(store
        .get_setting::<Evidence>("startup_gate_setup:second")
        .unwrap()
        .is_none());
    assert!(!fixture.root.join(".bazza-library-identity.json").exists());
}

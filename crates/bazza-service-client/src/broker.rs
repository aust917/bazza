//! Windows demand-start broker. Registration is an explicit local setup action.
//! External MCP has no access to these control methods.

use crate::ClientError;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

pub const BROKER_FILE: &str = ".bazza-service-broker.json";

fn setup_guard(root: &Path) -> Result<std::fs::File, ClientError> {
    let path = root.join(".bazza-service-broker-setup.lock");
    if path.symlink_metadata().is_ok() {
        crate::regular_file(&path)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()
        .map_err(|_| ClientError::BrokerUnavailable)?;
    Ok(file)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerSpec {
    version: u16,
    task_name: String,
    executable: PathBuf,
    arguments: String,
    build_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    library_id: Option<Uuid>,
}

fn require_fixture(root: &Path) -> Result<(), ClientError> {
    let marker = root.join(".bazza-synthetic-fixture");
    crate::regular_file(&marker)?;
    let mut value = String::new();
    std::fs::File::open(marker)?
        .take(129)
        .read_to_string(&mut value)?;
    if value != "bazza-mcp-v8-fixture-v1\n" {
        return Err(ClientError::BrokerUnavailable);
    }
    Ok(())
}

fn task_name(root: &Path) -> Result<String, ClientError> {
    let endpoint = bazza_service_transport::endpoint_name(root)?;
    let hash: String = endpoint
        .chars()
        .rev()
        .take(32)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if hash.len() != 32 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ClientError::BrokerUnavailable);
    }
    Ok(format!("BazzaService-{hash}"))
}

fn quote_argument(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
            result.push('"');
        } else {
            result.push_str(&"\\".repeat(slashes));
            result.push(character);
        }
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}

fn arguments(root: &Path, library_id: Option<Uuid>) -> Result<String, ClientError> {
    let root = quote_argument(root.to_str().ok_or(ClientError::BrokerUnavailable)?);
    Ok(if library_id.is_some() {
        format!("serve-library-external --library {root} --idle-timeout-ms 60000")
    } else {
        format!("serve-fixture --library {root}")
    })
}

fn read_spec(
    root: &Path,
    library_id: Option<Uuid>,
    require_current_build: bool,
) -> Result<BrokerSpec, ClientError> {
    let path = root.join(BROKER_FILE);
    crate::regular_file(&path)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(8193)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(ClientError::BrokerUnavailable);
    }
    let spec: BrokerSpec =
        serde_json::from_slice(&bytes).map_err(|_| ClientError::BrokerUnavailable)?;
    if spec.version != 1
        || spec.task_name != task_name(root)?
        || (require_current_build && spec.build_id != bazza_service_api::rpc::build_id())
        || !spec.executable.is_absolute()
        || spec.library_id != library_id
        || spec.arguments != arguments(root, library_id)?
    {
        return Err(ClientError::BrokerUnavailable);
    }
    Ok(spec)
}

pub async fn register_fixture(executable: &Path, root: &Path) -> Result<(), ClientError> {
    require_fixture(root)?;
    let root = root.canonicalize()?;
    register(executable, &root, None).await
}

/// Register demand start for this explicitly initialized library and its pinned ID.
pub async fn register_managed(executable: &Path, root: &Path) -> Result<(), ClientError> {
    let (root, library_id) = crate::validate_managed_root(root)?;
    register(executable, &root, Some(library_id)).await
}

async fn register(
    executable: &Path,
    root: &Path,
    library_id: Option<Uuid>,
) -> Result<(), ClientError> {
    let _setup_guard = setup_guard(root)?;
    let spec = BrokerSpec {
        version: 1,
        task_name: task_name(root)?,
        executable: executable.canonicalize()?,
        arguments: arguments(root, library_id)?,
        build_id: bazza_service_api::rpc::build_id().into(),
        library_id,
    };
    if root.join(BROKER_FILE).try_exists()? {
        let previous = read_spec(root, library_id, false)?;
        // Moving an installation is explicit: unregister the verified old task
        // first. A failed re-register must leave its old setup addressable.
        if previous.executable != spec.executable || previous.arguments != spec.arguments {
            return Err(ClientError::BrokerUnavailable);
        }
    }
    let path = root.join(BROKER_FILE);
    if path.symlink_metadata().is_ok() {
        crate::regular_file(&path)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut file, &spec).map_err(|_| ClientError::BrokerUnavailable)?;
    file.flush()?;
    file.as_file().sync_all()?;
    crate::atomic_publish::persist_atomic(file, &path)?;
    // Persist the bounded, nonsecret setup intent before registering an OS task.
    // Even an interrupted control call remains addressable by unregister/retry.
    control("register", &spec).await?;
    Ok(())
}

pub async fn start_fixture(root: &Path) -> Result<(), ClientError> {
    require_fixture(root)?;
    let root = root.canonicalize()?;
    control("start", &read_spec(&root, None, true)?).await
}
pub async fn unregister_fixture(root: &Path) -> Result<(), ClientError> {
    require_fixture(root)?;
    let root = root.canonicalize()?;
    unregister(&root, None, "unregister").await
}

/// Remove the verified registration after the task goes idle. MCP revocation
/// leaves the registration available to the recorder.
pub async fn unregister_managed(root: &Path) -> Result<(), ClientError> {
    let (root, library_id) = crate::validate_managed_root(root)?;
    unregister(&root, Some(library_id), "unregister-idle").await
}

async fn unregister(
    root: &Path,
    library_id: Option<Uuid>,
    operation: &str,
) -> Result<(), ClientError> {
    let _setup_guard = setup_guard(root)?;
    let path = root.join(BROKER_FILE);
    if library_id.is_some() && !path.try_exists()? {
        return Ok(());
    }
    let spec = read_spec(root, library_id, false)?;
    control(operation, &spec).await?;
    std::fs::remove_file(path)?;
    Ok(())
}

fn may_start(error: &ClientError) -> bool {
    use std::io::ErrorKind;
    match error {
        ClientError::Io(error) => matches!(
            error.kind(),
            ErrorKind::NotFound
                | ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
                | ErrorKind::UnexpectedEof
        ),
        ClientError::Timeout => true,
        _ => false,
    }
}

pub(crate) async fn connect_or_start_managed<T, F, Fut>(
    root: &Path,
    library_id: Uuid,
    connect: F,
) -> Result<T, ClientError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, ClientError>>,
{
    match connect().await {
        Err(error) if may_start(&error) => {}
        result => return result,
    }
    let (current_root, current_id) = crate::validate_managed_root(root)?;
    if current_root != root || current_id != library_id {
        return Err(bazza_service_api::rpc::RpcError::new(
            bazza_service_api::rpc::ErrorCode::ResyncRequired,
        )
        .into());
    }
    let spec = read_spec(root, Some(library_id), true)?;
    tokio::time::timeout(
        Duration::from_millis(bazza_service_api::rpc::MAX_CALL_MS),
        async {
            let mut start_again_at = tokio::time::Instant::now();
            loop {
                if tokio::time::Instant::now() >= start_again_at {
                    control("start", &spec).await?;
                    // IgnoreNew can accept the start while the last owner exits.
                    // A bounded retry also covers that transition.
                    start_again_at = tokio::time::Instant::now() + Duration::from_millis(500);
                }
                match connect().await {
                    Err(error) if may_start(&error) => {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    result => return result,
                }
            }
        },
    )
    .await
    .map_err(|_| ClientError::Timeout)?
}

#[cfg(windows)]
async fn control(operation: &str, spec: &BrokerSpec) -> Result<(), ClientError> {
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let script = include_str!("broker-control.ps1");
    let mut command = tokio::process::Command::new(system_powershell()?);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .env("BAZZA_BROKER_OPERATION", operation)
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let bytes = serde_json::to_vec(spec).map_err(|_| ClientError::BrokerUnavailable)?;
    let work = async {
        let mut input = child.stdin.take().ok_or(ClientError::BrokerUnavailable)?;
        input.write_all(&bytes).await?;
        input.shutdown().await?;
        drop(input);
        let output = child.stdout.take().ok_or(ClientError::BrokerUnavailable)?;
        let error = child.stderr.take().ok_or(ClientError::BrokerUnavailable)?;
        let (output, error, status) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                output
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .await
                    .map(|_| bytes)
            },
            async {
                let mut bytes = Vec::new();
                error
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .await
                    .map(|_| bytes)
            },
            child.wait(),
        );
        let output = output?;
        let error = error?;
        if !status?.success() || output.len() > 4096 || error.len() > 4096 {
            return Err(ClientError::BrokerUnavailable);
        }
        Ok(())
    };
    match tokio::time::timeout(
        Duration::from_millis(bazza_service_api::rpc::MAX_CALL_MS),
        work,
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(ClientError::Timeout)
        }
    }
}

#[cfg(windows)]
fn system_powershell() -> Result<PathBuf, ClientError> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let mut buffer = [0u16; 512];
    // SAFETY: the buffer is writable for the exact supplied element count.
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(ClientError::BrokerUnavailable);
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length]))
        .join("WindowsPowerShell/v1.0/powershell.exe"))
}

#[cfg(not(windows))]
async fn control(_: &str, _: &BrokerSpec) -> Result<(), ClientError> {
    Err(ClientError::BrokerUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_argument_quoting_preserves_spaces_quotes_and_trailing_slashes() {
        assert_eq!(
            quote_argument(r"E:\test library\"),
            "\"E:\\test library\\\\\""
        );
        assert_eq!(quote_argument("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_argument("normal"), "\"normal\"");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn incompatible_reregistration_preserves_the_old_spec_before_any_os_control() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        std::fs::write(
            root.join(".bazza-synthetic-fixture"),
            "bazza-mcp-v8-fixture-v1\n",
        )
        .unwrap();
        let old_executable = root.join("old.exe");
        let new_executable = root.join("new.exe");
        std::fs::write(&old_executable, b"synthetic file, never executed").unwrap();
        std::fs::write(&new_executable, b"synthetic file, never executed").unwrap();
        let spec = BrokerSpec {
            version: 1,
            task_name: task_name(&root).unwrap(),
            executable: old_executable,
            arguments: format!(
                "serve-fixture --library {}",
                quote_argument(root.to_str().unwrap())
            ),
            build_id: "previous-build".into(),
            library_id: None,
        };
        let bytes = serde_json::to_vec(&spec).unwrap();
        std::fs::write(root.join(BROKER_FILE), &bytes).unwrap();
        assert!(read_spec(&root, None, true).is_err());
        assert!(
            read_spec(&root, None, false).is_ok(),
            "old builds remain safely unregisterable"
        );
        assert!(register_fixture(&new_executable, &root).await.is_err());
        assert_eq!(std::fs::read(root.join(BROKER_FILE)).unwrap(), bytes);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn managed_broker_rejects_a_replaced_library_and_never_retries_auth_errors() {
        use bazza_service_api::rpc::{ErrorCode, RpcError};
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let original = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        std::fs::write(
            root.join(".bazza-library-identity.json"),
            format!(r#"{{"version":2,"library_id":"{replacement}"}}"#),
        )
        .unwrap();
        let spec = BrokerSpec {
            version: 1,
            task_name: task_name(&root).unwrap(),
            executable: root.join("never-run.exe"),
            arguments: arguments(&root, Some(original)).unwrap(),
            build_id: bazza_service_api::rpc::build_id().into(),
            library_id: Some(original),
        };
        std::fs::write(root.join(BROKER_FILE), serde_json::to_vec(&spec).unwrap()).unwrap();
        assert!(read_spec(&root, Some(replacement), true).is_err());

        let missing = connect_or_start_managed(&root, original, || async {
            Err::<(), _>(std::io::Error::from(std::io::ErrorKind::NotFound).into())
        })
        .await;
        assert!(
            matches!(missing, Err(ClientError::Remote(error)) if error.code == ErrorCode::ResyncRequired)
        );

        let denied = connect_or_start_managed(&root, replacement, || async {
            Err::<(), _>(RpcError::new(ErrorCode::PermissionDenied).into())
        })
        .await;
        assert!(
            matches!(denied, Err(ClientError::Remote(error)) if error.code == ErrorCode::PermissionDenied)
        );
    }
}

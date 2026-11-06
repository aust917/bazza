//! Explicit synthetic-library GUI. This builder is separate from normal app
//! startup: no AppState, database opener or ordinary background jobs.
use crate::recorder_service::{self, ServiceCapture};
use crate::state::RuntimeState;
use bazza_service_client::{
    api::{
        desktop::*,
        rpc::{ErrorCode, RpcError},
    },
    repository::{
        ChangeContinuation, MeetingContinuation, RepositoryChangePage, RepositoryIdentity,
        RepositoryMeetingPage, RepositorySnapshot, RepositorySummaryPage, RepositorySummaryRequest,
        RepositoryTranscriptPage, RepositoryTranscriptRequest,
    },
    validate_fixture_root, ClientError, RepositoryClient,
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::PathBuf};
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const FLAG: &str = "--service-fixture";
const IDENTIFIER: &str = "app.bazza.mcp.fixture";

#[derive(Debug, Serialize)]
pub(crate) struct FixtureError {
    code: &'static str,
    pub(crate) message: &'static str,
    retryable: bool,
}
impl FixtureError {
    fn new(code: &'static str, message: &'static str, retryable: bool) -> Self {
        Self {
            code,
            message,
            retryable,
        }
    }
    fn arguments() -> Self {
        Self::new(
            "INVALID_ARGUMENT",
            "Use --service-fixture followed by one absolute synthetic library directory.",
            false,
        )
    }
    fn unavailable() -> Self {
        Self::new(
            "SERVICE_UNAVAILABLE",
            "The fixture service is unavailable.",
            true,
        )
    }
}
impl From<ClientError> for FixtureError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::FixtureUnavailable | ClientError::ManagedLibraryUnavailable => Self::new(
                "INVALID_FIXTURE",
                "The selected directory is not a valid synthetic fixture.",
                false,
            ),
            ClientError::Timeout => Self::new(
                "DEADLINE_EXCEEDED",
                "The fixture service call timed out.",
                true,
            ),
            ClientError::Protocol => Self::new(
                "PROTOCOL_ERROR",
                "The fixture service reply was invalid.",
                false,
            ),
            ClientError::Remote(RpcError { code, .. }) => match code {
                ErrorCode::InvalidArgument => {
                    Self::new("INVALID_ARGUMENT", "The bounded request is invalid.", false)
                }
                ErrorCode::Conflict => Self::new(
                    "CONFLICT",
                    "The document changed. Reload its current revision.",
                    false,
                ),
                ErrorCode::IdempotencyConflict => Self::new(
                    "IDEMPOTENCY_CONFLICT",
                    "This request key was already used for different input.",
                    false,
                ),
                ErrorCode::ResyncRequired => Self::new(
                    "RESYNC_REQUIRED",
                    "Reconnect and reload the current library snapshot.",
                    true,
                ),
                ErrorCode::NotFound => {
                    Self::new("NOT_FOUND", "The selected item is unavailable.", false)
                }
                ErrorCode::AuthRequired | ErrorCode::PermissionDenied => Self::new(
                    "PERMISSION_DENIED",
                    "The fixture service did not authorize this operation.",
                    false,
                ),
                ErrorCode::UpgradeRequired => Self::new(
                    "UPGRADE_REQUIRED",
                    "The fixture service build is incompatible.",
                    false,
                ),
                ErrorCode::LibraryUnavailable => Self::new(
                    "LIBRARY_UNAVAILABLE",
                    "The selected fixture library is unavailable.",
                    false,
                ),
                ErrorCode::ResourceBusy => {
                    Self::new("RESOURCE_BUSY", "The fixture resource is busy.", true)
                }
                ErrorCode::DeadlineExceeded => Self::new(
                    "DEADLINE_EXCEEDED",
                    "The fixture service call timed out.",
                    true,
                ),
                ErrorCode::ServiceUnavailable | ErrorCode::BackendUnavailable => {
                    Self::unavailable()
                }
            },
            ClientError::Io(_) | ClientError::Discovery | ClientError::BrokerUnavailable => {
                Self::unavailable()
            }
        }
    }
}

/// Arguments exclude the executable name. A malformed fixture flag fails
/// closed; it never falls through to the normal library startup branch.
pub(crate) fn selected_root(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<PathBuf>, FixtureError> {
    let args: Vec<_> = args.into_iter().collect();
    if !args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with(FLAG))
    {
        return Ok(None);
    }
    if args.len() != 2 || args[0] != FLAG {
        return Err(FixtureError::arguments());
    }
    let root = PathBuf::from(&args[1]);
    if !root.is_absolute() {
        return Err(FixtureError::arguments());
    }
    validate_fixture_root(&root).map(Some).map_err(Into::into)
}

pub(crate) fn run(root: PathBuf) -> Result<(), FixtureError> {
    let root = validate_fixture_root(&root)?;
    let repository =
        tauri::async_runtime::block_on(RepositoryClient::connect_or_start_fixture(&root))?;
    let runtime =
        tauri::async_runtime::block_on(RuntimeState::initialize_from_service(&repository))?;
    let library_id = tauri::async_runtime::block_on(repository.identity()).library_id;
    let mut context = tauri::generate_context!();
    let config = context.config_mut();
    // Tauri 2.11 derives the WebView2 data directory from this identifier.
    // No single-instance plugin is installed in this dedicated builder.
    config.identifier = IDENTIFIER.into();
    config.product_name = Some("Bazza Service Fixture".into());
    config.app.windows.clear();
    config.app.security.asset_protocol = Default::default();

    tauri::Builder::default()
        .manage(repository)
        .manage(runtime)
        .manage(ServiceCapture::new(root, library_id))
        .invoke_handler(tauri::generate_handler![
            service_fixture_bootstrap,
            service_fixture_preferences,
            service_fixture_update_preferences,
            service_fixture_meetings,
            service_fixture_meeting,
            service_fixture_transcript,
            service_fixture_summary,
            service_fixture_rename_meeting,
            service_fixture_delete_meeting,
            service_fixture_changes,
            service_fixture_reconnect,
            recorder_service::service_fixture_capture_start,
            recorder_service::service_fixture_capture_pause,
            recorder_service::service_fixture_capture_resume,
            recorder_service::service_fixture_capture_stop,
            recorder_service::service_fixture_capture_status,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if !window.state::<ServiceCapture>().may_close() {
                    api.prevent_close();
                }
            }
        })
        .setup(|app| {
            WebviewWindowBuilder::new(
                app,
                "service-fixture",
                WebviewUrl::App("index.html?window=service-fixture".into()),
            )
            .title("Bazza Service Fixture")
            .incognito(true)
            .inner_size(1200.0, 780.0)
            .min_inner_size(800.0, 600.0)
            .build()?;
            Ok(())
        })
        .run(context)
        .map_err(|_| {
            FixtureError::new(
                "GUI_UNAVAILABLE",
                "The fixture window could not be started.",
                false,
            )
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeetingsRequest {
    filter: DesktopMeetingFilter,
    cursor: Option<MeetingContinuation>,
    limit: u16,
    max_bytes: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesRequest {
    cursor: Option<ChangeContinuation>,
    limit: u16,
    max_bytes: u32,
}

#[tauri::command]
async fn service_fixture_bootstrap(
    repository: State<'_, RepositoryClient>,
) -> Result<RepositorySnapshot<DesktopBootstrap>, FixtureError> {
    repository.bootstrap().await.map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_preferences(
    repository: State<'_, RepositoryClient>,
) -> Result<RepositorySnapshot<DesktopPreferences>, FixtureError> {
    repository.preferences().await.map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_update_preferences(
    repository: State<'_, RepositoryClient>,
    request: UpdateDesktopPreferencesRequest,
) -> Result<RepositorySnapshot<DesktopMutationOutcome>, FixtureError> {
    repository
        .update_preferences(request)
        .await
        .map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_meetings(
    repository: State<'_, RepositoryClient>,
    request: MeetingsRequest,
) -> Result<RepositoryMeetingPage, FixtureError> {
    repository
        .meetings(
            request.filter,
            request.cursor,
            request.limit,
            request.max_bytes,
        )
        .await
        .map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_meeting(
    repository: State<'_, RepositoryClient>,
    request: DesktopMeetingLookup,
) -> Result<RepositorySnapshot<Option<DesktopMeeting>>, FixtureError> {
    repository
        .meeting(request.meeting_id)
        .await
        .map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_transcript(
    repository: State<'_, RepositoryClient>,
    request: RepositoryTranscriptRequest,
) -> Result<RepositoryTranscriptPage, FixtureError> {
    repository.transcript(request).await.map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_summary(
    repository: State<'_, RepositoryClient>,
    request: RepositorySummaryRequest,
) -> Result<RepositorySummaryPage, FixtureError> {
    repository.summary(request).await.map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_rename_meeting(
    repository: State<'_, RepositoryClient>,
    request: RenameDesktopMeetingRequest,
) -> Result<RepositorySnapshot<DesktopMutationOutcome>, FixtureError> {
    repository.rename_meeting(request).await.map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_delete_meeting(
    repository: State<'_, RepositoryClient>,
    request: DeleteDesktopMeetingRequest,
) -> Result<RepositorySnapshot<DesktopMutationOutcome>, FixtureError> {
    repository.delete_meeting(request).await.map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_changes(
    repository: State<'_, RepositoryClient>,
    request: ChangesRequest,
) -> Result<RepositoryChangePage, FixtureError> {
    repository
        .changes(request.cursor, request.limit, request.max_bytes)
        .await
        .map_err(Into::into)
}
#[tauri::command]
async fn service_fixture_reconnect(
    repository: State<'_, RepositoryClient>,
) -> Result<RepositoryIdentity, FixtureError> {
    validate_fixture_root(repository.root())?;
    repository.reconnect().await.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(|value| OsString::from(*value)).collect()
    }

    #[test]
    fn ordinary_arguments_leave_the_normal_entry_branch_unchanged() {
        for input in [args(&[]), args(&["--ordinary"])] {
            assert!(selected_root(input).unwrap().is_none());
        }
    }

    #[test]
    fn fixture_arguments_reject_missing_relative_duplicate_and_mixed_input() {
        for input in [
            args(&[FLAG]),
            args(&[FLAG, "relative"]),
            args(&["--service-fixture=relative"]),
            args(&[FLAG, "relative", FLAG, "relative"]),
            args(&["--ordinary", FLAG, "relative"]),
        ] {
            assert_eq!(selected_root(input).unwrap_err().code, "INVALID_ARGUMENT");
        }
    }

    #[test]
    fn fixture_selection_never_creates_a_missing_root_or_accepts_an_unmarked_root() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        for root in [missing.clone(), temp.path().to_path_buf()] {
            assert_eq!(
                selected_root([OsString::from(FLAG), root.into_os_string()])
                    .unwrap_err()
                    .code,
                "INVALID_FIXTURE"
            );
        }
        assert!(!missing.exists());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn fixture_selection_returns_only_the_explicit_canonical_synthetic_root() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".bazza-synthetic-fixture"),
            b"bazza-mcp-v8-fixture-v1\n",
        )
        .unwrap();
        let selected = selected_root([OsString::from(FLAG), temp.path().as_os_str().to_owned()])
            .unwrap()
            .unwrap();
        assert_eq!(selected, temp.path().canonicalize().unwrap());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        assert!(!temp.path().join("bazza.db").exists());
    }

    #[test]
    fn fixture_errors_do_not_expose_private_transport_or_remote_details() {
        let private = "E:\\private-library\\secret-marker";
        let errors = [
            ClientError::Io(std::io::Error::other(private)),
            ClientError::Remote(RpcError {
                code: ErrorCode::BackendUnavailable,
                message: private.into(),
                retryable: false,
            }),
        ];
        for error in errors {
            let value = serde_json::to_string(&FixtureError::from(error)).unwrap();
            assert!(!value.contains("private-library"));
            assert!(value.contains("SERVICE_UNAVAILABLE"));
        }
    }
}

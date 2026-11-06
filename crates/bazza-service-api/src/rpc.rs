//! Private application RPC. Public MCP grants/worker/admin contracts are separate.
//! The local transport authenticates the OS user; requests cannot select a role.

use crate::agent_summary::CommitDesktopAgentSummaryRequest;
use crate::chats::{DesktopChatsReply, DesktopChatsRequest};
use crate::configuration::{NativeConfiguration, UpdateNativeConfigurationRequest};
use crate::content::*;
use crate::desktop::*;
use crate::external_read::{
    ExternalReadGrantIssued, ExternalReadGrantPage, ExternalReadGrantRevoked,
    IssueExternalReadGrant, ListExternalReadGrants, RevokeExternalReadGrant,
};
use crate::fixture_read::{FixtureReadReply, FixtureReadRequest};
use crate::library::{DesktopLibraryReply, DesktopLibraryRequest};
use crate::postprocess::*;
use crate::postprocess_cleanup::{PostProcessCleanupReply, PostProcessCleanupRequest};
use crate::postprocess_complete::{
    CompleteFixturePostProcessReceipt, CompleteFixturePostProcessRequest,
};
use crate::postprocess_input::PostProcessTranscriptRequest;
use crate::postprocess_media::{PostProcessMediaGrant, PostProcessMediaRequest};
use crate::postprocess_ready::{PostProcessReadyReceipt, PostProcessReadyRequest};
use crate::postprocess_result::{CommitPostProcessAutoTitleRequest, PostProcessAutoTitleReceipt};
use crate::postprocess_source::{PostProcessSourceReply, PostProcessSourceRequest};
use crate::postprocess_speakers::{PostProcessSpeakersReply, PostProcessSpeakersRequest};
use crate::postprocess_summary::{PostProcessSummaryReply, PostProcessSummaryRequest};
use crate::postprocess_video::{PostProcessVideoReceipt, PostProcessVideoRequest};
use crate::soniox_permission::{
    SonioxKeyOperation, SonioxPermissionReply, SonioxPermissionRequest,
};
use crate::templates::{DesktopTemplatesReply, DesktopTemplatesRequest};
use crate::transcript_versions::{DesktopTranscriptVersionReply, DesktopTranscriptVersionRequest};
use crate::worker::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const API_MAJOR: u16 = 1;
pub const API_MINOR: u16 = 0;
/// Highest storage schema accepted by this version of the private client.
/// Kept independent of Store so the client never links SQLite.
pub const STORAGE_SCHEMA_MAX: i64 = 25;
pub const MAX_CALL_MS: u64 = 5_000;
pub const MAX_CONNECTIONS: usize = 16;
pub const DISCOVERY_VERSION: u16 = 1;
pub const FIXTURE_STATUS_CAPABILITY: &str = "experimental_fixture_status";
pub const FIXTURE_METADATA_CAPABILITY: &str = "experimental_fixture_metadata_read";
pub const FIXTURE_WORKER_CAPABILITY: &str = "experimental_fixture_worker_handoff";
pub const FIXTURE_CAPTURE_TERMINAL_V2_CAPABILITY: &str = "experimental_fixture_capture_terminal_v2";
pub const FIXTURE_CAPTURE_LOCAL_LIVE_CAPABILITY: &str = "experimental_fixture_capture_local_live";

pub fn build_id() -> &'static str {
    env!("BAZZA_API_BUILD_ID")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl ApiVersion {
    pub const CURRENT: Self = Self {
        major: API_MAJOR,
        minor: API_MINOR,
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointDescriptor {
    pub discovery_version: u16,
    pub api: ApiVersion,
    pub process_id: u32,
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub build_id: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    pub api: ApiVersion,
    pub schema_min: i64,
    pub schema_max: i64,
    pub build_id: String,
    pub library_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub request_id: Uuid,
    pub library_epoch: Uuid,
    pub deadline_unix_ms: u64,
    pub request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Request {
    Hello(ClientHello),
    GetStatus,
    GetMeeting(MeetingRequest),
    RegisterWorker(RegisterWorkerRequest),
    RenewWorker(RenewWorkerRequest),
    PrepareCapture(PrepareCaptureRequest),
    BeginCapture(BeginCaptureRequest),
    AbortCapture(AbortCaptureRequest),
    ReattachCapture(ReattachCaptureRequest),
    FinalizeCapture(FinalizeCaptureRequest),
    SonioxPermission(SonioxPermissionRequest),
    SonioxKey(SonioxKeyOperation),
    DesktopBootstrap,
    DesktopPreferences,
    NativeConfiguration,
    UpdateNativeConfiguration(Box<UpdateNativeConfigurationRequest>),
    UpdateDesktopPreferences(UpdateDesktopPreferencesRequest),
    DesktopMeetings(DesktopMeetingRequest),
    DesktopMeeting(DesktopMeetingLookup),
    DesktopMeetingDecorations(DesktopMeetingLookup),
    DesktopChanges(DesktopChangesRequest),
    DesktopLibrary(DesktopLibraryRequest),
    PostProcessJobs(Box<PostProcessJobsRequest>),
    ClaimPostProcessJob(Box<ClaimPostProcessJobRequest>),
    RenewPostProcessJob(Box<RenewPostProcessJobRequest>),
    FailPostProcessJob(Box<FailPostProcessJobRequest>),
    DesktopTemplates(Box<DesktopTemplatesRequest>),
    PostProcessTranscript(Box<PostProcessTranscriptRequest>),
    CommitPostProcessAutoTitle(Box<CommitPostProcessAutoTitleRequest>),
    PostProcessMedia(Box<PostProcessMediaRequest>),
    PostProcessVideo(Box<PostProcessVideoRequest>),
    PostProcessCleanup(Box<PostProcessCleanupRequest>),
    PostProcessSource(Box<PostProcessSourceRequest>),
    PostProcessSpeakers(Box<PostProcessSpeakersRequest>),
    MarkPostProcessReady(Box<PostProcessReadyRequest>),
    PostProcessSummary(Box<PostProcessSummaryRequest>),
    CompleteFixturePostProcess(Box<CompleteFixturePostProcessRequest>),
    DesktopChats(Box<DesktopChatsRequest>),
    FixtureRead(Box<FixtureReadRequest>),
    IssueExternalReadGrant(IssueExternalReadGrant),
    RevokeExternalReadGrant(RevokeExternalReadGrant),
    ListExternalReadGrants(ListExternalReadGrants),
    RenameDesktopMeeting(RenameDesktopMeetingRequest),
    DeleteDesktopMeeting(DeleteDesktopMeetingRequest),
    DesktopTranscript(DesktopTranscriptRequest),
    DesktopSummary(DesktopSummaryRequest),
    CommitDesktopAgentSummary(Box<CommitDesktopAgentSummaryRequest>),
    DesktopTranscriptVersion(Box<DesktopTranscriptVersionRequest>),
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingRequest {
    pub meeting_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyEnvelope {
    pub request_id: Uuid,
    pub library_epoch: Uuid,
    pub reply: Reply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Reply {
    Hello(ServiceStatus),
    Status(ServiceStatus),
    Meeting(Option<MeetingMetadata>),
    RegisteredWorker(RegisteredWorker),
    CaptureLease(CaptureLease),
    CaptureBegun(CaptureLease),
    CaptureAborted(WorkerMutationOutcome),
    WorkerMutationOutcome(WorkerMutationOutcome),
    DesktopBootstrap(DesktopEnvelope<DesktopBootstrap>),
    DesktopPreferences(DesktopEnvelope<DesktopPreferences>),
    NativeConfiguration(Box<DesktopEnvelope<NativeConfiguration>>),
    NativeConfigurationUpdated(DesktopEnvelope<DesktopMutationOutcome>),
    DesktopMeetings(DesktopEnvelope<DesktopMeetingPage>),
    DesktopMeeting(Box<DesktopEnvelope<Option<DesktopMeeting>>>),
    DesktopMeetingDecorations(Box<DesktopEnvelope<DesktopMeetingDecorations>>),
    DesktopChanges(DesktopEnvelope<DesktopChangePage>),
    DesktopLibrary(Box<DesktopEnvelope<DesktopLibraryReply>>),
    PostProcessJobs(Box<DesktopEnvelope<PostProcessJobPage>>),
    PostProcessClaimed(Box<DesktopEnvelope<PostProcessJobLease>>),
    PostProcessRenewed(Box<DesktopEnvelope<PostProcessJobLease>>),
    PostProcessFailed(Box<DesktopEnvelope<PostProcessFailureReceipt>>),
    DesktopTemplates(Box<DesktopEnvelope<DesktopTemplatesReply>>),
    PostProcessTranscript(Box<DesktopEnvelope<DesktopTranscriptPage>>),
    PostProcessAutoTitle(Box<DesktopEnvelope<PostProcessAutoTitleReceipt>>),
    PostProcessMedia(Box<DesktopEnvelope<PostProcessMediaGrant>>),
    PostProcessVideo(Box<DesktopEnvelope<PostProcessVideoReceipt>>),
    PostProcessCleanup(Box<DesktopEnvelope<PostProcessCleanupReply>>),
    PostProcessSource(Box<DesktopEnvelope<PostProcessSourceReply>>),
    PostProcessSpeakers(Box<DesktopEnvelope<PostProcessSpeakersReply>>),
    PostProcessReady(Box<DesktopEnvelope<PostProcessReadyReceipt>>),
    PostProcessSummary(Box<DesktopEnvelope<PostProcessSummaryReply>>),
    FixturePostProcessComplete(Box<DesktopEnvelope<CompleteFixturePostProcessReceipt>>),
    DesktopChats(Box<DesktopEnvelope<DesktopChatsReply>>),
    FixtureRead(Box<FixtureReadReply>),
    ExternalReadGrantIssued(ExternalReadGrantIssued),
    ExternalReadGrantRevoked(ExternalReadGrantRevoked),
    ExternalReadGrantPage(ExternalReadGrantPage),
    SonioxPermission(Box<SonioxPermissionReply>),
    SonioxKeyPresent(bool),
    DesktopMutation(DesktopEnvelope<DesktopMutationOutcome>),
    DesktopTranscript(Box<DesktopEnvelope<DesktopTranscriptPage>>),
    DesktopSummary(Box<DesktopEnvelope<DesktopSummaryPage>>),
    DesktopTranscriptVersion(Box<DesktopEnvelope<DesktopTranscriptVersionReply>>),
    ShutdownAccepted,
    Error(RpcError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceStatus {
    pub api: ApiVersion,
    pub schema_version: i64,
    pub build_id: String,
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub process_id: u32,
    pub authentication: String,
    pub capabilities: Vec<String>,
    pub development_fixture_only: bool,
}

/// An experimental private metadata projection, not the future public meeting DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingMetadata {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: i64,
    pub status: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AuthRequired,
    UpgradeRequired,
    LibraryUnavailable,
    ServiceUnavailable,
    InvalidArgument,
    ResourceBusy,
    DeadlineExceeded,
    ResyncRequired,
    BackendUnavailable,
    PermissionDenied,
    Conflict,
    IdempotencyConflict,
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl RpcError {
    pub fn new(code: ErrorCode) -> Self {
        let (message, retryable) = match code {
            ErrorCode::AuthRequired => ("Authenticated hello is required", false),
            ErrorCode::UpgradeRequired => ("Incompatible API, schema or build", false),
            ErrorCode::LibraryUnavailable => ("The selected library is unavailable", false),
            ErrorCode::ServiceUnavailable => ("The service is unavailable", true),
            ErrorCode::InvalidArgument => ("Invalid bounded request", false),
            ErrorCode::ResourceBusy => ("The library or service is busy", true),
            ErrorCode::DeadlineExceeded => ("The request deadline expired", true),
            ErrorCode::ResyncRequired => ("Reconnect to the current library instance", true),
            ErrorCode::BackendUnavailable => ("The local operation could not be completed", true),
            ErrorCode::PermissionDenied => ("The worker operation is not permitted", false),
            ErrorCode::Conflict => ("The operation refers to a stale revision", false),
            ErrorCode::IdempotencyConflict => {
                ("The retry does not match its original operation", false)
            }
            ErrorCode::NotFound => ("The selected worker resource is unavailable", false),
        };
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for RpcError {}

impl ClientHello {
    pub fn compatible_with(&self, status: &ServiceStatus) -> bool {
        self.compatible_with_mode(status, true)
    }

    pub fn compatible_with_mode(&self, status: &ServiceStatus, fixture: bool) -> bool {
        self.api.major == status.api.major
            && self.api.minor <= status.api.minor
            && self.schema_min > 0
            && self.schema_min <= self.schema_max
            && (self.schema_min..=self.schema_max).contains(&status.schema_version)
            && self.build_id == status.build_id
            && self.library_id == status.library_id
            && status.development_fixture_only == fixture
            && status.authentication == "local_os_user"
            && [FIXTURE_STATUS_CAPABILITY, FIXTURE_METADATA_CAPABILITY]
                .iter()
                .all(|required| {
                    status
                        .capabilities
                        .iter()
                        .any(|capability| capability == required)
                })
    }
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handshake_rejects_api_schema_build_and_library_mismatches() {
        let status = ServiceStatus {
            api: ApiVersion::CURRENT,
            schema_version: 9,
            build_id: build_id().into(),
            library_id: Uuid::new_v4(),
            library_epoch: Uuid::new_v4(),
            process_id: 1,
            authentication: "local_os_user".into(),
            capabilities: vec![
                FIXTURE_STATUS_CAPABILITY.into(),
                FIXTURE_METADATA_CAPABILITY.into(),
            ],
            development_fixture_only: true,
        };
        let hello = ClientHello {
            api: ApiVersion::CURRENT,
            schema_min: 8,
            schema_max: 9,
            build_id: build_id().into(),
            library_id: status.library_id,
        };
        assert!(hello.compatible_with(&status));
        let mut lacking = status.clone();
        lacking.capabilities.clear();
        assert!(!hello.compatible_with(&lacking));
        let mut wrong_context = status.clone();
        wrong_context.development_fixture_only = false;
        assert!(!hello.compatible_with(&wrong_context));
        let mut bad = hello.clone();
        bad.api.major += 1;
        assert!(!bad.compatible_with(&status));
        let mut bad = hello.clone();
        bad.api.minor += 1;
        assert!(!bad.compatible_with(&status));
        let mut bad = hello.clone();
        bad.schema_max = 8;
        assert!(!bad.compatible_with(&status));
        let mut bad = hello.clone();
        bad.schema_min = 10;
        assert!(!bad.compatible_with(&status));
        let mut bad = hello.clone();
        bad.build_id = "other".into();
        assert!(!bad.compatible_with(&status));
        let mut bad = hello;
        bad.library_id = Uuid::new_v4();
        assert!(!bad.compatible_with(&status));
    }
    #[test]
    fn wire_cannot_supply_auth_role_or_unknown_method() {
        let request = RequestEnvelope {
            request_id: Uuid::new_v4(),
            library_epoch: Uuid::new_v4(),
            deadline_unix_ms: unix_ms() + 1000,
            request: Request::GetStatus,
        };
        let mut value = serde_json::to_value(request).unwrap();
        value["role"] = "administration".into();
        assert!(serde_json::from_value::<RequestEnvelope>(value).is_err());
        assert!(serde_json::from_str::<Request>(
            r#"{"method":"invoke","params":{"sql":"DROP TABLE meetings"}}"#
        )
        .is_err());
    }
}

//! Connect only to the service selected by the explicit library root. Each tool
//! call starts its own authenticated pipe to tolerate the idle deadline.
use bazza_service_api::{
    fixture_read::{
        FixtureReadReply, FixtureReadRequest, FixtureReadTool, FIXTURE_READ_CAPABILITY,
    },
    outputs::ErrorCode,
    rpc::{ErrorCode as RpcCode, RpcError},
    tool_schemas,
    worker::WorkerSecret,
    ToolSchema,
};
use bazza_service_client::{
    external_read::ExternalReadClient, external_read_credentials, read_endpoint,
    validate_fixture_root, validate_managed_root, ClientError, ServiceClient,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) struct FixtureTool {
    pub(crate) schema: ToolSchema,
    pub(crate) description: &'static str,
}

pub(crate) fn tools() -> Vec<FixtureTool> {
    let schemas = tool_schemas();
    FixtureReadTool::ALL
        .into_iter()
        .map(|tool| FixtureTool {
            schema: schemas
                .iter()
                .find(|schema| schema.name == tool.name())
                .expect("implemented fixture tool has a contract")
                .clone(),
            description: tool.description(),
        })
        .collect()
}

pub(crate) struct FixtureReads {
    root: PathBuf,
    library_id: Uuid,
    access: Access,
}

enum Access {
    PrivateFixture,
    Grant {
        connection_id: Uuid,
        secret: WorkerSecret,
        mode: GrantMode,
    },
    Installed {
        connection_id: Uuid,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantMode {
    Fixture,
    Managed,
    Installed,
}

async fn verified_client(root: &Path) -> Result<(ServiceClient, Uuid), ClientError> {
    let mut service = ServiceClient::connect(root).await?;
    let status = service.status().await?;
    if !status.development_fixture_only
        || !status
            .capabilities
            .iter()
            .any(|capability| capability == FIXTURE_READ_CAPABILITY)
        || status.library_id.is_nil()
    {
        return Err(RpcError::new(RpcCode::UpgradeRequired).into());
    }
    Ok((service, status.library_id))
}

fn public_code(error: ClientError) -> ErrorCode {
    match error {
        ClientError::FixtureUnavailable | ClientError::ManagedLibraryUnavailable => {
            ErrorCode::LibraryUnavailable
        }
        ClientError::Remote(error) => match error.code {
            RpcCode::AuthRequired => ErrorCode::AuthRequired,
            RpcCode::PermissionDenied => ErrorCode::AccessRevoked,
            RpcCode::UpgradeRequired => ErrorCode::UpgradeRequired,
            RpcCode::LibraryUnavailable => ErrorCode::LibraryUnavailable,
            RpcCode::ResyncRequired => ErrorCode::ResyncRequired,
            RpcCode::ResourceBusy => ErrorCode::ResourceBusy,
            RpcCode::InvalidArgument => ErrorCode::InvalidArgument,
            _ => ErrorCode::ServiceUnavailable,
        },
        _ => ErrorCode::ServiceUnavailable,
    }
}

impl FixtureReads {
    pub(crate) fn library_id(&self) -> Uuid {
        self.library_id
    }

    pub(crate) fn is_managed(&self) -> bool {
        matches!(
            self.access,
            Access::Grant {
                mode: GrantMode::Managed,
                ..
            } | Access::Installed { .. }
        )
    }

    pub(crate) async fn connect(root: &Path) -> Result<Self, ClientError> {
        let root = validate_fixture_root(root)?;
        let (_, library_id) = verified_client(&root).await?;
        Ok(Self {
            root,
            library_id,
            access: Access::PrivateFixture,
        })
    }

    pub(crate) async fn connect_granted(
        root: &Path,
        connection_id: Uuid,
    ) -> Result<Self, ClientError> {
        let root = validate_fixture_root(root)?;
        let library_id = read_endpoint(&root)?.library_id;
        let missing = || RpcError::new(RpcCode::AuthRequired);
        let secret = match std::env::var("BAZZA_MCP_READ_SECRET") {
            Ok(value) => WorkerSecret::from_string(value).map_err(|_| missing())?,
            Err(std::env::VarError::NotPresent) => {
                external_read_credentials::load(library_id, connection_id)
                    .map_err(|_| missing())?
                    .ok_or_else(missing)?
            }
            Err(std::env::VarError::NotUnicode(_)) => return Err(missing().into()),
        };
        Self::granted(root, library_id, connection_id, secret, GrantMode::Fixture).await
    }

    pub(crate) async fn connect_managed(
        root: &Path,
        connection_id: Uuid,
    ) -> Result<Self, ClientError> {
        Self::connect_managed_mode(root, connection_id, GrantMode::Managed).await
    }

    pub(crate) async fn connect_managed_or_start(
        root: &Path,
        connection_id: Uuid,
    ) -> Result<Self, ClientError> {
        Self::connect_managed_mode(root, connection_id, GrantMode::Installed).await
    }

    async fn connect_managed_mode(
        root: &Path,
        connection_id: Uuid,
        mode: GrantMode,
    ) -> Result<Self, ClientError> {
        let (root, library_id) = validate_managed_root(root)?;
        let secret = managed_secret(library_id, connection_id, RpcCode::AuthRequired)?;
        Self::granted(root, library_id, connection_id, secret, mode).await
    }

    async fn granted(
        root: PathBuf,
        library_id: Uuid,
        connection_id: Uuid,
        secret: WorkerSecret,
        mode: GrantMode,
    ) -> Result<Self, ClientError> {
        connect_external(&root, library_id, connection_id, &secret, mode).await?;
        Ok(Self {
            root,
            library_id,
            access: match mode {
                GrantMode::Installed => Access::Installed { connection_id },
                _ => Access::Grant {
                    connection_id,
                    secret,
                    mode,
                },
            },
        })
    }

    pub(crate) async fn call(&self, name: &str, arguments: Value) -> Option<Value> {
        let tool = FixtureReadTool::ALL
            .into_iter()
            .find(|tool| tool.name() == name)?;
        let request = FixtureReadRequest { tool, arguments };
        let failure = |code| FixtureReadReply::failure(self.library_id, code).output;
        if !request.valid() {
            return Some(failure(ErrorCode::InvalidArgument));
        }
        let reply = self.read(request).await;
        Some(match reply {
            Ok(reply) => reply.output,
            Err(error) => failure(public_code(error)),
        })
    }

    async fn read(&self, request: FixtureReadRequest) -> Result<FixtureReadReply, ClientError> {
        let mut client = match &self.access {
            Access::Grant {
                connection_id,
                secret,
                mode,
            } => {
                connect_external(&self.root, self.library_id, *connection_id, secret, *mode).await?
            }
            Access::Installed { connection_id } => {
                // An open host must observe credential removal before starting a
                // service or making another read with a former grant.
                let secret =
                    managed_secret(self.library_id, *connection_id, RpcCode::PermissionDenied)?;
                connect_external(
                    &self.root,
                    self.library_id,
                    *connection_id,
                    &secret,
                    GrantMode::Installed,
                )
                .await?
            }
            Access::PrivateFixture => {
                check_fixture_root(&self.root)?;
                let (mut service, library_id) = verified_client(&self.root).await?;
                if library_id != self.library_id {
                    return Err(RpcError::new(RpcCode::ResyncRequired).into());
                }
                return service.fixture_read(request).await;
            }
        };
        client.read(request).await
    }
}

fn managed_secret(
    library_id: Uuid,
    connection_id: Uuid,
    code: RpcCode,
) -> Result<WorkerSecret, ClientError> {
    let missing = || RpcError::new(code);
    let secret = external_read_credentials::load(library_id, connection_id)
        .map_err(|_| missing())?
        .ok_or_else(missing)?;
    Ok(secret)
}

fn check_fixture_root(root: &Path) -> Result<(), ClientError> {
    if validate_fixture_root(root)? != root {
        return Err(ClientError::FixtureUnavailable);
    }
    Ok(())
}

async fn connect_external(
    root: &Path,
    library_id: Uuid,
    connection_id: Uuid,
    secret: &WorkerSecret,
    mode: GrantMode,
) -> Result<ExternalReadClient, ClientError> {
    if mode != GrantMode::Fixture {
        let (selected_root, selected_id) = validate_managed_root(root)?;
        if selected_root != root || selected_id != library_id {
            return Err(RpcError::new(RpcCode::ResyncRequired).into());
        }
        if mode == GrantMode::Installed {
            return ExternalReadClient::connect_or_start_managed_pinned(
                root,
                connection_id,
                secret,
                library_id,
            )
            .await;
        }
        return ExternalReadClient::connect_managed_pinned(root, connection_id, secret, library_id)
            .await;
    }
    check_fixture_root(root)?;
    ExternalReadClient::connect_pinned(root, connection_id, secret, library_id).await
}

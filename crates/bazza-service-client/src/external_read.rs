//! Client for the external role. This connection cannot send private RPCs.

use crate::{read_endpoint, validate_managed_root, ClientError, ServiceClient};
use bazza_service_api::{
    external_read::*,
    fixture_read::{FixtureReadReply, FixtureReadRequest},
    rpc::{
        build_id, unix_ms, ApiVersion, ClientHello, EndpointDescriptor, ErrorCode, Reply, Request,
        RpcError, MAX_CALL_MS, STORAGE_SCHEMA_MAX,
    },
    worker::WorkerSecret,
};
use bazza_service_transport::{
    connect, external_session_endpoint_name, read_frame, write_frame, PeerStream,
};
use std::{path::Path, time::Duration};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_millis(MAX_CALL_MS);

pub struct ExternalReadClient {
    stream: Option<PeerStream>,
    endpoint: EndpointDescriptor,
}

impl ExternalReadClient {
    pub async fn connect_pinned(
        root: &Path,
        connection_id: Uuid,
        secret: &WorkerSecret,
        library_id: Uuid,
    ) -> Result<Self, ClientError> {
        Self::connect_mode(root, connection_id, secret, library_id, true).await
    }

    pub async fn connect_managed_pinned(
        root: &Path,
        connection_id: Uuid,
        secret: &WorkerSecret,
        library_id: Uuid,
    ) -> Result<Self, ClientError> {
        let (root, selected_id) = validate_managed_root(root)?;
        if selected_id != library_id {
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        Self::connect_mode(&root, connection_id, secret, library_id, false).await
    }

    /// Demand-start with external credentials and an explicitly pinned managed ID.
    /// No private RPC connection is opened by this path.
    pub async fn connect_or_start_managed_pinned(
        root: &Path,
        connection_id: Uuid,
        secret: &WorkerSecret,
        library_id: Uuid,
    ) -> Result<Self, ClientError> {
        let (root, selected_id) = validate_managed_root(root)?;
        if selected_id != library_id {
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        crate::broker::connect_or_start_managed(&root, library_id, || {
            Self::connect_mode(&root, connection_id, secret, library_id, false)
        })
        .await
    }

    async fn connect_mode(
        root: &Path,
        connection_id: Uuid,
        secret: &WorkerSecret,
        library_id: Uuid,
        development_fixture_only: bool,
    ) -> Result<Self, ClientError> {
        if connection_id.is_nil() || library_id.is_nil() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let endpoint = read_endpoint(root)?;
        if endpoint.library_id != library_id {
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        if endpoint.api != ApiVersion::CURRENT || endpoint.build_id != build_id() {
            return Err(RpcError::new(ErrorCode::UpgradeRequired).into());
        }
        let address = external_session_endpoint_name(root, endpoint.library_epoch.as_bytes())?;
        let stream = tokio::time::timeout(TIMEOUT, connect(&address, endpoint.process_id))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let hello = ExternalHello {
            client: ClientHello {
                api: ApiVersion::CURRENT,
                schema_min: 1,
                schema_max: STORAGE_SCHEMA_MAX,
                build_id: build_id().into(),
                library_id: endpoint.library_id,
            },
            connection_id,
            secret: secret.clone(),
        };
        let mut client = Self {
            stream: Some(stream),
            endpoint,
        };
        match client.call(ExternalRequest::Hello(hello)).await? {
            ExternalReply::Hello(accepted)
                if accepted.library_id == client.library_id()
                    && accepted.connection_id == connection_id
                    && accepted.development_fixture_only == development_fixture_only =>
            {
                Ok(client)
            }
            _ => Err(ClientError::Protocol),
        }
    }

    pub fn library_id(&self) -> Uuid {
        self.endpoint.library_id
    }

    pub async fn read(
        &mut self,
        request: FixtureReadRequest,
    ) -> Result<FixtureReadReply, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        match self.call(ExternalRequest::Read(request.clone())).await? {
            ExternalReply::Read(reply) if reply.valid_for(&request, self.library_id()) => Ok(reply),
            _ => {
                self.stream.take();
                Err(ClientError::Protocol)
            }
        }
    }

    async fn call(&mut self, request: ExternalRequest) -> Result<ExternalReply, ClientError> {
        let request_id = Uuid::new_v4();
        let envelope = ExternalRequestEnvelope {
            request_id,
            library_epoch: self.endpoint.library_epoch,
            deadline_unix_ms: unix_ms().saturating_add(MAX_CALL_MS),
            request,
        };
        let mut stream = self.stream.take().ok_or(ClientError::Protocol)?;
        let reply: ExternalReplyEnvelope = tokio::time::timeout(TIMEOUT, async {
            write_frame(&mut stream, &envelope).await?;
            read_frame(&mut stream).await
        })
        .await
        .map_err(|_| ClientError::Timeout)??;
        if reply.request_id != request_id {
            return Err(ClientError::Protocol);
        }
        if reply.library_epoch != self.endpoint.library_epoch {
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        match reply.reply {
            ExternalReply::Error(error) => Err(error.into()),
            reply => {
                self.stream = Some(stream);
                Ok(reply)
            }
        }
    }
}

impl ServiceClient {
    pub async fn list_fixture_external_read_grants(
        &mut self,
        request: ListExternalReadGrants,
    ) -> Result<ExternalReadGrantPage, ClientError> {
        self.list_external_read_grants(request).await
    }

    pub async fn list_external_read_grants(
        &mut self,
        request: ListExternalReadGrants,
    ) -> Result<ExternalReadGrantPage, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        match self
            .call(Request::ListExternalReadGrants(request.clone()))
            .await?
        {
            Reply::ExternalReadGrantPage(page)
                if page.valid_for(&request, self.descriptor.library_id) =>
            {
                Ok(page)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn issue_fixture_external_read_grant(
        &mut self,
        request: IssueExternalReadGrant,
    ) -> Result<ExternalReadGrantIssued, ClientError> {
        self.issue_external_read_grant(request).await
    }

    pub async fn issue_external_read_grant(
        &mut self,
        request: IssueExternalReadGrant,
    ) -> Result<ExternalReadGrantIssued, ClientError> {
        match self.call(Request::IssueExternalReadGrant(request)).await? {
            Reply::ExternalReadGrantIssued(grant)
                if grant.library_id == self.descriptor.library_id
                    && !grant.connection_id.is_nil() =>
            {
                Ok(grant)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn revoke_fixture_external_read_grant(
        &mut self,
        request: RevokeExternalReadGrant,
    ) -> Result<ExternalReadGrantRevoked, ClientError> {
        self.revoke_external_read_grant(request).await
    }

    pub async fn revoke_external_read_grant(
        &mut self,
        request: RevokeExternalReadGrant,
    ) -> Result<ExternalReadGrantRevoked, ClientError> {
        let connection_id = request.connection_id;
        match self.call(Request::RevokeExternalReadGrant(request)).await? {
            Reply::ExternalReadGrantRevoked(grant)
                if grant.library_id == self.descriptor.library_id
                    && grant.connection_id == connection_id =>
            {
                Ok(grant)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

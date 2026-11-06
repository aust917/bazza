//! Closed protocol for the external listener. Private application messages
//! cannot be decoded on this endpoint, including after a successful Hello.
use crate::external_read_gate::{ExternalReadError, ExternalReadGate};
use bazza_core::data::ExternalReadSecret;
use bazza_service_api::{
    external_read::{
        ExternalHelloAccepted, ExternalReply, ExternalReplyEnvelope, ExternalRequest,
        ExternalRequestEnvelope,
    },
    rpc::{unix_ms, ErrorCode, RpcError, ServiceStatus, MAX_CALL_MS},
};
use bazza_service_transport::{read_frame, write_frame, PeerStream};
use std::{sync::Arc, time::Duration};
use tokio::time::{timeout, Instant};
use uuid::Uuid;

fn response(request_id: Uuid, library_epoch: Uuid, reply: ExternalReply) -> ExternalReplyEnvelope {
    ExternalReplyEnvelope {
        request_id,
        library_epoch,
        reply,
    }
}

async fn reject(stream: &mut PeerStream, request_id: Uuid, library_epoch: Uuid, code: ErrorCode) {
    let reply = response(
        request_id,
        library_epoch,
        ExternalReply::Error(RpcError::new(code)),
    );
    let _ = timeout(
        Duration::from_millis(MAX_CALL_MS),
        write_frame(stream, &reply),
    )
    .await;
}

pub(super) async fn handle(
    mut stream: PeerStream,
    status: ServiceStatus,
    gate: Arc<ExternalReadGate>,
) {
    let epoch = status.library_epoch;
    let mut session: Option<(Uuid, Arc<ExternalReadSecret>)> = None;
    loop {
        let request: ExternalRequestEnvelope =
            match timeout(Duration::from_millis(MAX_CALL_MS), read_frame(&mut stream)).await {
                Ok(Ok(request)) => request,
                _ => return,
            };
        let id = request.request_id;
        let now = unix_ms();
        let invalid = if request.library_epoch != epoch {
            Some(ErrorCode::ResyncRequired)
        } else if request.deadline_unix_ms <= now {
            Some(ErrorCode::DeadlineExceeded)
        } else if id.is_nil() || request.deadline_unix_ms > now.saturating_add(MAX_CALL_MS) {
            Some(ErrorCode::InvalidArgument)
        } else {
            None
        };
        if let Some(code) = invalid {
            reject(&mut stream, id, epoch, code).await;
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(request.deadline_unix_ms - now);
        let result = match request.request {
            ExternalRequest::Hello(hello) if session.is_none() => {
                if !hello
                    .client
                    .compatible_with_mode(&status, status.development_fixture_only)
                {
                    reject(&mut stream, id, epoch, ErrorCode::UpgradeRequired).await;
                    return;
                }
                let Ok(secret) = ExternalReadSecret::parse(hello.secret.expose().to_owned()) else {
                    reject(&mut stream, id, epoch, ErrorCode::PermissionDenied).await;
                    return;
                };
                let secret = Arc::new(secret);
                let accepted = response(
                    id,
                    epoch,
                    ExternalReply::Hello(ExternalHelloAccepted {
                        library_id: status.library_id,
                        connection_id: hello.connection_id,
                        development_fixture_only: status.development_fixture_only,
                    }),
                );
                let stream = &mut stream;
                let result = gate
                    .authenticate(
                        hello.connection_id,
                        secret.clone(),
                        deadline,
                        || async move { write_frame(stream, &accepted).await },
                    )
                    .await;
                if result.is_ok() {
                    session = Some((hello.connection_id, secret));
                }
                result
            }
            ExternalRequest::Hello(_) => {
                reject(&mut stream, id, epoch, ErrorCode::InvalidArgument).await;
                return;
            }
            ExternalRequest::Read(request) => {
                let Some((connection_id, secret)) = &session else {
                    reject(&mut stream, id, epoch, ErrorCode::AuthRequired).await;
                    return;
                };
                let stream = &mut stream;
                gate.read(
                    *connection_id,
                    secret.clone(),
                    request,
                    deadline,
                    |reply| async move {
                        write_frame(stream, &response(id, epoch, ExternalReply::Read(reply))).await
                    },
                )
                .await
            }
        };
        match result {
            Ok(()) => {}
            Err(ExternalReadError::AccessDenied | ExternalReadError::GrantNotFound) => {
                reject(&mut stream, id, epoch, ErrorCode::PermissionDenied).await;
                return;
            }
            Err(ExternalReadError::BackendUnavailable) => {
                reject(&mut stream, id, epoch, ErrorCode::BackendUnavailable).await;
                return;
            }
            Err(ExternalReadError::DeadlineExceeded | ExternalReadError::PublicationFailed) => {
                // The publisher may have emitted a partial frame. Close this
                // stream instead of sending an additional error response.
                return;
            }
        }
    }
}

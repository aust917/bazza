//! Private, identity-pinned transcript version transfers.

use super::*;
use bazza_service_api::{
    rpc::{Reply, Request},
    transcript_versions::{
        version_chunk, version_id, DesktopTranscriptVersionReply as VersionReply,
        DesktopTranscriptVersionRequest as VersionRequest, NewTranscriptVersion, TranscriptVersion,
        TranscriptVersionSegment, TranscriptVersions, DESKTOP_TRANSCRIPT_VERSIONS_CAPABILITY,
        MAX_VERSION_CHUNK_BYTES, MAX_VERSION_READ_BYTES,
    },
};
use std::collections::HashSet;

impl ServiceClient {
    async fn require_transcript_versions(&mut self) -> Result<(), ClientError> {
        let status = self.status().await?;
        if status
            .capabilities
            .iter()
            .any(|name| name == DESKTOP_TRANSCRIPT_VERSIONS_CAPABILITY)
        {
            Ok(())
        } else {
            Err(RpcError::new(ErrorCode::UpgradeRequired).into())
        }
    }

    async fn transcript_version_request(
        &mut self,
        request: VersionRequest,
    ) -> Result<VersionReply, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        match self
            .call(Request::DesktopTranscriptVersion(Box::new(request)))
            .await?
        {
            Reply::DesktopTranscriptVersion(reply) if self.desktop_envelope_valid(&reply) => {
                Ok(reply.data)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

impl RepositoryClient {
    pub async fn transcript_versions(
        &self,
        meeting_id: &str,
    ) -> Result<RepositorySnapshot<TranscriptVersions>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let connection = client(&mut state)?;
        connection.require_transcript_versions().await?;
        let reply = connection
            .transcript_version_request(VersionRequest::List {
                meeting_id: meeting_id.into(),
            })
            .await?;
        let VersionReply::Listed { versions } = reply else {
            return connection.invalid_worker_reply();
        };
        let mut ids = HashSet::new();
        if !versions
            .versions
            .iter()
            .all(|item| version_id(&item.id) && ids.insert(item.id.as_str()))
            || versions
                .initial_id
                .as_deref()
                .is_some_and(|id| id != "original" || !ids.contains(id))
            || versions.effective_id.as_deref().is_some_and(|id| !ids.contains(id))
        {
            return connection.invalid_worker_reply();
        }
        Ok(snapshot(&state, versions))
    }

    pub async fn transcript_version(
        &self,
        meeting_id: &str,
        version: &str,
    ) -> Result<RepositorySnapshot<Option<Vec<TranscriptVersionSegment>>>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let connection = client(&mut state)?;
        connection.require_transcript_versions().await?;
        let mut reply = connection
            .transcript_version_request(VersionRequest::Read {
                meeting_id: meeting_id.into(),
                version_id: version.into(),
            })
            .await?;
        if matches!(reply, VersionReply::Missing) {
            return Ok(snapshot(&state, None));
        }
        let mut text = String::new();
        let mut pinned = None;
        loop {
            let VersionReply::Chunk {
                transfer_id,
                offset,
                total_bytes,
                text: chunk,
            } = reply
            else {
                return connection.invalid_worker_reply();
            };
            if transfer_id.is_nil()
                || offset != text.len()
                || chunk.is_empty()
                || chunk.len() > MAX_VERSION_CHUNK_BYTES
                || total_bytes > MAX_VERSION_READ_BYTES
                || offset + chunk.len() > total_bytes
                || pinned.is_some_and(|pin| pin != (transfer_id, total_bytes))
            {
                return connection.invalid_worker_reply();
            }
            pinned = Some((transfer_id, total_bytes));
            text.push_str(&chunk);
            if text.len() == total_bytes {
                break;
            }
            reply = connection
                .transcript_version_request(VersionRequest::ReadNext {
                    transfer_id,
                    offset: text.len(),
                })
                .await?;
        }
        let Ok(rows) = serde_json::from_str::<Vec<TranscriptVersionSegment>>(&text) else {
            return connection.invalid_worker_reply();
        };
        if rows.iter().any(|row| {
            let segment = &row.segment;
            segment.meeting_id != meeting_id
                || segment.start_ms < 0
                || segment.end_ms < segment.start_ms
                || row.playback.is_some_and(|timing| !timing.valid())
                || row.provenance.as_ref().is_some_and(|origin| !origin.valid())
                || segment.id == 0
                || (version == "original" && row.playback.is_some())
                || rows.first().is_some_and(|first| (first.segment.id > 0) != (segment.id > 0))
        }) {
            return connection.invalid_worker_reply();
        }
        Ok(snapshot(&state, Some(rows)))
    }

    pub async fn save_transcript_version(
        &self,
        identity: &RepositoryIdentity,
        meeting_id: &str,
        lifecycle: &str,
        transcript: &str,
        input: &NewTranscriptVersion,
    ) -> Result<RepositorySnapshot<TranscriptVersion>, ClientError> {
        let version_id =
            Uuid::parse_str(&input.id).map_err(|_| RpcError::new(ErrorCode::InvalidArgument))?;
        if !input.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let text =
            serde_json::to_string(input).map_err(|_| RpcError::new(ErrorCode::InvalidArgument))?;
        let mut state = self.shared.state.lock().await;
        if &state.identity != identity {
            return Err(resync());
        }
        let connection = client(&mut state)?;
        connection.require_transcript_versions().await?;
        let reply = connection
            .transcript_version_request(VersionRequest::Begin {
                meeting_id: meeting_id.into(),
                expected_lifecycle_revision: lifecycle.into(),
                expected_transcript_revision: transcript.into(),
                version_id,
                total_bytes: text.len(),
            })
            .await?;
        let VersionReply::Staged {
            transfer_id,
            received_bytes: 0,
        } = reply
        else {
            return connection.invalid_worker_reply();
        };
        if transfer_id.is_nil() {
            return connection.invalid_worker_reply();
        }
        let mut offset = 0;
        while let Some(chunk) = version_chunk(&text, offset) {
            let reply = connection
                .transcript_version_request(VersionRequest::Append {
                    transfer_id,
                    offset,
                    text: chunk.into(),
                })
                .await?;
            offset += chunk.len();
            if !matches!(reply, VersionReply::Staged { transfer_id: id, received_bytes } if id == transfer_id && received_bytes == offset)
            {
                return connection.invalid_worker_reply();
            }
        }
        let reply = connection
            .transcript_version_request(VersionRequest::Commit { transfer_id })
            .await?;
        let VersionReply::Saved { version } = reply else {
            return connection.invalid_worker_reply();
        };
        if version.id != input.id
            || version.source != input.source
            || version.model.as_deref() != Some(input.model.as_str())
            || version.language != input.language
            || version.partial != input.partial
        {
            return connection.invalid_worker_reply();
        }
        Ok(snapshot(&state, version))
    }
}

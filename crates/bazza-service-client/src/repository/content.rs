//! Identity-bound content pages. The facade returns one bounded page per call;
//! collecting transcript or structured summary documents is the caller's choice.
use super::*;
use bazza_service_api::{agent_summary::CommitDesktopAgentSummaryRequest, content::*};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: DesktopTranscriptCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: DesktopSummaryCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTranscriptRequest {
    pub meeting_id: String,
    pub source_id: Option<String>,
    pub expected_revision: String,
    pub representation: DesktopTranscriptRepresentation,
    pub cursor: Option<TranscriptContinuation>,
    pub limit: u16,
    pub max_bytes: u32,
}
impl RepositoryTranscriptRequest {
    pub fn new(
        meeting_id: String,
        expected_revision: String,
        representation: DesktopTranscriptRepresentation,
    ) -> Self {
        Self {
            meeting_id,
            source_id: None,
            expected_revision,
            representation,
            cursor: None,
            limit: 20,
            max_bytes: MAX_DESKTOP_DATA_BYTES,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositorySummaryRequest {
    pub meeting_id: String,
    pub expected_revision: String,
    pub field: DesktopSummaryField,
    pub cursor: Option<SummaryContinuation>,
    pub max_bytes: u32,
}
impl RepositorySummaryRequest {
    pub fn new(meeting_id: String, expected_revision: String, field: DesktopSummaryField) -> Self {
        Self {
            meeting_id,
            expected_revision,
            field,
            cursor: None,
            max_bytes: MAX_DESKTOP_DATA_BYTES,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTranscriptPage {
    pub identity: RepositoryIdentity,
    pub state: DesktopTranscriptState,
    pub source_id: Option<String>,
    pub representation: DesktopTranscriptRepresentation,
    pub items: Vec<DesktopTranscriptFragment>,
    pub next: Option<TranscriptContinuation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositorySummaryPage {
    pub identity: RepositoryIdentity,
    pub state: DesktopSummaryState,
    pub field: DesktopSummaryField,
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub next: Option<SummaryContinuation>,
}

impl RepositoryClient {
    pub async fn transcript(
        &self,
        request: RepositoryTranscriptRequest,
    ) -> Result<RepositoryTranscriptPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if request.cursor.as_ref().is_some_and(|cursor| {
            cursor.library_id != state.identity.library_id
                || cursor.library_epoch != state.identity.library_epoch
                || cursor.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let page = client(&mut state)?
            .desktop_transcript(DesktopTranscriptRequest {
                meeting_id: request.meeting_id,
                source_id: request.source_id,
                expected_revision: request.expected_revision,
                representation: request.representation,
                cursor: request.cursor.map(|cursor| cursor.cursor),
                limit: request.limit,
                max_bytes: request.max_bytes,
            })
            .await?
            .data;
        if page.library_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositoryTranscriptPage {
            identity: state.identity.clone(),
            state: page.state,
            source_id: page.source_id,
            representation: page.representation,
            items: page.items,
            next: page.next.map(|cursor| TranscriptContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor,
            }),
        })
    }
    pub async fn commit_agent_summary(
        &self,
        request: CommitDesktopAgentSummaryRequest,
    ) -> Result<RepositorySnapshot<DesktopMutationOutcome>, ClientError> {
        let mut state = self.shared.state.lock().await;
        if request.library_id != state.identity.library_id
            || request.library_epoch != state.identity.library_epoch
        {
            return Err(resync());
        }
        let data = client(&mut state)?.commit_desktop_agent_summary(request).await?.data;
        Ok(snapshot(&state, data))
    }

    pub async fn summary(
        &self,
        request: RepositorySummaryRequest,
    ) -> Result<RepositorySummaryPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if request.cursor.as_ref().is_some_and(|cursor| {
            cursor.library_id != state.identity.library_id
                || cursor.library_epoch != state.identity.library_epoch
                || cursor.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let page = client(&mut state)?
            .desktop_summary(DesktopSummaryRequest {
                meeting_id: request.meeting_id,
                expected_revision: request.expected_revision,
                field: request.field,
                cursor: request.cursor.map(|cursor| cursor.cursor),
                max_bytes: request.max_bytes,
            })
            .await?
            .data;
        if page.library_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositorySummaryPage {
            identity: state.identity.clone(),
            state: page.state,
            field: page.field,
            text: page.text,
            byte_offset: page.byte_offset,
            total_bytes: page.total_bytes,
            next: page.next.map(|cursor| SummaryContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor,
            }),
        })
    }
}

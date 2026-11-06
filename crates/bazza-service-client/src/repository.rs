//! Cloneable desktop repository facade. All persistence remains in the service.
//! Calls and explicit reconnect are serialized; no implicit mutation retry,
//! library creation, default directory selection or SQLite fallback exists.
mod chats;
mod configuration;
mod library;
mod templates;
mod transcript_versions;
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::*,
    rpc::{ErrorCode, RpcError},
};
pub use chats::{RepositoryChatsContinuation, RepositoryChatsPage};
pub use library::*;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
pub use templates::*;
use tokio::sync::Mutex;
use uuid::Uuid;

mod content;
pub use content::{
    RepositorySummaryPage, RepositorySummaryRequest, RepositoryTranscriptPage,
    RepositoryTranscriptRequest, SummaryContinuation, TranscriptContinuation,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryIdentity {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor_scope: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositorySnapshot<T> {
    pub identity: RepositoryIdentity,
    pub data: T,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: DesktopCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: DesktopChangeCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMeetingPage {
    pub identity: RepositoryIdentity,
    pub items: Vec<DesktopMeeting>,
    pub next: Option<MeetingContinuation>,
    pub snapshot_sequence: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryChangePage {
    pub identity: RepositoryIdentity,
    pub items: Vec<DesktopChange>,
    pub next: ChangeContinuation,
    pub has_more: bool,
    pub current_sequence: i64,
}
struct State {
    client: Option<ServiceClient>,
    identity: RepositoryIdentity,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RepositoryMode {
    Fixture,
    Managed,
    Installed,
}
struct Shared {
    root: PathBuf,
    library_id: Uuid,
    cursor_scope: String,
    mode: RepositoryMode,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct RepositoryClient {
    shared: Arc<Shared>,
}

fn resync() -> ClientError {
    RpcError::new(ErrorCode::ResyncRequired).into()
}
fn client(state: &mut State) -> Result<&mut ServiceClient, ClientError> {
    state.client.as_mut().ok_or(ClientError::Protocol)
}
fn snapshot<T>(state: &State, data: T) -> RepositorySnapshot<T> {
    RepositorySnapshot {
        identity: state.identity.clone(),
        data,
    }
}

impl RepositoryClient {
    /// Explicit canonical root selected at construction, never a fallback path.
    pub fn root(&self) -> &Path {
        &self.shared.root
    }
    pub async fn connect(root: &Path) -> Result<Self, ClientError> {
        let root = root.canonicalize()?;
        let client = ServiceClient::connect(&root).await?;
        Self::from_client(root, client, RepositoryMode::Fixture).await
    }
    pub async fn connect_managed(root: &Path) -> Result<Self, ClientError> {
        let (root, _) = crate::validate_managed_root(root)?;
        let client = ServiceClient::connect_managed(&root).await?;
        Self::from_client(root, client, RepositoryMode::Managed).await
    }
    /// Use the same demand-start registration as the installed MCP adapter.
    pub async fn connect_or_start_managed(root: &Path) -> Result<Self, ClientError> {
        let (root, library_id) = crate::validate_managed_root(root)?;
        let client = ServiceClient::connect_or_start_managed_pinned(&root, library_id).await?;
        Self::from_client(root, client, RepositoryMode::Installed).await
    }
    /// Explicit demand-start through an already configured verified broker.
    pub async fn connect_or_start_fixture(root: &Path) -> Result<Self, ClientError> {
        let root = root.canonicalize()?;
        let client = ServiceClient::connect_or_start_fixture(&root).await?;
        Self::from_client(root, client, RepositoryMode::Fixture).await
    }
    async fn from_client(
        root: PathBuf,
        mut client: ServiceClient,
        mode: RepositoryMode,
    ) -> Result<Self, ClientError> {
        let bootstrap = client.desktop_bootstrap().await?;
        let identity = RepositoryIdentity {
            library_id: bootstrap.library_id,
            library_epoch: bootstrap.library_epoch,
            cursor_scope: bootstrap.data.cursor_scope,
        };
        Ok(Self {
            shared: Arc::new(Shared {
                root,
                library_id: identity.library_id,
                cursor_scope: identity.cursor_scope.clone(),
                mode,
                state: Mutex::new(State {
                    client: Some(client),
                    identity,
                }),
            }),
        })
    }
    pub async fn identity(&self) -> RepositoryIdentity {
        self.shared.state.lock().await.identity.clone()
    }

    /// Keep the selected desktop session open. This never retries a write;
    /// reconnection uses the same pinned library and checks its cursor scope.
    pub async fn keep_desktop_session(&self) -> Result<(), ClientError> {
        {
            let mut state = self.shared.state.lock().await;
            if let Ok(connection) = client(&mut state) {
                if connection.status().await.is_ok_and(|status| {
                    status.library_id == self.shared.library_id
                        && status.library_epoch == state.identity.library_epoch
                        && status.development_fixture_only
                            == (self.shared.mode == RepositoryMode::Fixture)
                }) {
                    return Ok(());
                }
            }
        }
        self.reconnect().await.map(|_| ())
    }
    /// A failed reconnect leaves no stale fallback connection. Opening another
    /// library requires constructing another explicit RepositoryClient.
    pub async fn reconnect(&self) -> Result<RepositoryIdentity, ClientError> {
        let mut state = self.shared.state.lock().await;
        state.client.take();
        let mut next = match self.shared.mode {
            RepositoryMode::Fixture => ServiceClient::connect(&self.shared.root).await?,
            RepositoryMode::Managed => ServiceClient::connect_managed(&self.shared.root).await?,
            RepositoryMode::Installed => {
                ServiceClient::connect_or_start_managed_pinned(
                    &self.shared.root,
                    self.shared.library_id,
                )
                .await?
            }
        };
        if next.descriptor.library_id != self.shared.library_id {
            return Err(resync());
        }
        let bootstrap = next.desktop_bootstrap().await?;
        if bootstrap.library_id != self.shared.library_id
            || bootstrap.data.cursor_scope != self.shared.cursor_scope
        {
            return Err(resync());
        }
        state.identity = RepositoryIdentity {
            library_id: bootstrap.library_id,
            library_epoch: bootstrap.library_epoch,
            cursor_scope: bootstrap.data.cursor_scope,
        };
        state.client = Some(next);
        Ok(state.identity.clone())
    }
    pub async fn bootstrap(&self) -> Result<RepositorySnapshot<DesktopBootstrap>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let result = client(&mut state)?.desktop_bootstrap().await?;
        if result.data.cursor_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(snapshot(&state, result.data))
    }
    pub async fn preferences(&self) -> Result<RepositorySnapshot<DesktopPreferences>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?.desktop_preferences().await?.data;
        Ok(snapshot(&state, data))
    }
    pub async fn update_preferences(
        &self,
        request: UpdateDesktopPreferencesRequest,
    ) -> Result<RepositorySnapshot<DesktopMutationOutcome>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?
            .update_desktop_preferences(request)
            .await?
            .data;
        Ok(snapshot(&state, data))
    }
    pub async fn meeting(
        &self,
        meeting_id: String,
    ) -> Result<RepositorySnapshot<Option<DesktopMeeting>>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?
            .desktop_meeting(DesktopMeetingLookup { meeting_id })
            .await?
            .data;
        Ok(snapshot(&state, data))
    }
    pub async fn meeting_decorations(
        &self,
        meeting_id: String,
    ) -> Result<RepositorySnapshot<DesktopMeetingDecorations>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?
            .desktop_meeting_decorations(DesktopMeetingLookup { meeting_id })
            .await?
            .data;
        Ok(snapshot(&state, data))
    }
    pub async fn rename_meeting(
        &self,
        request: RenameDesktopMeetingRequest,
    ) -> Result<RepositorySnapshot<DesktopMutationOutcome>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?
            .rename_desktop_meeting(request)
            .await?
            .data;
        Ok(snapshot(&state, data))
    }
    pub async fn delete_meeting(
        &self,
        request: DeleteDesktopMeetingRequest,
    ) -> Result<RepositorySnapshot<DesktopMutationOutcome>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?
            .delete_desktop_meeting(request)
            .await?
            .data;
        Ok(snapshot(&state, data))
    }
    pub async fn meetings(
        &self,
        filter: DesktopMeetingFilter,
        cursor: Option<MeetingContinuation>,
        limit: u16,
        max_bytes: u32,
    ) -> Result<RepositoryMeetingPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.library_id != state.identity.library_id
                || cursor.library_epoch != state.identity.library_epoch
                || cursor.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let page = client(&mut state)?
            .desktop_meetings(DesktopMeetingRequest {
                filter,
                cursor: cursor.map(|cursor| cursor.cursor),
                limit,
                max_bytes,
            })
            .await?
            .data;
        if page
            .next
            .as_ref()
            .is_some_and(|cursor| cursor.library_scope != self.shared.cursor_scope)
        {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositoryMeetingPage {
            identity: state.identity.clone(),
            items: page.items,
            next: page.next.map(|cursor| MeetingContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor,
            }),
            snapshot_sequence: page.snapshot_sequence,
        })
    }
    pub async fn changes(
        &self,
        cursor: Option<ChangeContinuation>,
        limit: u16,
        max_bytes: u32,
    ) -> Result<RepositoryChangePage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.library_id != state.identity.library_id
                || cursor.library_epoch != state.identity.library_epoch
                || cursor.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let page = client(&mut state)?
            .desktop_changes(DesktopChangesRequest {
                cursor: cursor.map(|cursor| cursor.cursor),
                limit,
                max_bytes,
            })
            .await?
            .data;
        if page.next.library_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositoryChangePage {
            identity: state.identity.clone(),
            items: page.items,
            next: ChangeContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor: page.next,
            },
            has_more: page.has_more,
            current_sequence: page.current_sequence,
        })
    }
}

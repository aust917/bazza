//! One bounded organization page per call. Continuations are bound to this
//! facade's canonical library, persistent scope and current service epoch.
use super::*;
use bazza_service_api::library::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: DesktopLibraryCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryLibraryPage {
    pub identity: RepositoryIdentity,
    pub items: Vec<DesktopLibraryItem>,
    pub next: Option<LibraryContinuation>,
    pub snapshot_sequence: i64,
}

impl RepositoryClient {
    pub async fn library_page(
        &self,
        cursor: Option<LibraryContinuation>,
        limit: u16,
        max_bytes: u32,
    ) -> Result<RepositoryLibraryPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.library_id != state.identity.library_id
                || cursor.library_epoch != state.identity.library_epoch
                || cursor.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let data = client(&mut state)?
            .desktop_library(DesktopLibraryRequest::List {
                cursor: cursor.map(|cursor| cursor.cursor),
                limit,
                max_bytes,
            })
            .await?
            .data;
        let DesktopLibraryReply::Page(page) = data else {
            state.client.take();
            return Err(ClientError::Protocol);
        };
        if page.library_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositoryLibraryPage {
            identity: state.identity.clone(),
            items: page.items,
            next: page.next.map(|cursor| LibraryContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor,
            }),
            snapshot_sequence: page.snapshot_sequence,
        })
    }

    pub async fn library_mutate(
        &self,
        request: DesktopLibraryRequest,
    ) -> Result<RepositorySnapshot<DesktopLibraryMutationReply>, ClientError> {
        if request.operation().is_none() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?.desktop_library(request).await?.data;
        let DesktopLibraryReply::Mutation(reply) = data else {
            state.client.take();
            return Err(ClientError::Protocol);
        };
        Ok(snapshot(&state, reply))
    }
}

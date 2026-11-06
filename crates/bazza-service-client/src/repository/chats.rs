//! One bounded chat read at a time. Continuations retain the original query,
//! exact issued cursor and fragment boundary; they never collect source text.
use super::{client, resync, RepositoryClient, RepositoryIdentity};
use crate::ClientError;
use bazza_service_api::{
    chats::*,
    rpc::{ErrorCode, RpcError},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryChatsContinuation {
    identity: RepositoryIdentity,
    initial: DesktopChatsRequest,
    request: DesktopChatsRequest,
    boundary: Boundary,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Boundary {
    List(DesktopChatListCursor),
    Messages(DesktopChatMessagesCursor),
    ChatField {
        cursor: DesktopChatFieldCursor,
        previous_byte_offset: u64,
        fragment_bytes: u64,
        total_bytes: u64,
        present: bool,
    },
    MessageField {
        cursor: DesktopChatMessageFieldCursor,
        previous_byte_offset: u64,
        fragment_bytes: u64,
        total_bytes: u64,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryChatsPage {
    pub identity: RepositoryIdentity,
    pub data: DesktopChatsReply,
    pub next: Option<RepositoryChatsContinuation>,
}

impl std::fmt::Debug for RepositoryChatsContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryChatsContinuation")
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for RepositoryChatsPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryChatsPage")
            .field("identity", &self.identity)
            .field("has_next", &self.next.is_some())
            .finish_non_exhaustive()
    }
}

fn has_cursor(request: &DesktopChatsRequest) -> bool {
    match request {
        DesktopChatsRequest::List { cursor, .. } => cursor.is_some(),
        DesktopChatsRequest::Messages { cursor, .. } => cursor.is_some(),
        DesktopChatsRequest::ReadChatField { cursor, .. } => cursor.is_some(),
        DesktopChatsRequest::ReadMessageField { cursor, .. } => cursor.is_some(),
        DesktopChatsRequest::Describe { .. } => false,
    }
}

impl Boundary {
    fn scope(&self) -> &str {
        match self {
            Self::List(cursor) => &cursor.library_scope,
            Self::Messages(cursor) => &cursor.library_scope,
            Self::ChatField { cursor, .. } => &cursor.library_scope,
            Self::MessageField { cursor, .. } => &cursor.library_scope,
        }
    }

    fn apply(&self, initial: &DesktopChatsRequest) -> Option<DesktopChatsRequest> {
        if has_cursor(initial) || !initial.valid() {
            return None;
        }
        let mut request = initial.clone();
        match (&mut request, self) {
            (DesktopChatsRequest::List { cursor, .. }, Self::List(next)) => {
                *cursor = Some(next.clone());
            }
            (DesktopChatsRequest::Messages { cursor, .. }, Self::Messages(next)) => {
                *cursor = Some(next.clone());
            }
            (
                DesktopChatsRequest::ReadChatField { cursor, .. },
                Self::ChatField {
                    cursor: next,
                    previous_byte_offset,
                    fragment_bytes,
                    total_bytes,
                    present,
                },
            ) if *fragment_bytes != 0
                && previous_byte_offset.checked_add(*fragment_bytes) == Some(next.byte_offset)
                && next.total_bytes == *total_bytes
                && next.present == *present =>
            {
                *cursor = Some(next.clone());
            }
            (
                DesktopChatsRequest::ReadMessageField { cursor, .. },
                Self::MessageField {
                    cursor: next,
                    previous_byte_offset,
                    fragment_bytes,
                    total_bytes,
                },
            ) if *fragment_bytes != 0
                && previous_byte_offset.checked_add(*fragment_bytes) == Some(next.byte_offset)
                && next.total_bytes == *total_bytes =>
            {
                *cursor = Some(next.clone());
            }
            _ => return None,
        }
        request.valid().then_some(request)
    }

    fn from_reply(reply: &DesktopChatsReply) -> Option<Self> {
        match reply {
            DesktopChatsReply::List(page) => page.next.clone().map(Self::List),
            DesktopChatsReply::Messages(page) => page.next.clone().map(Self::Messages),
            DesktopChatsReply::ChatField(page) => page.next.clone().map(|cursor| Self::ChatField {
                cursor,
                previous_byte_offset: page.byte_offset,
                fragment_bytes: page.text.len() as u64,
                total_bytes: page.total_bytes,
                present: page.present,
            }),
            DesktopChatsReply::MessageField(page) => {
                page.next.clone().map(|cursor| Self::MessageField {
                    cursor,
                    previous_byte_offset: page.byte_offset,
                    fragment_bytes: page.text.len() as u64,
                    total_bytes: page.total_bytes,
                })
            }
            DesktopChatsReply::Header(_) => None,
        }
    }
}

fn reply_scope(reply: &DesktopChatsReply) -> Option<&str> {
    match reply {
        DesktopChatsReply::List(page) => Some(&page.library_scope),
        DesktopChatsReply::Messages(page) => Some(&page.library_scope),
        DesktopChatsReply::ChatField(page) => Some(&page.library_scope),
        DesktopChatsReply::MessageField(page) => Some(&page.library_scope),
        DesktopChatsReply::Header(_) => None,
    }
}

impl RepositoryClient {
    /// Starts one of the five closed read operations. A raw wire cursor is not
    /// accepted here: advance a returned opaque continuation explicitly instead.
    pub async fn chats(
        &self,
        request: DesktopChatsRequest,
    ) -> Result<RepositoryChatsPage, ClientError> {
        if has_cursor(&request) {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        self.read_chats(request.clone(), request, None).await
    }

    /// Performs one more bounded request against the same root, library, epoch,
    /// scope, query and document pins. This never retries or rebases a stale read.
    pub async fn continue_chats(
        &self,
        continuation: RepositoryChatsContinuation,
    ) -> Result<RepositoryChatsPage, ClientError> {
        let expected = continuation.boundary.apply(&continuation.initial);
        if expected.as_ref() != Some(&continuation.request) {
            return Err(resync());
        }
        self.read_chats(
            continuation.initial,
            continuation.request,
            Some((continuation.identity, continuation.boundary)),
        )
        .await
    }

    async fn read_chats(
        &self,
        initial: DesktopChatsRequest,
        request: DesktopChatsRequest,
        continuation: Option<(RepositoryIdentity, Boundary)>,
    ) -> Result<RepositoryChatsPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if continuation.as_ref().is_some_and(|(identity, boundary)| {
            identity != &state.identity || boundary.scope() != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let data = client(&mut state)?.desktop_chats(request).await?.data;
        if reply_scope(&data).is_some_and(|scope| scope != self.shared.cursor_scope) {
            state.client.take();
            return Err(resync());
        }
        let next = match Boundary::from_reply(&data) {
            Some(boundary) => {
                let Some(request) = boundary.apply(&initial) else {
                    state.client.take();
                    return Err(ClientError::Protocol);
                };
                Some(RepositoryChatsContinuation {
                    identity: state.identity.clone(),
                    initial,
                    request,
                    boundary,
                })
            }
            None => None,
        };
        Ok(RepositoryChatsPage {
            identity: state.identity.clone(),
            data,
            next,
        })
    }
}

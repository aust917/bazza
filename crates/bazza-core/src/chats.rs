//! Bounded private chat history reads. Generation/revision checks remain in the
//! same store snapshot as each page; native sending and token events are separate.
use crate::{desktop, CoreError, Repository};
use bazza_service_api::{
    chats::{DesktopChatsReply, DesktopChatsRequest},
    desktop::DesktopEnvelope,
};
use bazza_store::service_data as data;

impl Repository {
    pub fn desktop_chats(
        &self,
        peer_pid: u32,
        request: DesktopChatsRequest,
    ) -> Result<DesktopEnvelope<DesktopChatsReply>, CoreError> {
        desktop::authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop chats request").into());
        }
        let stored_request = serde_json::from_value(serde_json::to_value(&request)?)?;
        let result = self.store.service_desktop_chats(&stored_request)?;
        let result: DesktopChatsReply = serde_json::from_value(serde_json::to_value(result)?)?;
        if !result.valid_for(&request) {
            return Err(data::DataError::Invalid("desktop chats reply").into());
        }
        self.desktop_envelope(result)
    }
}

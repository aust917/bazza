//! Typed private desktop calls. Invalid replies poison the connection before a
//! caller can accidentally continue a mixed library/page stream.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    agent_summary::CommitDesktopAgentSummaryRequest,
    desktop::*,
    rpc::{ErrorCode, Reply, Request, RpcError},
};
use serde::Serialize;
use std::collections::HashSet;

fn bad_request() -> ClientError {
    RpcError::new(ErrorCode::InvalidArgument).into()
}
fn mutation_valid(value: &DesktopMutationOutcome, entity: &str) -> bool {
    value.entity_id == entity
        && desktop_revision(&value.revision)
        && value.change_sequence > 0
        && value.job_id.is_none()
}
impl ServiceClient {
    pub(crate) fn desktop_envelope_valid<T: Serialize>(&self, value: &DesktopEnvelope<T>) -> bool {
        value.library_id == self.descriptor.library_id
            && value.library_epoch == self.descriptor.library_epoch
            && desktop_body_fits(value, MAX_DESKTOP_REPLY_BYTES)
    }
    pub async fn desktop_bootstrap(
        &mut self,
    ) -> Result<DesktopEnvelope<DesktopBootstrap>, ClientError> {
        match self.call(Request::DesktopBootstrap).await? {
            Reply::DesktopBootstrap(value)
                if self.desktop_envelope_valid(&value) && value.data.valid() =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn desktop_preferences(
        &mut self,
    ) -> Result<DesktopEnvelope<DesktopPreferences>, ClientError> {
        match self.call(Request::DesktopPreferences).await? {
            Reply::DesktopPreferences(value)
                if self.desktop_envelope_valid(&value) && value.data.valid() =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn update_desktop_preferences(
        &mut self,
        request: UpdateDesktopPreferencesRequest,
    ) -> Result<DesktopEnvelope<DesktopMutationOutcome>, ClientError> {
        if !request.valid() {
            return Err(bad_request());
        }
        match self
            .call(Request::UpdateDesktopPreferences(request))
            .await?
        {
            Reply::DesktopMutation(value)
                if self.desktop_envelope_valid(&value)
                    && mutation_valid(&value.data, "desktop_preferences") =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn desktop_meetings(
        &mut self,
        request: DesktopMeetingRequest,
    ) -> Result<DesktopEnvelope<DesktopMeetingPage>, ClientError> {
        if !request.valid() {
            return Err(bad_request());
        }
        let original = request.clone();
        match self.call(Request::DesktopMeetings(request)).await? {
            Reply::DesktopMeetings(value)
                if self.desktop_envelope_valid(&value)
                    && meeting_page_valid(&value.data, &original) =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn desktop_meeting(
        &mut self,
        request: DesktopMeetingLookup,
    ) -> Result<DesktopEnvelope<Option<DesktopMeeting>>, ClientError> {
        if !desktop_id(&request.meeting_id) {
            return Err(bad_request());
        }
        let id = request.meeting_id.clone();
        match self.call(Request::DesktopMeeting(request)).await? {
            Reply::DesktopMeeting(value)
                if self.desktop_envelope_valid(&value)
                    && value
                        .data
                        .as_ref()
                        .is_none_or(|meeting| meeting.id == id && meeting.valid()) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn desktop_meeting_decorations(
        &mut self,
        request: DesktopMeetingLookup,
    ) -> Result<DesktopEnvelope<DesktopMeetingDecorations>, ClientError> {
        if !desktop_id(&request.meeting_id) {
            return Err(bad_request());
        }
        let id = request.meeting_id.clone();
        match self
            .call(Request::DesktopMeetingDecorations(request))
            .await?
        {
            Reply::DesktopMeetingDecorations(value)
                if self.desktop_envelope_valid(&value) && value.data.valid(&id) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn commit_desktop_agent_summary(
        &mut self,
        request: CommitDesktopAgentSummaryRequest,
    ) -> Result<DesktopEnvelope<DesktopMutationOutcome>, ClientError> {
        if !request.valid() {
            return Err(bad_request());
        }
        if request.library_id != self.descriptor.library_id
            || request.library_epoch != self.descriptor.library_epoch
        {
            return Err(RpcError::new(ErrorCode::ResyncRequired).into());
        }
        match self.call(Request::CommitDesktopAgentSummary(Box::new(request.clone()))).await? {
            Reply::DesktopMutation(value)
                if self.desktop_envelope_valid(&value) && request.accepts(&value.data) =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn desktop_changes(
        &mut self,
        request: DesktopChangesRequest,
    ) -> Result<DesktopEnvelope<DesktopChangePage>, ClientError> {
        if !request.valid() {
            return Err(bad_request());
        }
        let original = request.clone();
        match self.call(Request::DesktopChanges(request)).await? {
            Reply::DesktopChanges(value)
                if self.desktop_envelope_valid(&value)
                    && change_page_valid(&value.data, &original) =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn rename_desktop_meeting(
        &mut self,
        request: RenameDesktopMeetingRequest,
    ) -> Result<DesktopEnvelope<DesktopMutationOutcome>, ClientError> {
        if !request.valid() {
            return Err(bad_request());
        }
        let id = request.meeting_id.clone();
        match self.call(Request::RenameDesktopMeeting(request)).await? {
            Reply::DesktopMutation(value)
                if self.desktop_envelope_valid(&value) && mutation_valid(&value.data, &id) =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }

    pub async fn delete_desktop_meeting(
        &mut self,
        request: DeleteDesktopMeetingRequest,
    ) -> Result<DesktopEnvelope<DesktopMutationOutcome>, ClientError> {
        if !request.valid() {
            return Err(bad_request());
        }
        match self
            .call(Request::DeleteDesktopMeeting(request.clone()))
            .await?
        {
            Reply::DesktopMutation(value)
                if self.desktop_envelope_valid(&value) && request.accepts(&value.data) =>
            {
                Ok(value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

fn meeting_page_valid(page: &DesktopMeetingPage, request: &DesktopMeetingRequest) -> bool {
    if page.items.len() > request.limit as usize
        || page.snapshot_sequence < 0
        || !desktop_body_fits(page, request.max_bytes as usize)
    {
        return false;
    }
    let mut ids = HashSet::new();
    if page.items.iter().any(|meeting| {
        !meeting.valid()
            || !ids.insert(&meeting.id)
            || request
                .filter
                .content_kind
                .as_ref()
                .is_some_and(|kind| &meeting.content_kind != kind)
            || (matches!(request.filter.folder, DesktopFolderFilter::Unfiled)
                && meeting.folder_id.is_some())
    }) {
        return false;
    }
    let offset = request.cursor.as_ref().map_or(0, |cursor| cursor.offset);
    if request
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.snapshot_sequence != page.snapshot_sequence)
    {
        return false;
    }
    if let Some(next) = &page.next {
        if page.items.is_empty()
            || !next.valid()
            || next.snapshot_sequence != page.snapshot_sequence
            || next.offset != offset + page.items.len() as u64
            || request.cursor.as_ref().is_some_and(|cursor| {
                cursor.library_scope != next.library_scope || cursor.query_hash != next.query_hash
            })
        {
            return false;
        }
    }
    true
}
fn change_page_valid(page: &DesktopChangePage, request: &DesktopChangesRequest) -> bool {
    if page.items.len() > request.limit as usize
        || page.current_sequence < 0
        || !desktop_body_fits(page, request.max_bytes as usize)
        || !desktop_scope(&page.next.library_scope)
        || page.next.after < 0
        || page.next.after > page.current_sequence
    {
        return false;
    }
    let mut after = request.cursor.as_ref().map_or(0, |cursor| cursor.after);
    if request
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.library_scope != page.next.library_scope)
    {
        return false;
    }
    for item in &page.items {
        if after.checked_add(1) != Some(item.sequence)
            || item.sequence > page.current_sequence
            || !desktop_id(&item.entity_kind)
            || !desktop_change_id(&item.entity_kind, &item.entity_id)
            || !desktop_revision(&item.revision)
            || item.actor.is_empty()
            || item.actor.len() > 128
            || item.actor.contains('\0')
        {
            return false;
        }
        after = item.sequence;
    }
    page.next.after == after
        && page.has_more == (after < page.current_sequence)
        && (!page.has_more || !page.items.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_changes_keep_bounded_unicode_sql_identity_without_relaxing_meeting_ids() {
        let request = DesktopChangesRequest::default();
        let mut page = DesktopChangePage {
            items: vec![DesktopChange {
                sequence: 1,
                entity_kind: "template".into(),
                entity_id: format!("  Клиент/{}\" ", "🙂".repeat(40)),
                revision: "revision-1".into(),
                actor: "desktop".into(),
            }],
            next: DesktopChangeCursor {
                library_scope: "a".repeat(32),
                after: 1,
            },
            has_more: false,
            current_sequence: 1,
        };
        assert!(change_page_valid(&page, &request));
        page.items[0].entity_kind = "meeting".into();
        assert!(!change_page_valid(&page, &request));
        page.items[0].entity_kind = "template".into();
        page.items[0].entity_id.push('\0');
        assert!(!change_page_valid(&page, &request));
        page.items[0].entity_id = "🙂".repeat(129);
        assert!(!change_page_valid(&page, &request));
    }
    #[test]
    fn change_pages_reject_gaps_stalls_and_foreign_scope() {
        let scope = "a".repeat(32);
        let request = DesktopChangesRequest {
            cursor: Some(DesktopChangeCursor {
                library_scope: scope.clone(),
                after: 4,
            }),
            ..Default::default()
        };
        let mut page = DesktopChangePage {
            items: vec![DesktopChange {
                sequence: 5,
                entity_kind: "meeting".into(),
                entity_id: "meeting-id".into(),
                revision: "r5".into(),
                actor: "desktop".into(),
            }],
            next: DesktopChangeCursor {
                library_scope: scope,
                after: 5,
            },
            has_more: false,
            current_sequence: 5,
        };
        assert!(change_page_valid(&page, &request));
        page.items[0].sequence = 6;
        assert!(!change_page_valid(&page, &request));
        page.items[0].sequence = 5;
        page.next.library_scope = "b".repeat(32);
        assert!(!change_page_valid(&page, &request));
        page.next.library_scope = "a".repeat(32);
        page.items.clear();
        page.has_more = true;
        assert!(!change_page_valid(&page, &request));
    }
}

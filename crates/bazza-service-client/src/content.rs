//! Bounded private transcript and summary reads, without document assembly or
//! direct storage access. Malformed content replies poison the connection.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    content::*,
    desktop::{desktop_body_fits, desktop_scope, DesktopEnvelope},
    rpc::{ErrorCode, Reply, Request, RpcError},
};
use std::collections::HashSet;

impl ServiceClient {
    pub async fn desktop_transcript(
        &mut self,
        request: DesktopTranscriptRequest,
    ) -> Result<DesktopEnvelope<DesktopTranscriptPage>, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self.call(Request::DesktopTranscript(request)).await? {
            Reply::DesktopTranscript(value)
                if self.desktop_envelope_valid(&value)
                    && transcript_page_valid(&value.data, &original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
    pub async fn desktop_summary(
        &mut self,
        request: DesktopSummaryRequest,
    ) -> Result<DesktopEnvelope<DesktopSummaryPage>, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self.call(Request::DesktopSummary(request)).await? {
            Reply::DesktopSummary(value)
                if self.desktop_envelope_valid(&value)
                    && summary_page_valid(&value.data, &original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}

fn same_segment(a: &DesktopTranscriptFragment, b: &DesktopTranscriptFragment) -> bool {
    a.segment_id == b.segment_id
        && a.start_ms == b.start_ms
        && a.end_ms == b.end_ms
        && a.speaker == b.speaker
        && a.total_bytes == b.total_bytes
        && a.has_clean_text == b.has_clean_text
        && a.clean_text_manual == b.clean_text_manual
        && a.effective_origin == b.effective_origin
}

pub(crate) fn transcript_page_valid(
    page: &DesktopTranscriptPage,
    request: &DesktopTranscriptRequest,
) -> bool {
    if !page.state.valid()
        || !desktop_scope(&page.library_scope)
        || page.state.meeting_id != request.meeting_id
        || page.state.revision != request.expected_revision
        || page.representation != request.representation
        || page.source_id.as_ref()
            != request
                .source_id
                .as_ref()
                .or(page.state.current_source_id.as_ref())
        || page.items.len() > request.limit as usize
        || !desktop_body_fits(page, request.max_bytes as usize)
        || request.cursor.as_ref().is_some_and(|cursor| {
            cursor.library_scope != page.library_scope
                || cursor.current_source_id != page.state.current_source_id
                || cursor.source_id != page.source_id
        })
        || (page.source_id.is_none() && (!page.items.is_empty() || page.next.is_some()))
        || (request.cursor.is_some() && page.items.is_empty())
    {
        return false;
    }
    let (mut index, mut offset) = request
        .cursor
        .as_ref()
        .map_or((0, 0), |cursor| (cursor.segment_index, cursor.byte_offset));
    let mut previous: Option<&DesktopTranscriptFragment> = None;
    let mut segment_ids = HashSet::new();
    for fragment in &page.items {
        if !fragment.valid() || fragment.segment_index != index || fragment.byte_offset != offset {
            return false;
        }
        if let Some(previous) = previous {
            if previous.segment_index == fragment.segment_index {
                if !same_segment(previous, fragment) {
                    return false;
                }
            } else if !segment_ids.insert(fragment.segment_id) {
                return false;
            }
        } else {
            segment_ids.insert(fragment.segment_id);
        }
        let Some(end) = fragment.end_offset() else {
            return false;
        };
        if end == fragment.total_bytes {
            let Some(next_index) = index
                .checked_add(1)
                .filter(|next| *next <= MAX_CONTENT_NUMBER)
            else {
                return false;
            };
            index = next_index;
            offset = 0;
        } else {
            offset = end;
        }
        previous = Some(fragment);
    }
    match &page.next {
        Some(next) => {
            !page.items.is_empty()
                && next.valid()
                && next.library_scope == page.library_scope
                && next.meeting_id == request.meeting_id
                && next.revision == request.expected_revision
                && next.current_source_id == page.state.current_source_id
                && next.source_id == page.source_id
                && next.representation == page.representation
                && next.segment_index == index
                && next.byte_offset == offset
                && request
                    .cursor
                    .as_ref()
                    .is_none_or(|cursor| cursor.query_hash == next.query_hash)
        }
        None => offset == 0,
    }
}

fn summary_page_valid(page: &DesktopSummaryPage, request: &DesktopSummaryRequest) -> bool {
    if !page.state.valid()
        || !desktop_scope(&page.library_scope)
        || page.state.meeting_id != request.meeting_id
        || page.state.revision != request.expected_revision
        || page.field != request.field
        || page.total_bytes > MAX_CONTENT_NUMBER
        || page.text.len() > 65_536
        || page.byte_offset
            != request
                .cursor
                .as_ref()
                .map_or(0, |cursor| cursor.byte_offset)
        || !desktop_body_fits(page, request.max_bytes as usize)
        || request
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.library_scope != page.library_scope)
    {
        return false;
    }
    let Some(end) = page.byte_offset.checked_add(page.text.len() as u64) else {
        return false;
    };
    if end > page.total_bytes
        || (page.total_bytes > 0 && (page.text.is_empty() || page.byte_offset >= page.total_bytes))
        || (page.state.document_state != DesktopDocumentState::Present
            && (page.total_bytes != 0 || page.byte_offset != 0 || !page.text.is_empty()))
    {
        return false;
    }
    match &page.next {
        Some(next) => {
            end < page.total_bytes
                && next.valid()
                && next.library_scope == page.library_scope
                && next.meeting_id == request.meeting_id
                && next.revision == request.expected_revision
                && next.field == page.field
                && next.byte_offset == end
                && request
                    .cursor
                    .as_ref()
                    .is_none_or(|cursor| cursor.query_hash == next.query_hash)
        }
        None => end == page.total_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_continuations_reject_changed_query_field_offset_and_stalls() {
        let cursor = DesktopSummaryCursor {
            library_scope: "a".repeat(32),
            meeting_id: "legacy-meeting".into(),
            revision: "s1".into(),
            field: DesktopSummaryField::Markdown,
            byte_offset: 3,
            query_hash: "b".repeat(64),
        };
        let request = DesktopSummaryRequest {
            meeting_id: "legacy-meeting".into(),
            expected_revision: "s1".into(),
            field: DesktopSummaryField::Markdown,
            cursor: Some(cursor.clone()),
            max_bytes: 4096,
        };
        let mut next = cursor;
        next.byte_offset = 6;
        let mut page = DesktopSummaryPage {
            library_scope: "a".repeat(32),
            state: DesktopSummaryState {
                meeting_id: "legacy-meeting".into(),
                revision: "s1".into(),
                document_state: DesktopDocumentState::Present,
                deletion_epoch: 0,
                automatic_generation_suppressed: false,
                origin: "manual".into(),
                auto_replace: false,
                source_label: None,
                source_revision: None,
                source_missing: false,
            },
            field: DesktopSummaryField::Markdown,
            text: "文".into(),
            byte_offset: 3,
            total_bytes: 9,
            next: Some(next),
        };
        assert!(summary_page_valid(&page, &request));
        page.next.as_mut().unwrap().query_hash = "c".repeat(64);
        assert!(!summary_page_valid(&page, &request));
        page.next.as_mut().unwrap().query_hash = "b".repeat(64);
        page.next.as_mut().unwrap().field = DesktopSummaryField::StructuredJson;
        assert!(!summary_page_valid(&page, &request));
        page.next.as_mut().unwrap().field = DesktopSummaryField::Markdown;
        page.next.as_mut().unwrap().byte_offset = 3;
        assert!(!summary_page_valid(&page, &request));
        page.next.as_mut().unwrap().byte_offset = 6;
        page.text.clear();
        assert!(!summary_page_valid(&page, &request));
    }
}

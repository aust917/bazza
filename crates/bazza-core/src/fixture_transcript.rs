//! Projection from a single bounded Store snapshot into the public transcript
//! schema. The fixture service retains exclusive ownership during the read.
use crate::{fixture_read::FixtureReadError, Repository};
use bazza_service_api::{
    desktop::desktop_body_fits,
    fixture_read::FixtureReadReply,
    inputs::GetTranscriptInput,
    outputs::{
        SegmentContinuation, SegmentView, TextPage, TranscriptPage, TranscriptSourceMetadata,
    },
    types::*,
};
use bazza_store::service_data::{
    fitting_prefix, DataError, DataResult, DocumentState as StoredDocumentState,
    FixtureTranscriptRepresentation as StoredRepresentation, FixtureTranscriptSnapshot,
    FixtureTranscriptSource,
};
use uuid::Uuid;

// Reserve space for the fixed Success + private FixtureReadReply wrappers.
const DATA_BUDGET: usize = FixtureReadReply::MAX_ENCODED_BYTES - 256;

impl Repository {
    pub(super) fn fixture_transcript(
        &self,
        input: GetTranscriptInput,
    ) -> Result<TranscriptPage, FixtureReadError> {
        let had_cursor = input.cursor.is_some();
        let result = self.store.with_fixture_transcript(
            &input.meeting_id.0,
            input.source_id.as_ref().map(|source| source.0.as_str()),
            |snapshot| read_page(&snapshot, self.epoch, &input),
        );
        match result {
            Err(DataError::NotFound) if had_cursor => Err(FixtureReadError::Stale),
            Err(DataError::Invalid("transcript byte budget too small")) => {
                Err(FixtureReadError::Invalid)
            }
            result => Ok(result?),
        }
    }
}

fn source_metadata(source: &FixtureTranscriptSource) -> DataResult<TranscriptSourceMetadata> {
    let origin = match source.origin.as_str() {
        "legacy" => TranscriptOrigin::Legacy,
        "imported" => TranscriptOrigin::Imported,
        "external_agent" => TranscriptOrigin::ExternalAgent,
        "captured" | "local_asr" => TranscriptOrigin::Captured,
        _ => return Err(DataError::Invalid("transcript source origin")),
    };
    Ok(TranscriptSourceMetadata {
        source_id: EntityId(source.id.clone()),
        origin,
        source_label: source.source_label.clone().map(Text),
        provider_label: source.provider_label.clone().map(Text),
        model_label: source.model_label.clone().map(Text),
        language: source.language.clone().map(Language),
        created_at: Timestamp(source.created_at.to_rfc3339()),
    })
}

fn cursor_position(
    cursor: Option<&Cursor>,
    snapshot: &FixtureTranscriptSnapshot<'_>,
    epoch: Uuid,
    representation: StoredRepresentation,
) -> DataResult<(u64, u64)> {
    let Some(Cursor(cursor)) = cursor else {
        return Ok((0, 0));
    };
    let mut parts = cursor.split('.');
    let (Some("v1"), Some("transcript"), Some(index), Some(offset), Some(tag), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(DataError::StaleCursor);
    };
    let parse = |text: &str| {
        let value: u64 = text.parse().map_err(|_| DataError::StaleCursor)?;
        if value.to_string() != text || value > 9_007_199_254_740_991 {
            return Err(DataError::StaleCursor);
        }
        Ok(value)
    };
    let (index, offset) = (parse(index)?, parse(offset)?);
    if snapshot
        .source
        .as_ref()
        .is_none_or(|source| index >= source.segment_count)
        || (index == 0 && offset == 0)
        || tag != snapshot.cursor_tag(epoch, representation, index, offset)
    {
        return Err(DataError::StaleCursor);
    }
    Ok((index, offset))
}

fn read_page(
    snapshot: &FixtureTranscriptSnapshot<'_>,
    epoch: Uuid,
    input: &GetTranscriptInput,
) -> DataResult<TranscriptPage> {
    let representation = match input.representation {
        Representation::Raw => StoredRepresentation::Raw,
        Representation::Effective => StoredRepresentation::Effective,
        Representation::Both => StoredRepresentation::Both,
    };
    let (mut index, mut offset) =
        cursor_position(input.cursor.as_ref(), snapshot, epoch, representation)?;
    let next_at = |index, offset| {
        let tag = snapshot.cursor_tag(epoch, representation, index, offset);
        Cursor(format!("v1.transcript.{index}.{offset}.{tag}"))
    };
    let state = match snapshot.state.document_state {
        StoredDocumentState::Absent => DocumentState::Absent,
        StoredDocumentState::Present => DocumentState::Present,
        StoredDocumentState::Deleted => DocumentState::Deleted,
    };
    let source = snapshot.source.as_ref().map(source_metadata).transpose()?;
    let mut page = TranscriptPage {
        meeting_id: input.meeting_id.clone(),
        state,
        revision: Revision(snapshot.state.revision.clone()),
        current_source_id: snapshot.state.current_source_id.clone().map(EntityId),
        source_id: source.as_ref().map(|source| source.source_id.clone()),
        source,
        representation: input.representation,
        segments: Vec::new(),
        continuation: None,
        page: TextPage {
            next_cursor: None,
            truncated: false,
            returned_utf8_bytes: 0,
        },
    };
    let Some(selected) = &snapshot.source else {
        return Ok(page);
    };
    if state != DocumentState::Present {
        return Err(DataError::Invalid(
            "transcript source exists for unavailable document",
        ));
    }
    let count = selected.segment_count;
    while index < count && page.segments.len() < usize::from(input.limit.0) {
        let used = page.page.returned_utf8_bytes;
        let remaining = (input.max_bytes.0 - used) as usize;
        let window = snapshot.window(representation, index, offset, remaining)?;
        let segment_id = EntityId(window.id.to_string());
        page.segments.push(SegmentView {
            segment_id: segment_id.clone(),
            start_ms: Millis(window.start as u64),
            end_ms: Millis(window.end as u64),
            speaker: window.speaker.map(Text),
            raw: None,
            effective: None,
            clean_text_manual: window.manual,
            text_complete: false,
        });
        let mut candidate = |len| {
            let raw_len = window.raw_prefix_bytes.min(len);
            let (raw, effective) = window.text[..len].split_at(raw_len);
            let segment = page.segments.last_mut().expect("candidate segment");
            segment.raw =
                (input.representation != Representation::Effective).then(|| Text(raw.into()));
            segment.effective =
                (input.representation != Representation::Raw).then(|| Text(effective.into()));
            let end = offset + len as u64;
            segment.text_complete = end == window.total_bytes;
            page.continuation = (!segment.text_complete).then(|| SegmentContinuation {
                segment_id: segment_id.clone(),
                offset_utf8_bytes: end,
                cursor: next_at(index, end),
            });
            page.page.next_cursor = page
                .continuation
                .as_ref()
                .map(|c| c.cursor.clone())
                .or_else(|| (index + 1 < count).then(|| next_at(index + 1, 0)));
            page.page.truncated = page.page.next_cursor.is_some();
            page.page.returned_utf8_bytes = used + len as u32;
            desktop_body_fits(&page, DATA_BUDGET)
        };
        let len = fitting_prefix(&window.text, &mut candidate);
        let complete = offset + len as u64 == window.total_bytes;
        if !candidate(len) || (len == 0 && !complete) {
            page.segments.pop();
            if page.segments.is_empty() {
                return Err(DataError::Invalid("transcript byte budget too small"));
            }
            page.continuation = None;
            page.page.next_cursor = Some(next_at(index, offset));
            page.page.truncated = true;
            page.page.returned_utf8_bytes = used;
            break;
        }
        if !complete {
            break;
        }
        index += 1;
        offset = 0;
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_api::{outputs::Success, validate_tool_output_for_input};
    use bazza_service_test_support::{create_v8, ORPHAN_MEETING_ID, READY_MEETING_ID};
    use bazza_store::service_data::{
        MutationContext, TranscriptCorrection, TranscriptCorrections, TranscriptImportOrigin,
        TranscriptWrite,
    };
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, Repository) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("synthetic transcript");
        create_v8(&root).unwrap();
        let repository = Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap();
        (directory, repository)
    }

    fn read(repository: &Repository, input: &GetTranscriptInput) -> TranscriptPage {
        let data = repository.fixture_transcript(input.clone()).unwrap();
        let output = FixtureReadReply {
            output: serde_json::to_value(Success {
                schema_version: 1,
                library_id: LibraryId(repository.library_id.to_string()),
                data: &data,
                warnings: Vec::new(),
            })
            .unwrap(),
        };
        validate_tool_output_for_input(
            "bazza_get_transcript",
            &serde_json::to_value(input).unwrap(),
            &output.output,
        )
        .unwrap();
        assert!(desktop_body_fits(
            &output,
            FixtureReadReply::MAX_ENCODED_BYTES
        ));
        data
    }

    #[test]
    fn partial_both_is_exact_and_mode_or_epoch_changes_reject_cursor() {
        let (_directory, repository) = fixture();
        let mut input: GetTranscriptInput = serde_json::from_value(json!({
            "meeting_id": READY_MEETING_ID, "representation": "both",
            "limit": 1, "max_bytes": 3
        }))
        .unwrap();
        let first = read(&repository, &input);
        assert_eq!(first.segments[0].raw, Some(Text("э".into())));
        assert_eq!(first.segments[0].effective, Some(Text(String::new())));
        assert_eq!(first.continuation.unwrap().offset_utf8_bytes, 2);
        input.cursor = first.page.next_cursor;
        let mut changed = input.clone();
        changed.representation = Representation::Raw;
        assert!(matches!(
            repository.fixture_transcript(changed),
            Err(FixtureReadError::Data(DataError::StaleCursor))
        ));
        let state = repository
            .data()
            .transcript_state(READY_MEETING_ID)
            .unwrap();
        repository
            .store
            .with_fixture_transcript(READY_MEETING_ID, None, |snapshot| {
                assert!(matches!(
                    read_page(&snapshot, Uuid::new_v4(), &input),
                    Err(DataError::StaleCursor)
                ));
                Ok(())
            })
            .unwrap();
        input.max_bytes = PageBytes(65_536);
        input.limit = PageLimit(100);
        let rest = read(&repository, &input);
        assert_eq!(rest.segments[0].raw, Some(Text("э проект Орион".into())));
        assert_eq!(
            rest.segments[0].effective,
            Some(Text("ээ проект Орион".into()))
        );
        assert_eq!(rest.segments[2].effective, Some(Text(String::new())));
        assert!(rest.segments[2].clean_text_manual);
        assert!(rest.page.next_cursor.is_none());
        assert_eq!(
            repository
                .data()
                .transcript_state(READY_MEETING_ID)
                .unwrap(),
            state
        );
    }

    #[test]
    fn archived_overlay_spans_wire_pages_without_rewriting_raw() {
        let (_directory, repository) = fixture();
        let data = repository.data();
        let original = data.transcript_state(READY_MEETING_ID).unwrap();
        let old_source = original.current_source_id.unwrap();
        let overlay = "🦀\u{1}\"\\\n".repeat(6_000);
        let correction = data
            .correct_transcript(
                &MutationContext {
                    principal: "fixture".into(),
                    idempotency_key: "large-overlay".into(),
                },
                READY_MEETING_ID,
                &TranscriptCorrections {
                    expected_revision: original.revision,
                    corrections: vec![TranscriptCorrection {
                        segment_id: 101,
                        text: Some(overlay.clone()),
                    }],
                },
            )
            .unwrap();
        let mut input: GetTranscriptInput = serde_json::from_value(json!({
            "meeting_id": READY_MEETING_ID, "representation": "both", "limit": 1
        }))
        .unwrap();
        let old_current_cursor = read(&repository, &input).page.next_cursor.unwrap();
        data.replace_transcript(
            &MutationContext {
                principal: "fixture".into(),
                idempotency_key: "replace".into(),
            },
            READY_MEETING_ID,
            &TranscriptWrite {
                expected_revision: correction.revision,
                segments: vec![bazza_store::NewSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    speaker: None,
                    text: "replacement".into(),
                }],
                source_label: Some("fixture replacement".into()),
                language: Some("ru-RU".into()),
                origin: TranscriptImportOrigin::Imported,
            },
        )
        .unwrap();
        input.cursor = Some(old_current_cursor);
        assert!(matches!(
            repository.fixture_transcript(input.clone()),
            Err(FixtureReadError::Data(DataError::StaleCursor))
        ));
        input.source_id = Some(EntityId(old_source.clone()));
        input.cursor = None;
        let mut raw = String::new();
        let mut effective = String::new();
        let mut pages = 0;
        loop {
            let page = read(&repository, &input);
            let source = page.source.unwrap();
            assert_eq!(source.origin, TranscriptOrigin::Legacy);
            assert_eq!(source.source_id.0, old_source);
            assert_ne!(page.current_source_id, page.source_id);
            let item = &page.segments[0];
            assert_eq!(item.segment_id.0, "101");
            raw.push_str(&item.raw.as_ref().unwrap().0);
            effective.push_str(&item.effective.as_ref().unwrap().0);
            pages += 1;
            assert!(pages < 10);
            if item.text_complete {
                break;
            }
            assert_eq!(
                page.continuation.unwrap().offset_utf8_bytes,
                (raw.len() + effective.len()) as u64
            );
            input.cursor = page.page.next_cursor;
        }
        assert!(pages > 1);
        assert_eq!(raw, "ээ проект Орион");
        assert_eq!(effective, overlay);

        let absent: GetTranscriptInput = serde_json::from_value(json!({
            "meeting_id": ORPHAN_MEETING_ID
        }))
        .unwrap();
        let absent = read(&repository, &absent);
        assert_eq!(absent.state, DocumentState::Absent);
        assert!(absent.source_id.is_none() && absent.segments.is_empty());
    }
}

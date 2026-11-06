//! Public fixture summary projection from one bounded Store read transaction.
use crate::{fixture_read::FixtureReadError, Repository};
use bazza_service_api::{
    desktop::desktop_body_fits,
    fixture_read::FixtureReadReply,
    inputs::GetSummaryInput,
    outputs::{ActorKind, DocumentProvenance, SummaryPage, TextPage},
    types::{Cursor, DocumentState, Revision, SummaryFormat, SummaryOrigin, Text, Timestamp},
};
use bazza_store::service_data::{
    fitting_prefix, DataError, DataResult, DesktopSummaryField,
    DocumentState as StoredDocumentState, FixtureSummarySnapshot,
};
use uuid::Uuid;

// Space for the success envelope and private reply wrappers.
const DATA_BUDGET: usize = FixtureReadReply::MAX_ENCODED_BYTES - 256;

impl Repository {
    pub(super) fn fixture_summary(
        &self,
        input: GetSummaryInput,
    ) -> Result<SummaryPage, FixtureReadError> {
        let result = self
            .store
            .with_fixture_summary(&input.meeting_id.0, |snapshot| {
                read_page(&snapshot, self.epoch, &input)
            });
        match result {
            Err(DataError::NotFound) if input.cursor.is_some() => Err(FixtureReadError::Stale),
            Err(DataError::Invalid("summary byte budget" | "summary byte budget too small")) => {
                Err(FixtureReadError::Invalid)
            }
            result => Ok(result?),
        }
    }
}

fn cursor_offset(
    snapshot: &FixtureSummarySnapshot<'_>,
    epoch: Uuid,
    field: DesktopSummaryField,
    cursor: Option<&Cursor>,
) -> DataResult<u64> {
    let Some(Cursor(cursor)) = cursor else {
        return Ok(0);
    };
    let mut parts = cursor.split('.');
    let (Some("v1"), Some("summary"), Some(raw), Some(tag), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(DataError::StaleCursor);
    };
    let offset = raw.parse::<u64>().map_err(|_| DataError::StaleCursor)?;
    if snapshot.document_state != StoredDocumentState::Present
        || !(1..=9_007_199_254_740_991).contains(&offset)
        || offset.to_string() != raw
        || tag != snapshot.cursor_tag(epoch, field, offset)
    {
        return Err(DataError::StaleCursor);
    }
    Ok(offset)
}

fn provenance(
    snapshot: &FixtureSummarySnapshot<'_>,
) -> DataResult<(Option<SummaryOrigin>, Option<DocumentProvenance>)> {
    let Some(published_at) = snapshot.published_at else {
        return Ok((None, None));
    };
    let (origin, actor_kind) = match snapshot.origin.as_str() {
        "legacy" => (SummaryOrigin::Legacy, ActorKind::Legacy),
        "imported" => (SummaryOrigin::Imported, ActorKind::Imported),
        "manual" => (SummaryOrigin::Manual, ActorKind::Human),
        "external_agent" => (SummaryOrigin::ExternalAgent, ActorKind::ExternalAgent),
        "configured_generation" => (SummaryOrigin::ConfiguredGeneration, ActorKind::Service),
        _ => return Err(DataError::Invalid("summary origin")),
    };
    // A summary body has one persisted publication timestamp per version.
    let timestamp = Timestamp(published_at.to_rfc3339());
    Ok((
        Some(origin),
        Some(DocumentProvenance {
            actor_kind,
            actor_label: None,
            source_label: snapshot.source_label.clone().map(Text),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }),
    ))
}

fn read_page(
    snapshot: &FixtureSummarySnapshot<'_>,
    epoch: Uuid,
    input: &GetSummaryInput,
) -> DataResult<SummaryPage> {
    let field = match input.format {
        SummaryFormat::Markdown => DesktopSummaryField::Markdown,
        SummaryFormat::StructuredJson => DesktopSummaryField::StructuredJson,
    };
    if !(1..=65_536).contains(&input.max_bytes.0) {
        return Err(DataError::Invalid("summary byte budget"));
    }
    let offset = cursor_offset(snapshot, epoch, field, input.cursor.as_ref())?;
    let (origin, provenance) = provenance(snapshot)?;
    let state = match snapshot.document_state {
        StoredDocumentState::Absent => DocumentState::Absent,
        StoredDocumentState::Present => DocumentState::Present,
        StoredDocumentState::Deleted => DocumentState::Deleted,
    };
    let mut page = SummaryPage {
        meeting_id: input.meeting_id.clone(),
        state,
        revision: Revision(snapshot.revision.clone()),
        origin,
        auto_replace: snapshot.auto_replace,
        automatic_generation_suppressed: snapshot.automatic_generation_suppressed,
        source_missing: snapshot.source_missing,
        provenance,
        format: input.format,
        content: None,
        page: TextPage {
            next_cursor: None,
            truncated: false,
            returned_utf8_bytes: 0,
        },
    };
    if state != DocumentState::Present {
        return Ok(page);
    }
    let window = snapshot.window(field, offset, input.max_bytes.0)?;
    let mut candidate = |length| {
        let end = offset + length as u64;
        page.content = Some(Text(window.text[..length].into()));
        page.page.next_cursor = (end < window.total_bytes).then(|| {
            let tag = snapshot.cursor_tag(epoch, field, end);
            Cursor(format!("v1.summary.{end}.{tag}"))
        });
        page.page.truncated = page.page.next_cursor.is_some();
        page.page.returned_utf8_bytes = length as u32;
        desktop_body_fits(&page, DATA_BUDGET)
    };
    let length = fitting_prefix(&window.text, &mut candidate);
    if !candidate(length) || (length == 0 && offset < window.total_bytes) {
        return Err(DataError::Invalid("summary byte budget too small"));
    }
    if window.text[..length].contains('\0') {
        return Err(DataError::Invalid("summary content"));
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_api::{
        outputs::Success,
        types::{EntityId, LibraryId, PageBytes},
        validate_tool_output_for_input,
    };
    use bazza_service_test_support::{
        create_v8, DAMAGED_MEETING_ID, ORPHAN_MEETING_ID, READY_MEETING_ID,
    };
    use bazza_store::service_data::{
        MutationContext, SummaryAuthor, SummaryWrite, SummaryWriteMode,
    };
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, Repository) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("synthetic summary");
        create_v8(&root).unwrap();
        let repository = Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap();
        (directory, repository)
    }

    fn input(meeting_id: &str) -> GetSummaryInput {
        GetSummaryInput {
            meeting_id: EntityId(meeting_id.into()),
            format: SummaryFormat::Markdown,
            cursor: None,
            max_bytes: PageBytes(65_536),
        }
    }

    fn read(repository: &Repository, input: &GetSummaryInput) -> SummaryPage {
        let data = repository.fixture_summary(input.clone()).unwrap();
        let reply = FixtureReadReply {
            output: serde_json::to_value(Success {
                schema_version: 1,
                library_id: LibraryId(repository.library_id.to_string()),
                data: &data,
                warnings: Vec::new(),
            })
            .unwrap(),
        };
        validate_tool_output_for_input(
            "bazza_get_summary",
            &serde_json::to_value(input).unwrap(),
            &reply.output,
        )
        .unwrap();
        assert!(desktop_body_fits(
            &reply,
            FixtureReadReply::MAX_ENCODED_BYTES
        ));
        data
    }

    fn context(key: &str) -> MutationContext {
        MutationContext {
            principal: "fixture".into(),
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn summary_distinguishes_legacy_absent_present_empty_and_deleted() {
        let (_directory, repository) = fixture();
        let mut request = input(READY_MEETING_ID);
        request.format = SummaryFormat::StructuredJson;
        let legacy = read(&repository, &request);
        assert_eq!(
            legacy.content,
            Some(Text(r#"{"fixture_origin":"manual","budget":14}"#.into()))
        );
        assert_eq!(legacy.origin, Some(SummaryOrigin::Legacy));
        let provenance = legacy.provenance.unwrap();
        assert!(matches!(provenance.actor_kind, ActorKind::Legacy));
        assert_eq!(provenance.created_at.0, "2026-11-02T00:00:00+00:00");
        assert_eq!(provenance.created_at, provenance.updated_at);

        let absent = read(&repository, &input(ORPHAN_MEETING_ID));
        assert_eq!(absent.state, DocumentState::Absent);
        assert!(absent.origin.is_none() && absent.provenance.is_none() && absent.content.is_none());

        let written = repository
            .data()
            .write_summary(
                &context("empty-summary"),
                READY_MEETING_ID,
                &SummaryWrite {
                    expected_revision: legacy.revision.0,
                    mode: SummaryWriteMode::Replace,
                    markdown: String::new(),
                    structured_json: json!({}),
                    source_label: Some("fixture source".into()),
                    author: SummaryAuthor::ExternalAgent,
                },
            )
            .unwrap();
        request.format = SummaryFormat::Markdown;
        let present = read(&repository, &request);
        assert_eq!(present.state, DocumentState::Present);
        assert_eq!(present.content, Some(Text(String::new())));
        assert_eq!(present.page.returned_utf8_bytes, 0);
        assert_eq!(present.origin, Some(SummaryOrigin::ExternalAgent));
        assert!(
            !present.auto_replace
                && !present.automatic_generation_suppressed
                && !present.source_missing
        );
        let provenance = present.provenance.unwrap();
        assert!(matches!(provenance.actor_kind, ActorKind::ExternalAgent));
        assert_eq!(provenance.source_label, Some(Text("fixture source".into())));

        let deleted = repository
            .data()
            .delete_summary(
                &context("delete-summary"),
                READY_MEETING_ID,
                &written.revision,
            )
            .unwrap();
        let page = read(&repository, &request);
        assert_eq!(page.state, DocumentState::Deleted);
        assert_eq!(page.revision.0, deleted.revision);
        assert!(page.automatic_generation_suppressed);
        assert!(page.origin.is_none() && page.provenance.is_none() && page.content.is_none());
    }

    #[test]
    fn summary_pages_round_trip_escaped_utf8_and_pin_cursor() {
        let (_directory, repository) = fixture();
        let mut request = input(READY_MEETING_ID);
        let revision = read(&repository, &request).revision;
        let body = "🦀\u{1}\"\\\n".repeat(6_000);
        let written = repository
            .data()
            .write_summary(
                &context("large-summary"),
                READY_MEETING_ID,
                &SummaryWrite {
                    expected_revision: revision.0,
                    mode: SummaryWriteMode::Replace,
                    markdown: body.clone(),
                    structured_json: json!({}),
                    source_label: None,
                    author: SummaryAuthor::Manual,
                },
            )
            .unwrap();
        request.max_bytes = PageBytes(1);
        assert!(matches!(
            repository.fixture_summary(request.clone()),
            Err(FixtureReadError::Invalid)
        ));
        request.max_bytes = PageBytes(65_536);
        let first = read(&repository, &request);
        assert!(first.page.truncated);
        request.cursor = first.page.next_cursor;
        for changed in [
            GetSummaryInput {
                format: SummaryFormat::StructuredJson,
                ..request.clone()
            },
            GetSummaryInput {
                meeting_id: EntityId(DAMAGED_MEETING_ID.into()),
                ..request.clone()
            },
        ] {
            assert!(matches!(
                repository.fixture_summary(changed),
                Err(FixtureReadError::Data(DataError::StaleCursor))
            ));
        }
        repository
            .store
            .with_fixture_summary(READY_MEETING_ID, |snapshot| {
                assert!(matches!(
                    read_page(&snapshot, Uuid::new_v4(), &request),
                    Err(DataError::StaleCursor)
                ));
                Ok(())
            })
            .unwrap();
        let rest = read(&repository, &request);
        assert!(!rest.page.truncated);
        let first = first.content.unwrap().0;
        let rest = rest.content.unwrap().0;
        assert_eq!(first + &rest, body);

        repository
            .data()
            .delete_summary(
                &context("delete-paged-summary"),
                READY_MEETING_ID,
                &written.revision,
            )
            .unwrap();
        assert!(matches!(
            repository.fixture_summary(request),
            Err(FixtureReadError::Data(DataError::StaleCursor))
        ));
    }
}

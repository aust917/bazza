//! Public fixture note metadata and bounded Markdown pages.
use crate::{
    fixture_read::{cursor_in, page, FixtureReadError},
    Repository,
};
use bazza_service_api::{
    desktop::desktop_body_fits,
    fixture_read::FixtureReadReply,
    inputs::{GetNoteInput, ListNotesInput},
    outputs::{ActorKind, DocumentProvenance, NoteMetadata, NotePage, NotesPage, TextPage},
    types::{Cursor, EntityId, NoteScope, Revision, Text, Timestamp},
};
use bazza_store::service_data::{
    fitting_prefix, DataError, DataResult, FixtureNoteHeader, FixtureNoteScope, FixtureNoteSnapshot,
};
use uuid::Uuid;

// The list projection adds provenance fields to up to 100 Store headers.
const LIST_BUDGET: u32 = (FixtureReadReply::MAX_ENCODED_BYTES - 10_240) as u32;
// Reserve space for the success envelope and private reply wrapper.
const NOTE_BUDGET: usize = FixtureReadReply::MAX_ENCODED_BYTES - 256;

impl Repository {
    pub(super) fn fixture_notes(
        &self,
        input: ListNotesInput,
    ) -> Result<NotesPage, FixtureReadError> {
        let scope = match &input.scope {
            NoteScope::All => FixtureNoteScope::All,
            NoteScope::Standalone => FixtureNoteScope::Standalone,
            NoteScope::Meeting { meeting_id } => FixtureNoteScope::Meeting(&meeting_id.0),
        };
        let cursor = cursor_in(self.epoch, "notes", input.cursor)?;
        let snapshot =
            self.store
                .fixture_notes(scope, cursor.as_ref(), input.limit.0, LIST_BUDGET)?;
        Ok(NotesPage {
            items: snapshot
                .items
                .iter()
                .map(metadata)
                .collect::<DataResult<_>>()?,
            page: page(self.epoch, "notes", snapshot.next),
        })
    }

    pub(super) fn fixture_note(&self, input: GetNoteInput) -> Result<NotePage, FixtureReadError> {
        if !(1..=65_536).contains(&input.max_bytes.0) {
            return Err(FixtureReadError::Invalid);
        }
        let result = self.store.with_fixture_note(&input.note_id.0, |snapshot| {
            read_page(&snapshot, self.epoch, &input)
        });
        match result {
            Err(DataError::NotFound) if input.cursor.is_some() => Err(FixtureReadError::Stale),
            Err(DataError::Invalid("note byte budget too small")) => Err(FixtureReadError::Invalid),
            result => Ok(result?),
        }
    }
}

fn metadata(header: &FixtureNoteHeader) -> DataResult<NoteMetadata> {
    let actor_kind = match header.origin.as_str() {
        "external_agent" => ActorKind::ExternalAgent,
        _ => return Err(DataError::Invalid("note origin")),
    };
    Ok(NoteMetadata {
        note_id: EntityId(header.id.clone()),
        meeting_id: header.meeting_id.clone().map(EntityId),
        title: Text(header.title.clone()),
        revision: Revision(header.revision.clone()),
        provenance: DocumentProvenance {
            actor_kind,
            actor_label: None,
            source_label: None,
            created_at: Timestamp(header.created_at.to_rfc3339()),
            updated_at: Timestamp(header.updated_at.to_rfc3339()),
        },
    })
}

fn cursor_offset(
    cursor: Option<&Cursor>,
    snapshot: &FixtureNoteSnapshot<'_>,
    epoch: Uuid,
) -> DataResult<u64> {
    let Some(Cursor(cursor)) = cursor else {
        return Ok(0);
    };
    let mut parts = cursor.split('.');
    let (Some("v1"), Some("note"), Some(raw), Some(tag), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(DataError::StaleCursor);
    };
    let offset = raw.parse::<u64>().map_err(|_| DataError::StaleCursor)?;
    if !(1..=9_007_199_254_740_991).contains(&offset)
        || offset.to_string() != raw
        || tag != snapshot.cursor_tag(epoch, offset)
    {
        return Err(DataError::StaleCursor);
    }
    Ok(offset)
}

fn read_page(
    snapshot: &FixtureNoteSnapshot<'_>,
    epoch: Uuid,
    input: &GetNoteInput,
) -> DataResult<NotePage> {
    let offset = cursor_offset(input.cursor.as_ref(), snapshot, epoch)?;
    let window = snapshot.window(offset, input.max_bytes.0)?;
    let mut page = NotePage {
        note: metadata(&snapshot.header)?,
        content: Text(String::new()),
        page: TextPage {
            next_cursor: None,
            truncated: false,
            returned_utf8_bytes: 0,
        },
    };
    let mut candidate = |length| {
        let end = offset + length as u64;
        page.content = Text(window.text[..length].into());
        page.page.next_cursor = (end < window.total_bytes).then(|| {
            let tag = snapshot.cursor_tag(epoch, end);
            Cursor(format!("v1.note.{end}.{tag}"))
        });
        page.page.truncated = page.page.next_cursor.is_some();
        page.page.returned_utf8_bytes = length as u32;
        desktop_body_fits(&page, NOTE_BUDGET)
    };
    let length = fitting_prefix(&window.text, &mut candidate);
    if !candidate(length) || (length == 0 && offset < window.total_bytes) {
        return Err(DataError::Invalid("note byte budget too small"));
    }
    if page.content.0.contains('\0') {
        return Err(DataError::Invalid("note content"));
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        data::{MutationContext, NewNote, NoteUpdate},
        MigrationPolicy,
    };
    use bazza_service_api::{
        outputs::Success,
        types::{LibraryId, PageBytes, PageLimit},
        validate_tool_output_for_input,
    };
    use bazza_service_test_support::{create_v8, READY_MEETING_ID};

    fn repository() -> (tempfile::TempDir, Repository) {
        let temp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temp.path().join("fixture notes")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        (temp, repo)
    }

    fn context(key: &str) -> MutationContext {
        MutationContext {
            principal: "fixture-note-test-private".into(),
            idempotency_key: key.into(),
        }
    }

    fn create(repo: &Repository, key: &str, markdown: &str, meeting_id: Option<&str>) -> String {
        repo.data()
            .create_note(
                &context(key),
                &NewNote {
                    title: key.into(),
                    markdown: markdown.into(),
                    meeting_id: meeting_id.map(str::to_owned),
                },
            )
            .unwrap()
            .entity_id
    }

    #[test]
    fn list_filters_before_paging_and_binds_the_query_to_its_snapshot() {
        let (_temp, repo) = repository();
        let mut standalone_ids = [
            create(&repo, "standalone-1", "One", None),
            create(&repo, "standalone-2", "Two", None),
        ];
        standalone_ids.sort();
        let attached_id = create(&repo, "attached", "Three", Some(READY_MEETING_ID));
        let mut request = ListNotesInput {
            scope: NoteScope::Standalone,
            limit: PageLimit(1),
            cursor: None,
        };
        let first = repo.fixture_notes(request.clone()).unwrap();
        assert_eq!(first.items[0].note_id.0, standalone_ids[0]);
        assert!(first.items[0].meeting_id.is_none());
        request.cursor = first.page.next_cursor.clone();
        assert!(request.cursor.is_some());
        let second = repo.fixture_notes(request.clone()).unwrap();
        assert_eq!(second.items[0].note_id.0, standalone_ids[1]);
        assert!(second.page.next_cursor.is_none());
        let attached = repo
            .fixture_notes(ListNotesInput {
                scope: NoteScope::Meeting {
                    meeting_id: EntityId(READY_MEETING_ID.into()),
                },
                limit: PageLimit(1),
                cursor: None,
            })
            .unwrap();
        assert_eq!(attached.items[0].note_id.0, attached_id);
        assert_eq!(
            attached.items[0].meeting_id.as_ref().unwrap().0,
            READY_MEETING_ID
        );
        assert!(attached.page.next_cursor.is_none());

        for (scope, limit) in [(NoteScope::All, 1), (NoteScope::Standalone, 2)] {
            assert!(matches!(
                repo.fixture_notes(ListNotesInput {
                    scope,
                    limit: PageLimit(limit),
                    cursor: request.cursor.clone(),
                }),
                Err(FixtureReadError::Data(DataError::StaleCursor))
            ));
        }
        assert_eq!(
            repo.fixture_notes(request.clone()).unwrap().items[0]
                .note_id
                .0,
            standalone_ids[1]
        );
        create(&repo, "standalone-3", "Four", None);
        assert!(matches!(
            repo.fixture_notes(request),
            Err(FixtureReadError::Data(DataError::StaleCursor))
        ));
    }

    fn checked_note(repo: &Repository, input: &GetNoteInput) -> NotePage {
        let data = repo.fixture_note(input.clone()).unwrap();
        let reply = FixtureReadReply {
            output: serde_json::to_value(Success {
                schema_version: 1,
                library_id: LibraryId(repo.library_id.to_string()),
                data: &data,
                warnings: Vec::new(),
            })
            .unwrap(),
        };
        validate_tool_output_for_input(
            "bazza_get_note",
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

    #[test]
    fn get_preserves_utf8_across_byte_cuts_and_rejects_stale_note_cursors() {
        let (_temp, repo) = repository();
        let markdown = "🧪я\u{1}\r\n".repeat(8_000);
        let id = create(&repo, "body", &markdown, Some(READY_MEETING_ID));
        let other_id = create(&repo, "empty", "", None);
        let mut input = GetNoteInput {
            note_id: EntityId(id.clone()),
            max_bytes: PageBytes(1),
            cursor: None,
        };
        assert!(matches!(
            repo.fixture_note(input.clone()),
            Err(FixtureReadError::Invalid)
        ));
        input.max_bytes = PageBytes(5);
        let first = checked_note(&repo, &input);
        assert_eq!(first.content.0, "🧪");
        let revision = first.note.revision.0;
        assert!(matches!(
            first.note.provenance.actor_kind,
            ActorKind::ExternalAgent
        ));
        assert!(first.note.provenance.actor_label.is_none());
        assert_eq!(
            first.note.provenance.created_at,
            first.note.provenance.updated_at
        );
        input.cursor = first.page.next_cursor;
        assert!(input.cursor.is_some());
        let old_cursor = input.cursor.clone();

        // Change the byte budget on continuation. The large escaped Markdown
        // still needs more pages than its plain UTF-8 length would suggest.
        input.max_bytes = PageBytes(65_536);
        let mut collected = first.content.0;
        for _ in 0..8 {
            let next = checked_note(&repo, &input);
            assert_eq!(next.page.returned_utf8_bytes as usize, next.content.0.len());
            assert!(!next.content.0.is_empty());
            collected.push_str(&next.content.0);
            input.cursor = next.page.next_cursor;
            if input.cursor.is_none() {
                break;
            }
        }
        assert_eq!(collected, markdown);
        assert!(input.cursor.is_none());

        input.note_id = EntityId(other_id);
        let empty = checked_note(&repo, &input);
        assert_eq!(empty.content.0, "");
        assert!(empty.page.next_cursor.is_none());
        input.cursor = old_cursor;
        assert!(matches!(
            repo.fixture_note(input.clone()),
            Err(FixtureReadError::Data(DataError::StaleCursor))
        ));

        input.note_id = EntityId(id.clone());
        let updated = repo
            .data()
            .update_note(
                &context("update-note"),
                &id,
                &revision,
                &NoteUpdate {
                    title: Some("changed".into()),
                    ..NoteUpdate::default()
                },
            )
            .unwrap();
        assert!(matches!(
            repo.fixture_note(input.clone()),
            Err(FixtureReadError::Data(DataError::StaleCursor))
        ));
        repo.data()
            .delete_note(&context("delete-note"), &id, &updated.revision)
            .unwrap();
        assert!(matches!(
            repo.fixture_note(input.clone()),
            Err(FixtureReadError::Stale)
        ));
        input.cursor = None;
        assert!(matches!(
            repo.fixture_note(input),
            Err(FixtureReadError::Data(DataError::NotFound))
        ));
    }
}

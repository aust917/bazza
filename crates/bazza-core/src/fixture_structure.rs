//! Public folder and tag discovery over the fixture library snapshot.
use crate::{
    fixture_read::{cursor_in, page, FixtureReadError},
    Repository,
};
use bazza_service_api::{
    fixture_read::FixtureReadReply,
    inputs::GetLibraryStructureInput,
    outputs::{FolderView, LibraryStructurePage, TagView},
    types::{EntityId, Revision, StructureKind, Text},
};
use bazza_store::service_data::{
    DesktopLibraryCursor, DesktopLibraryItem, DesktopLibraryPageRequest, FixtureLibraryKind,
    SearchCursor,
};

const CURSOR_KIND: &str = "structure";
// Keep room for the public result envelope and encoded continuation token.
const PAGE_BUDGET: u32 = (FixtureReadReply::MAX_ENCODED_BYTES - 2048) as u32;

impl Repository {
    pub(super) fn fixture_structure(
        &self,
        input: GetLibraryStructureInput,
    ) -> Result<LibraryStructurePage, FixtureReadError> {
        let kind = match input.kind {
            StructureKind::All => FixtureLibraryKind::All,
            StructureKind::Folders => FixtureLibraryKind::Folders,
            StructureKind::Tags => FixtureLibraryKind::Tags,
        };
        let cursor =
            cursor_in(self.epoch, CURSOR_KIND, input.cursor)?.map(|c| DesktopLibraryCursor {
                library_scope: c.library_scope,
                snapshot_sequence: c.generation,
                query_hash: c.query_hash,
                offset: c.offset as u64,
            });
        let snapshot = self.store.fixture_read_library_structure(
            kind,
            &DesktopLibraryPageRequest {
                cursor,
                limit: input.limit.0,
                max_bytes: PAGE_BUDGET,
            },
        )?;
        let mut folders = Vec::new();
        let mut tags = Vec::new();
        for item in snapshot.items {
            match item {
                DesktopLibraryItem::Folder {
                    id,
                    name,
                    parent_id,
                    default_template_id,
                    revision,
                    ..
                } => folders.push(FolderView {
                    folder_id: EntityId(id),
                    name: Text(name),
                    parent_id: parent_id.map(EntityId),
                    default_template_id: default_template_id.map(EntityId),
                    revision: Revision(revision),
                }),
                DesktopLibraryItem::Tag {
                    id, name, revision, ..
                } => tags.push(TagView {
                    tag_id: EntityId(id),
                    name: Text(name),
                    revision: Revision(revision),
                }),
            }
        }
        let next = snapshot.next.map(|c| SearchCursor {
            library_scope: c.library_scope,
            generation: c.snapshot_sequence,
            query_hash: c.query_hash,
            offset: c.offset as usize,
        });
        Ok(LibraryStructurePage {
            folders,
            tags,
            page: page(self.epoch, CURSOR_KIND, next),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_api::fixture_read::{FixtureReadRequest, FixtureReadTool};
    use bazza_service_api::library::DesktopLibraryRequest;
    use bazza_service_test_support::create_v8;
    use serde_json::{json, Value};

    fn read(repo: &Repository, arguments: Value) -> Result<LibraryStructurePage, String> {
        let request = FixtureReadRequest {
            tool: FixtureReadTool::Structure,
            arguments,
        };
        let reply = repo.fixture_read(request.clone());
        assert!(reply.valid_for(&request, repo.library_id));
        match reply.output.get("data") {
            Some(data) => Ok(serde_json::from_value(data.clone()).unwrap()),
            None => Err(reply.output["error"]["code"].as_str().unwrap().to_owned()),
        }
    }

    fn tag_create(repo: &Repository, name: &str) {
        repo.desktop_library(
            std::process::id(),
            DesktopLibraryRequest::TagCreate {
                name: name.into(),
                idempotency_key: format!("structure-test-{name}"),
            },
        )
        .unwrap();
    }

    #[test]
    fn fixture_structure_filters_before_paging_and_pins_kind_limit_and_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temp.path().join("fixture")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        tag_create(&repo, "AAA");

        let all = read(&repo, json!({"limit":1})).unwrap();
        assert_eq!(all.folders[0].folder_id.0, "fixture-folder");
        assert!(!all.folders[0].revision.0.is_empty());
        assert!(all.tags.is_empty());
        let all_next = read(&repo, json!({"limit":1,"cursor":all.page.next_cursor})).unwrap();
        assert!(all_next.folders.is_empty());
        assert_eq!(all_next.tags[0].name.0, "AAA");

        let folders = read(&repo, json!({"kind":"folders","limit":1})).unwrap();
        assert_eq!(folders.folders[0].folder_id.0, "fixture-folder");
        assert!(folders.tags.is_empty());
        assert!(folders.page.next_cursor.is_none());

        let request = json!({"kind":"tags","limit":1});
        let tags = read(&repo, request.clone()).unwrap();
        assert!(tags.folders.is_empty());
        assert_eq!(tags.tags[0].name.0, "AAA");
        let cursor = tags.page.next_cursor.unwrap();
        let continuation = json!({"kind":"tags","limit":1,"cursor":cursor});
        let tags_next = read(&repo, continuation.clone()).unwrap();
        assert!(tags_next.folders.is_empty());
        assert_eq!(tags_next.tags[0].tag_id.0, "fixture-tag");
        assert!(!tags_next.tags[0].revision.0.is_empty());
        assert!(tags_next.page.next_cursor.is_none());
        assert_eq!(
            read(&repo, request).unwrap().page.next_cursor,
            Some(cursor.clone())
        );
        for changed in [
            json!({"limit":1,"cursor":cursor}),
            json!({"kind":"tags","limit":2,"cursor":cursor}),
        ] {
            assert_eq!(read(&repo, changed).unwrap_err(), "CURSOR_STALE");
        }

        tag_create(&repo, "ZZZ");
        assert_eq!(read(&repo, continuation).unwrap_err(), "CURSOR_STALE");
    }

    #[test]
    fn fixture_structure_preserves_the_full_desktop_tag_name() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temp.path().join("fixture")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let name = "T".repeat(512);
        repo.desktop_library(
            std::process::id(),
            DesktopLibraryRequest::TagCreate {
                name: name.clone(),
                idempotency_key: "structure-full-name".into(),
            },
        )
        .unwrap();
        let tags = read(&repo, json!({"kind":"tags"})).unwrap().tags;
        assert!(tags.iter().any(|tag| tag.name.0 == name));
    }
}

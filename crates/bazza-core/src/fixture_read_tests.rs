use super::*;
use crate::{
    data::{MutationContext, NewNote, NewTextMeeting},
    MigrationPolicy,
};
use bazza_service_api::library::DesktopLibraryRequest;
use bazza_service_test_support::{create_v8, READY_MEETING_ID};
use serde_json::json;
use std::collections::BTreeSet;

fn repository() -> (tempfile::TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    let fixture = create_v8(&directory.path().join("fixture")).unwrap();
    let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
    (directory, repository)
}

fn mutation(key: &str) -> MutationContext {
    MutationContext {
        principal: "fixture-test".into(),
        idempotency_key: key.into(),
    }
}

fn invoke(repo: &Repository, tool: FixtureReadTool, arguments: Value) -> Value {
    let request = FixtureReadRequest { tool, arguments };
    let reply = repo.fixture_read(request.clone());
    assert!(reply.valid_for(&request, repo.library_id));
    reply.output
}

#[test]
fn fixture_filters_exact_instants_and_stales_a_changed_query() {
    let (_directory, repo) = repository();
    let mut expected = Vec::new();
    for (index, date) in [
        "2026-12-20T00:00:00.000000001Z",
        "2026-12-20T00:00:00.000000003Z",
        "2026-12-20T00:00:00.000000007Z",
    ]
    .into_iter()
    .enumerate()
    {
        let outcome = repo
            .data()
            .create_meeting(
                &mutation(&format!("time-{index}")),
                &NewTextMeeting {
                    title: "nanosecondfixture timing".into(),
                    started_at: date.parse().unwrap(),
                    participants: vec!["Мария".into()],
                    folder_id: Some("fixture-folder".into()),
                    tag_ids: vec!["fixture-tag".into()],
                    calendar_uid: None,
                    transcript: None,
                },
            )
            .unwrap();
        expected.push(outcome.entity_id);
    }
    let filter = json!({
        "from":"2026-12-20T05:00:00.000000002+05:00",
        "to":"2026-12-20T05:00:00.000000007+05:00",
        "content_kind":"text", "participants":["Мария"],
        "folder_ids":["fixture-folder"], "tag_ids":["fixture-tag"]
    });
    let page = invoke(
        &repo,
        FixtureReadTool::ListMeetings,
        json!({"filter":filter}),
    );
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"]["items"][0]["meeting_id"], expected[1]);
    assert_eq!(page["data"]["items"][0]["tag_ids"], json!(["fixture-tag"]));
    let hit = invoke(
        &repo,
        FixtureReadTool::Search,
        json!({"query":"nanosecondfixture","filter":filter}),
    );
    assert_eq!(hit["data"]["hits"].as_array().unwrap().len(), 1);
    assert_eq!(hit["data"]["hits"][0]["meeting_id"], expected[1]);

    for tool in [FixtureReadTool::ListMeetings, FixtureReadTool::Search] {
        let mut query = match tool {
            FixtureReadTool::Search => json!({"query":"nanosecondfixture","limit":1}),
            _ => json!({"limit":1}),
        };
        let first = invoke(&repo, tool, query.clone());
        query["cursor"] = first["data"]["page"]["next_cursor"].clone();
        assert!(query["cursor"].is_string());
        query["filter"] = filter.clone();
        assert_eq!(invoke(&repo, tool, query)["error"]["code"], "CURSOR_STALE");
    }
}

#[test]
fn fixture_search_byte_cut_keeps_the_last_source_reachable() {
    let (_directory, repo) = repository();
    let mut expected = BTreeSet::new();
    let paragraph = format!(" {}", "q".repeat(190)).repeat(25);
    for index in 0..40 {
        let note = repo
            .data()
            .create_note(
                &mutation(&format!("snippet-{index}")),
                &NewNote {
                    title: format!("{index} test note"),
                    markdown: format!("excerptfixture{paragraph}"),
                    meeting_id: None,
                },
            )
            .unwrap();
        expected.insert(note.entity_id);
    }
    let mut found = BTreeSet::new();
    let mut query = json!({"query":"excerptfixture","fields":["notes"],"limit":100});
    for index in 0..3 {
        let page = invoke(&repo, FixtureReadTool::Search, query.clone());
        for hit in page["data"]["hits"].as_array().unwrap() {
            assert!(found.insert(hit["note_id"].as_str().unwrap().to_owned()));
        }
        let cursor = &page["data"]["page"]["next_cursor"];
        if index == 0 {
            assert!(
                cursor.is_string(),
                "40 long hits require a byte cut below the item limit"
            );
        }
        if cursor.is_null() {
            break;
        }
        query["cursor"] = cursor.clone();
    }
    assert_eq!(found, expected);
}

#[test]
fn fixture_title_search_tracks_metadata_only_revision_changes() {
    let (_directory, repo) = repository();
    let before = invoke(
        &repo,
        FixtureReadTool::GetMeeting,
        json!({"meeting_id": READY_MEETING_ID}),
    );
    let previous_revision = before["data"]["metadata_revision"].as_str().unwrap();
    repo.desktop_library(
        std::process::id(),
        DesktopLibraryRequest::MeetingUntag {
            meeting_id: READY_MEETING_ID.into(),
            tag_id: "fixture-tag".into(),
            expected_revision: previous_revision.into(),
            idempotency_key: "fixture-title-untag".into(),
        },
    )
    .unwrap();
    let current = invoke(
        &repo,
        FixtureReadTool::GetMeeting,
        json!({"meeting_id": READY_MEETING_ID}),
    );
    assert_ne!(current["data"]["metadata_revision"], previous_revision);
    let search = invoke(
        &repo,
        FixtureReadTool::Search,
        json!({"query": "Орион", "fields": ["title"]}),
    );
    assert_eq!(
        search["data"]["hits"][0]["source_revision"],
        current["data"]["metadata_revision"]
    );
}

use super::*;
use chrono::Utc;
use uuid::Uuid;

fn context(key: &str) -> MutationContext {
    MutationContext {
        principal: "private-fixture-principal".into(),
        idempotency_key: key.into(),
    }
}

fn note(meeting_id: Option<String>, markdown: String) -> NewNote {
    NewNote {
        meeting_id,
        title: "З".repeat(200),
        markdown,
    }
}

#[test]
fn lists_only_visible_metadata_and_pins_scope_budget_and_sequence() {
    let store = Store::open_in_memory().unwrap();
    let meeting = store
        .service_create_text_meeting(
            &context("meeting"),
            &NewTextMeeting {
                title: "Synthetic planning meeting".into(),
                started_at: Utc::now(),
                participants: vec![],
                folder_id: None,
                tag_ids: vec![],
                calendar_uid: None,
                transcript: None,
            },
        )
        .unwrap()
        .entity_id;
    let linked = store
        .service_create_note(
            &context("linked"),
            &note(Some(meeting.clone()), "private-markdown-sentinel".into()),
        )
        .unwrap();
    let standalone = store
        .service_create_note(
            &context("standalone"),
            &note(None, "private-markdown-sentinel".into()),
        )
        .unwrap();

    let scope = FixtureNoteScope::All;
    let page = store.fixture_notes(scope, None, 100, 1024).unwrap();
    let json = serde_json::to_string(&page).unwrap();
    assert!(json.len() <= 1024);
    assert!(!json.contains("private-markdown-sentinel"));
    assert!(!json.contains("private-fixture-principal"));
    assert_eq!(page.items.len(), 1);
    let next = page.next.unwrap();
    let last = store.fixture_notes(scope, Some(&next), 100, 1024).unwrap();
    assert_eq!(last.items.len(), 1);
    assert!(last.next.is_none());
    assert!(page.items[0].id < last.items[0].id);
    assert!(matches!(
        store.fixture_notes(FixtureNoteScope::Standalone, Some(&next), 100, 1024),
        Err(DataError::StaleCursor)
    ));
    assert!(matches!(
        store.fixture_notes(scope, Some(&next), 100, 2048),
        Err(DataError::StaleCursor)
    ));

    let meeting_page = store
        .fixture_notes(FixtureNoteScope::Meeting(&meeting), None, 100, 1024)
        .unwrap();
    assert_eq!(meeting_page.items.len(), 1);
    assert_eq!(meeting_page.items[0].id, linked.entity_id);
    let standalone_page = store
        .fixture_notes(FixtureNoteScope::Standalone, None, 100, 1024)
        .unwrap();
    assert_eq!(standalone_page.items.len(), 1);
    assert_eq!(standalone_page.items[0].id, standalone.entity_id);

    let revision = store
        .service_meeting(&meeting)
        .unwrap()
        .unwrap()
        .lifecycle_revision;
    store
        .service_delete_meeting(&context("delete-meeting"), &meeting, &revision)
        .unwrap();
    assert!(matches!(
        store.fixture_notes(scope, Some(&next), 100, 1024),
        Err(DataError::StaleCursor)
    ));
    assert!(matches!(
        store.with_fixture_note(&linked.entity_id, |_| Ok(())),
        Err(DataError::NotFound)
    ));
    let remaining = store.fixture_notes(scope, None, 100, 1024).unwrap();
    assert_eq!(remaining.items.len(), 1);
    assert_eq!(remaining.items[0].id, standalone.entity_id);
}

#[test]
fn note_windows_are_exact_and_tag_changes_with_revision_and_epoch() {
    let store = Store::open_in_memory().unwrap();
    let body = "🦀\u{1}\"\\\nраз".repeat(3000);
    let created = store
        .service_create_note(&context("body"), &note(None, body.clone()))
        .unwrap();
    let epoch = Uuid::new_v4();
    let first_tag = store
        .with_fixture_note(&created.entity_id, |snapshot| {
            assert!(snapshot.header.meeting_id.is_none());
            assert_eq!(snapshot.header.revision, created.revision);
            assert_eq!(snapshot.window(0, 1)?.text, "");
            assert_eq!(snapshot.window(0, 4)?.text, "🦀");
            assert!(matches!(snapshot.window(1, 32), Err(DataError::Invalid(_))));
            let mut read = String::new();
            while read.len() < body.len() {
                let page = snapshot.window(read.len() as u64, 8191)?;
                assert_eq!(page.total_bytes as usize, body.len());
                assert!(!page.text.is_empty() && page.text.len() <= 8191);
                read.push_str(&page.text);
            }
            assert_eq!(read, body);
            assert!(matches!(
                snapshot.window(body.len() as u64, 8191),
                Err(DataError::StaleCursor)
            ));
            assert_ne!(
                snapshot.cursor_tag(epoch, 4),
                snapshot.cursor_tag(Uuid::new_v4(), 4)
            );
            Ok(snapshot.cursor_tag(epoch, 4))
        })
        .unwrap();
    store
        .service_update_note(
            &context("change-body"),
            &created.entity_id,
            &created.revision,
            &NoteUpdate {
                markdown: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .with_fixture_note(&created.entity_id, |snapshot| {
            assert_ne!(snapshot.cursor_tag(epoch, 4), first_tag);
            let window = snapshot.window(0, 65_536)?;
            assert_eq!(window.total_bytes, 0);
            assert!(window.text.is_empty());
            Ok(())
        })
        .unwrap();
}

use super::*;
use crate::service_data::{MutationContext, NewTextMeeting};
use serde_json::json;

const DATE: &str = "2026-11-03T00:00:00Z";

fn chat(
    store: &Store,
    id: &str,
    kind: &str,
    scope: Option<&str>,
    title: &str,
    rowid: i64,
    date: &str,
) {
    store.conn.lock().execute("INSERT INTO chats(rowid,id,scope_kind,scope_id,title,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![rowid,id,kind,scope,title,date]).unwrap();
}
fn message(
    store: &Store,
    id: &str,
    chat: &str,
    role: &str,
    content: &str,
    citations: &str,
    position: (i64, &str),
) {
    store.conn.lock().execute("INSERT INTO chat_messages(rowid,id,chat_id,role,content,citations_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![position.0,id,chat,role,content,citations,position.1]).unwrap();
}
fn describe(store: &Store, id: &str) -> DesktopChatHeader {
    let DesktopChatsReply::Header(header) = store
        .service_desktop_chats(&DesktopChatsRequest::Describe { chat_id: id.into() })
        .unwrap()
    else {
        panic!("wrong reply")
    };
    header
}
fn list(scope: DesktopChatScope, limit: u16, max_bytes: u32) -> DesktopChatsRequest {
    DesktopChatsRequest::List {
        scope,
        limit,
        max_bytes,
        cursor: None,
    }
}
fn messages(header: &DesktopChatHeader, limit: u16, max_bytes: u32) -> DesktopChatsRequest {
    DesktopChatsRequest::Messages {
        chat_id: header.id.clone(),
        expected_generation: header.generation.clone(),
        expected_messages_revision: header.messages_revision.clone(),
        limit,
        max_bytes,
        cursor: None,
    }
}
fn chat_field(
    header: &DesktopChatHeader,
    field: DesktopChatField,
    max_bytes: u32,
) -> DesktopChatsRequest {
    DesktopChatsRequest::ReadChatField {
        chat_id: header.id.clone(),
        expected_generation: header.generation.clone(),
        expected_metadata_revision: header.metadata_revision.clone(),
        field,
        cursor: None,
        max_bytes,
    }
}
fn message_field(
    chat: &DesktopChatHeader,
    message: &DesktopChatMessageHeader,
    field: DesktopChatMessageField,
    max_bytes: u32,
) -> DesktopChatsRequest {
    DesktopChatsRequest::ReadMessageField {
        chat_id: chat.id.clone(),
        message_id: message.id.clone(),
        expected_chat_generation: chat.generation.clone(),
        expected_generation: message.generation.clone(),
        expected_revision: message.revision.clone(),
        field,
        cursor: None,
        max_bytes,
    }
}
fn list_all(store: &Store, mut request: DesktopChatsRequest) -> Vec<DesktopChatHeader> {
    let mut items = Vec::new();
    for _ in 0..1000 {
        let reply = store.service_desktop_chats(&request).unwrap();
        let DesktopChatsReply::List(page) = reply else {
            panic!("wrong reply")
        };
        items.extend(page.items);
        let DesktopChatsRequest::List { cursor, .. } = &mut request else {
            unreachable!()
        };
        *cursor = page.next;
        if cursor.is_none() {
            return items;
        }
    }
    panic!("list did not terminate")
}
fn read_chat(store: &Store, mut request: DesktopChatsRequest) -> (bool, String) {
    let mut text = String::new();
    for _ in 0..1000 {
        let reply = store.service_desktop_chats(&request).unwrap();
        let DesktopChatsRequest::ReadChatField {
            max_bytes, cursor, ..
        } = &mut request
        else {
            unreachable!()
        };
        assert!(serde_json::to_vec(&reply).unwrap().len() <= *max_bytes as usize);
        let DesktopChatsReply::ChatField(page) = reply else {
            panic!("wrong reply")
        };
        assert_eq!(page.byte_offset, text.len() as u64);
        text.push_str(&page.text);
        *cursor = page.next;
        if cursor.is_none() {
            assert_eq!(page.total_bytes, text.len() as u64);
            return (page.present, text);
        }
    }
    panic!("field did not terminate")
}
fn read_message(store: &Store, mut request: DesktopChatsRequest) -> String {
    let mut text = String::new();
    for _ in 0..1000 {
        let reply = store.service_desktop_chats(&request).unwrap();
        let DesktopChatsRequest::ReadMessageField {
            max_bytes, cursor, ..
        } = &mut request
        else {
            unreachable!()
        };
        assert!(serde_json::to_vec(&reply).unwrap().len() <= *max_bytes as usize);
        let DesktopChatsReply::MessageField(page) = reply else {
            panic!("wrong reply")
        };
        assert_eq!(page.byte_offset, text.len() as u64);
        text.push_str(&page.text);
        *cursor = page.next;
        if cursor.is_none() {
            assert_eq!(page.total_bytes, text.len() as u64);
            return text;
        }
    }
    panic!("field did not terminate")
}
fn snapshot(conn: &Connection) -> Vec<String> {
    [
        "SELECT json_group_array(json_array(rowid,id,hex(CAST(scope_kind AS BLOB)),hex(CAST(scope_id AS BLOB)),hex(CAST(title AS BLOB)),created_at)) FROM (SELECT rowid,* FROM chats ORDER BY rowid)",
        "SELECT json_group_array(json_array(rowid,id,chat_id,hex(CAST(role AS BLOB)),hex(CAST(content AS BLOB)),hex(CAST(citations_json AS BLOB)),created_at)) FROM (SELECT rowid,* FROM chat_messages ORDER BY rowid)",
        "SELECT json_group_array(json_array(chat_id,generation,metadata_revision,messages_revision,erasure_revision,state,title_bytes,scope_kind_bytes,scope_id_bytes)) FROM (SELECT * FROM service_chat_state ORDER BY chat_id)",
        "SELECT json_group_array(json_array(message_id,chat_id,generation,revision,state,role_bytes,content_bytes,citations_bytes)) FROM (SELECT * FROM service_chat_message_state ORDER BY message_id)",
        "SELECT revision FROM service_chat_catalog",
    ].into_iter().map(|sql|conn.query_row(sql,[],|r|r.get(0)).unwrap()).collect()
}

#[test]
fn migration_preserves_all_legacy_bytes_rowids_and_foreign_keys() {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    crate::migrations::run_until(&conn, 16).unwrap();
    let title = " Title\0  Привет 🦀 ".repeat(7000);
    let kind = "kind\0".repeat(100);
    let scope = "\0Скоуп ".repeat(10000);
    let content = "\0 raw\n🦀  ".repeat(12000);
    let role = "custom\0role".repeat(100);
    let citations = "{ invalid legacy\0 citations";
    conn.execute("INSERT INTO chats(rowid,id,scope_kind,scope_id,title,created_at) VALUES(-7,'чат SQL',?1,?2,?3,?4)",params![kind,scope,title,DATE]).unwrap();
    conn.execute("INSERT INTO chat_messages(rowid,id,chat_id,role,content,citations_json,created_at) VALUES(9007199254740992,'message SQL','чат SQL',?1,?2,?3,?4)",params![role,content,citations,DATE]).unwrap();
    let before:(i64,String,String,String,String,String,String)=conn.query_row("SELECT c.rowid,c.scope_kind,c.scope_id,c.title,m.role,m.content,m.citations_json FROM chats c JOIN chat_messages m ON m.chat_id=c.id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
    crate::migrations::run_until(&conn, 17).unwrap();
    let after:(i64,String,String,String,String,String,String)=conn.query_row("SELECT c.rowid,c.scope_kind,c.scope_id,c.title,m.role,m.content,m.citations_json FROM chats c JOIN chat_messages m ON m.chat_id=c.id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
    assert_eq!(before, after);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    let header = chat_header_on(&conn, "чат SQL").unwrap();
    assert_eq!(header.rowid, "-7");
    assert_eq!(header.title_bytes, title.len() as u64);
    assert_eq!(header.scope_kind_bytes, kind.len() as u64);
    assert_eq!(header.scope_id_bytes, Some(scope.len() as u64));
    assert!(header.title_preview.len() <= 256);
    let message = message_header_on(&conn, "чат SQL", "message SQL").unwrap();
    assert_eq!(message.rowid, "9007199254740992");
    assert_eq!(message.content_bytes, content.len() as u64);
    assert_eq!(message.citations_bytes, citations.len() as u64);
    let stable = snapshot(&conn);
    crate::migrations::run_until(&conn, 17).unwrap();
    assert_eq!(stable, snapshot(&conn));
}

#[test]
fn exact_scopes_preserve_none_empty_nul_and_giant_like_chat_keys() {
    let store = Store::open_in_memory().unwrap();
    for (index, scope) in [None, Some(""), Some(" "), Some("\0"), Some("Скоуп")]
        .into_iter()
        .enumerate()
    {
        chat(
            &store,
            &format!("chat-{index}"),
            "meeting",
            scope,
            "exact",
            index as i64,
            DATE,
        );
    }
    chat(
        &store,
        "all-some",
        "all",
        Some("ignored-by-retrieval"),
        "all",
        10,
        DATE,
    );
    chat(&store, "empty-kind", "", None, "empty", 11, DATE);
    let giant_kind = "kind\0".repeat(100);
    let giant_scope = "scope\0🦀".repeat(2000);
    chat(
        &store,
        "giant-1",
        &giant_kind,
        Some(&giant_scope),
        "one",
        12,
        DATE,
    );
    chat(
        &store,
        "giant-2",
        &giant_kind,
        Some(&giant_scope),
        "two",
        13,
        DATE,
    );
    for (index, scope) in [None, Some(""), Some(" "), Some("\0"), Some("Скоуп")]
        .into_iter()
        .enumerate()
    {
        let rows = list_all(
            &store,
            list(
                DesktopChatScope::Exact {
                    kind: "meeting".into(),
                    scope_id: scope.map(str::to_owned),
                },
                1,
                65024,
            ),
        );
        assert_eq!(
            rows.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            [format!("chat-{index}")]
        );
        assert_eq!(rows[0].scope_id_bytes, scope.map(|s| s.len() as u64));
        let (present, text) = read_chat(
            &store,
            chat_field(&rows[0], DesktopChatField::ScopeId, 2048),
        );
        assert_eq!(present, scope.is_some());
        assert_eq!(text, scope.unwrap_or(""));
    }
    let giant = describe(&store, "giant-1");
    let rows = list_all(
        &store,
        list(
            DesktopChatScope::LikeChat {
                chat_id: giant.id.clone(),
                generation: giant.generation.clone(),
            },
            1,
            65024,
        ),
    );
    assert_eq!(
        rows.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        ["giant-1", "giant-2"]
    );
    assert_eq!(
        read_chat(
            &store,
            chat_field(&giant, DesktopChatField::ScopeKind, 2048)
        )
        .1,
        giant_kind
    );
    assert_eq!(
        read_chat(&store, chat_field(&giant, DesktopChatField::ScopeId, 4096)).1,
        giant_scope
    );
    assert!(matches!(
        store.service_desktop_chats(&list(
            DesktopChatScope::Exact {
                kind: "meeting".into(),
                scope_id: Some("x".repeat(8193))
            },
            1,
            65024
        )),
        Err(DataError::Invalid(_))
    ));
    assert_eq!(
        list_all(&store, list(DesktopChatScope::AllCatalog, 100, 65024)).len(),
        9
    );
}

#[test]
fn pages_seek_original_sqlite_text_order_and_exact_signed_rowids() {
    let store = Store::open_in_memory().unwrap();
    let dates = [
        (-7, "z", DATE),
        (0, "a", DATE),
        (9007199254740992, "b", "2026-11-03T05:00:00+05:00"),
    ];
    for (rowid, id, date) in dates {
        chat(&store, id, "meeting", Some("orphan"), "order", rowid, date);
    }
    let legacy = store.list_chats("meeting", Some("orphan")).unwrap();
    let items = list_all(
        &store,
        list(
            DesktopChatScope::Exact {
                kind: "meeting".into(),
                scope_id: Some("orphan".into()),
            },
            1,
            65024,
        ),
    );
    assert_eq!(
        items.iter().map(|h| &h.id).collect::<Vec<_>>(),
        legacy.iter().map(|h| &h.id).collect::<Vec<_>>()
    );
    assert_eq!(
        items.iter().map(|h| h.rowid.as_str()).collect::<Vec<_>>(),
        ["9007199254740992", "-7", "0"]
    );
    for (rowid, id, date) in dates {
        message(
            &store,
            &format!("m{id}"),
            "z",
            "role",
            "text",
            "null",
            (rowid, date),
        );
    }
    let header = describe(&store, "z");
    let mut request = messages(&header, 1, 65024);
    let mut found = Vec::new();
    loop {
        let DesktopChatsReply::Messages(page) = store.service_desktop_chats(&request).unwrap()
        else {
            panic!("wrong reply")
        };
        found.extend(page.items.iter().map(|m| m.id.clone()));
        let DesktopChatsRequest::Messages { cursor, .. } = &mut request else {
            unreachable!()
        };
        *cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        found,
        store
            .messages("z")
            .unwrap()
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(found, ["mz", "ma", "mb"]);
}

#[test]
fn giant_fields_roundtrip_utf8_nul_whitespace_and_opaque_citations_without_read_writes() {
    let store = Store::open_in_memory().unwrap();
    let title = " \0🦀Заголовок\n ".repeat(7000);
    let role = "роль\0/custom ".repeat(300);
    let content = " \n\0🦀 точный текст\t".repeat(10000);
    let citations = "{invalid\0JSON still exact🦀}".repeat(5000);
    chat(&store, "chat", "meeting", None, &title, 0, DATE);
    message(
        &store,
        "large",
        "chat",
        &role,
        &content,
        &citations,
        (0, DATE),
    );
    message(&store, "pending", "chat", "assistant", "", "[]", (1, DATE));
    message(
        &store,
        "arbitrary-json",
        "chat",
        "other",
        "answer",
        r#"{"opaque":true}"#,
        (2, DATE),
    );
    let header = describe(&store, "chat");
    let DesktopChatsReply::Messages(page) = store
        .service_desktop_chats(&messages(&header, 100, 65024))
        .unwrap()
    else {
        panic!("wrong reply")
    };
    assert_eq!(page.items.len(), 3);
    let before = snapshot(&store.conn.lock());
    let changes: i64 = store
        .conn
        .lock()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        read_chat(&store, chat_field(&header, DesktopChatField::Title, 4096)),
        (true, title)
    );
    assert_eq!(
        read_message(
            &store,
            message_field(&header, &page.items[0], DesktopChatMessageField::Role, 2048)
        ),
        role
    );
    assert_eq!(
        read_message(
            &store,
            message_field(
                &header,
                &page.items[0],
                DesktopChatMessageField::Content,
                4096
            )
        ),
        content
    );
    assert_eq!(
        read_message(
            &store,
            message_field(
                &header,
                &page.items[0],
                DesktopChatMessageField::CitationsStoredJson,
                4096
            )
        ),
        citations
    );
    assert_eq!(
        read_message(
            &store,
            message_field(
                &header,
                &page.items[1],
                DesktopChatMessageField::Content,
                2048
            )
        ),
        ""
    );
    assert_eq!(
        read_message(
            &store,
            message_field(
                &header,
                &page.items[2],
                DesktopChatMessageField::CitationsStoredJson,
                2048
            )
        ),
        r#"{"opaque":true}"#
    );
    assert_eq!(snapshot(&store.conn.lock()), before);
    assert_eq!(
        store
            .conn
            .lock()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        changes
    );
    assert_eq!(store.messages("chat").unwrap()[0].citations, json!([]));
    assert_eq!(
        store.messages("chat").unwrap()[2].citations,
        json!({"opaque":true})
    );
}

#[test]
fn byte_budget_and_invalid_lookahead_return_valid_prefix_without_skipping_bad_rows() {
    let store = Store::open_in_memory().unwrap();
    for index in 0..20 {
        chat(
            &store,
            &format!("valid-{index:02}"),
            "all",
            None,
            &"\0".repeat(256),
            index,
            DATE,
        );
    }
    let bad = "bad".repeat(200);
    chat(&store, &bad, "all", None, "bad", 20, DATE);
    let mut request = list(DesktopChatScope::AllCatalog, 1, 65024);
    let DesktopChatsReply::List(first) = store.service_desktop_chats(&request).unwrap() else {
        panic!("wrong reply")
    };
    assert_eq!(first.items[0].id, "valid-00");
    assert!(first.next.is_some());
    request = list(DesktopChatScope::AllCatalog, 100, 6000);
    let mut all = Vec::new();
    loop {
        match store.service_desktop_chats(&request) {
            Ok(reply) => {
                assert!(serde_json::to_vec(&reply).unwrap().len() <= 6000);
                let DesktopChatsReply::List(page) = reply else {
                    panic!("wrong reply")
                };
                assert!(!page.items.is_empty());
                all.extend(page.items.iter().map(|h| h.id.clone()));
                let DesktopChatsRequest::List { cursor, .. } = &mut request else {
                    unreachable!()
                };
                *cursor = page.next;
                assert!(cursor.is_some());
            }
            Err(DataError::Invalid(_)) => break,
            other => panic!("unexpected reply {other:?}"),
        }
    }
    assert_eq!(
        all,
        (0..20).map(|i| format!("valid-{i:02}")).collect::<Vec<_>>()
    );
    message(&store, "good", "valid-00", "custom", "ok", "[]", (0, DATE));
    message(&store, &bad, "valid-00", "legacy", "bad", "[]", (1, DATE));
    let header = describe(&store, "valid-00");
    let mut request = messages(&header, 1, 65024);
    let DesktopChatsReply::Messages(first) = store.service_desktop_chats(&request).unwrap() else {
        panic!("wrong reply")
    };
    assert_eq!(first.items[0].id, "good");
    let DesktopChatsRequest::Messages { cursor, .. } = &mut request else {
        unreachable!()
    };
    *cursor = first.next;
    assert!(matches!(
        store.service_desktop_chats(&request),
        Err(DataError::Invalid(_))
    ));
}

#[test]
fn independent_revisions_generation_tombstones_and_cursor_scopes_are_durable() {
    let store = Store::open_in_memory().unwrap();
    chat(
        &store,
        "chat",
        "all",
        None,
        &"long 🦀 title".repeat(2000),
        0,
        DATE,
    );
    chat(&store, "second", "all", None, "second", 1, DATE);
    message(
        &store,
        "first",
        "chat",
        "assistant",
        &"body🦀".repeat(2000),
        "[]",
        (0, DATE),
    );
    message(
        &store,
        "second-message",
        "chat",
        "user",
        "question",
        "[]",
        (1, DATE),
    );
    let header = describe(&store, "chat");
    let DesktopChatsReply::Messages(page) = store
        .service_desktop_chats(&messages(&header, 1, 65024))
        .unwrap()
    else {
        panic!("wrong reply")
    };
    let mut chat_request = chat_field(&header, DesktopChatField::Title, 2048);
    let DesktopChatsReply::ChatField(field) = store.service_desktop_chats(&chat_request).unwrap()
    else {
        panic!("wrong reply")
    };
    let DesktopChatsRequest::ReadChatField { cursor, .. } = &mut chat_request else {
        unreachable!()
    };
    *cursor = field.next;
    let mut message_request = message_field(
        &header,
        &page.items[0],
        DesktopChatMessageField::Content,
        2048,
    );
    let DesktopChatsReply::MessageField(field) =
        store.service_desktop_chats(&message_request).unwrap()
    else {
        panic!("wrong reply")
    };
    let DesktopChatsRequest::ReadMessageField { cursor, .. } = &mut message_request else {
        unreachable!()
    };
    *cursor = field.next;
    let added = store
        .add_message("chat", "user", "next", &json!([]))
        .unwrap();
    let updated = describe(&store, "chat");
    assert_eq!(updated.metadata_revision, header.metadata_revision);
    assert_eq!(updated.erasure_revision, header.erasure_revision);
    assert_ne!(updated.messages_revision, header.messages_revision);
    assert!(store.service_desktop_chats(&chat_request).is_ok());
    assert!(store.service_desktop_chats(&message_request).is_ok());
    let mut stale_messages = messages(&header, 1, 65024);
    let DesktopChatsRequest::Messages { cursor, .. } = &mut stale_messages else {
        unreachable!()
    };
    *cursor = page.next;
    assert!(matches!(
        store.service_desktop_chats(&stale_messages),
        Err(DataError::Conflict { .. })
    ));
    store
        .update_message("first", "new body", &json!({"x":true}))
        .unwrap();
    assert!(matches!(
        store.service_desktop_chats(&message_request),
        Err(DataError::Conflict { .. })
    ));
    let old = describe(&store, "chat");
    let mut request = list(DesktopChatScope::AllCatalog, 1, 65024);
    let DesktopChatsReply::List(page) = store.service_desktop_chats(&request).unwrap() else {
        panic!("wrong reply")
    };
    let DesktopChatsRequest::List { cursor, .. } = &mut request else {
        unreachable!()
    };
    *cursor = page.next;
    let foreign = Store::open_in_memory().unwrap();
    assert!(matches!(
        foreign.service_desktop_chats(&request),
        Err(DataError::StaleCursor)
    ));
    let mut changed = request.clone();
    let DesktopChatsRequest::List { scope, .. } = &mut changed else {
        unreachable!()
    };
    *scope = DesktopChatScope::Exact {
        kind: "all".into(),
        scope_id: None,
    };
    assert!(matches!(
        store.service_desktop_chats(&changed),
        Err(DataError::StaleCursor)
    ));
    store.rename_chat("chat", "exact\0name").unwrap();
    assert!(matches!(
        store.service_desktop_chats(&request),
        Err(DataError::StaleCursor)
    ));
    assert_ne!(
        old.metadata_revision,
        describe(&store, "chat").metadata_revision
    );
    store.delete_chat("chat").unwrap();
    let tombstone: String = store
        .conn
        .lock()
        .query_row(
            "SELECT state FROM service_chat_message_state WHERE message_id=?1",
            [&added.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstone, "deleted");
    store
        .update_message(&added.id, "late callback", &json!([]))
        .unwrap();
    assert!(store.messages("chat").unwrap().is_empty());
    chat(&store, "chat", "all", None, "new lifetime", 0, DATE);
    assert_ne!(old.generation, describe(&store, "chat").generation);
    assert!(matches!(
        store.service_desktop_chats(&chat_field(&old, DesktopChatField::Title, 2048)),
        Err(DataError::Conflict { .. })
    ));
    let stable = describe(&store, "chat");
    store
        .conn
        .lock()
        .execute("DELETE FROM service_changes", [])
        .unwrap();
    assert_eq!(stable, describe(&store, "chat"));
}

fn context(key: &str) -> MutationContext {
    MutationContext {
        principal: "desktop".into(),
        idempotency_key: key.into(),
    }
}
fn meeting(store: &Store) -> String {
    store
        .service_create_text_meeting(
            &context("meeting"),
            &NewTextMeeting {
                title: "Synthetic".into(),
                started_at: Utc::now(),
                participants: vec![],
                folder_id: None,
                tag_ids: vec![],
                calendar_uid: None,
                transcript: Some(vec![crate::NewSegment {
                    start_ms: 0,
                    end_ms: 1000,
                    speaker: None,
                    text: "original raw".into(),
                }]),
            },
        )
        .unwrap()
        .entity_id
}

#[test]
fn transcript_and_meeting_erasure_fence_global_pending_rows_but_keep_history_and_unrelated_scope() {
    let store = Store::open_in_memory().unwrap();
    let target = meeting(&store);
    for (index, id, kind, scope) in [
        (0, "all-none", "all", None),
        (1, "all-some", "all", Some("other")),
        (2, "meeting-none", "meeting", None),
        (3, "target", "meeting", Some(target.as_str())),
        (4, "unrelated", "meeting", Some("other")),
        (5, "reference", "meeting", Some("reference")),
        (6, "unknown", "custom", Some("anything")),
    ] {
        chat(&store, id, kind, scope, "kept title", index, DATE);
        let citations = if id == "reference" {
            json!([{"meeting_id":target}]).to_string()
        } else {
            json!([]).to_string()
        };
        message(
            &store,
            &format!("pending-{id}"),
            id,
            "assistant",
            "",
            &citations,
            (index, DATE),
        );
    }
    message(
        &store,
        "history",
        "all-none",
        "user",
        "keep exact\0history",
        r#"[{"meeting_id":"other"}]"#,
        (100, DATE),
    );
    let before = list_all(&store, list(DesktopChatScope::AllCatalog, 100, 65024));
    let original = store.service_transcript_state(&target).unwrap();
    store
        .service_delete_transcript(&context("erase-transcript"), &target, &original.revision)
        .unwrap();
    for old in &before {
        let current = describe(&store, &old.id);
        if old.id == "unrelated" {
            assert_eq!(current, old.clone());
        } else {
            assert_ne!(current.erasure_revision, old.erasure_revision);
            assert_ne!(current.messages_revision, old.messages_revision);
            assert_eq!(current.metadata_revision, old.metadata_revision);
            assert_eq!(current.generation, old.generation);
        }
    }
    assert_eq!(
        store.messages("all-none").unwrap()[1].content,
        "keep exact\0history"
    );
    assert_eq!(
        store.messages("all-none").unwrap()[1].citations,
        json!([{"meeting_id":"other"}])
    );
    assert_eq!(store.messages("reference").unwrap()[0].citations, json!([]));
    let before = describe(&store, "meeting-none");
    let state = store.service_meeting(&target).unwrap().unwrap();
    store
        .service_delete_meeting(
            &context("erase-meeting"),
            &target,
            &state.lifecycle_revision,
        )
        .unwrap();
    assert_ne!(
        describe(&store, "meeting-none").erasure_revision,
        before.erasure_revision
    );
    assert_eq!(store.messages("meeting-none").unwrap()[0].content, "");
    assert!(store.get_chat("target").unwrap().is_some());
}

#[test]
fn passive_trigger_and_erasure_failures_roll_back_rows_lengths_revisions_and_citations() {
    let store = Store::open_in_memory().unwrap();
    let target = meeting(&store);
    chat(&store, "chat", "meeting", Some(&target), "title", 0, DATE);
    message(
        &store,
        "message",
        "chat",
        "assistant",
        "retained raw",
        &json!([{"meeting_id":target}]).to_string(),
        (0, DATE),
    );
    let before = snapshot(&store.conn.lock());
    let transcript = store.service_transcript_state(&target).unwrap();
    let segments = store.segments(&target).unwrap();
    store.conn.lock().execute_batch("CREATE TRIGGER fixture_chat_catalog_failure BEFORE UPDATE ON service_chat_catalog BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    assert!(store.rename_chat("chat", "must roll back").is_err());
    assert_eq!(snapshot(&store.conn.lock()), before);
    assert!(matches!(
        store.service_delete_transcript(&context("fault"), &target, &transcript.revision),
        Err(DataError::Sqlite(_))
    ));
    assert_eq!(snapshot(&store.conn.lock()), before);
    assert_eq!(store.service_transcript_state(&target).unwrap(), transcript);
    assert_eq!(store.segments(&target).unwrap(), segments);
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER fixture_chat_catalog_failure")
        .unwrap();
    store
        .service_delete_transcript(&context("fault"), &target, &transcript.revision)
        .unwrap();
    assert_ne!(snapshot(&store.conn.lock()), before);
}

#[test]
fn terminal_field_fits_without_cursor_and_missing_projection_never_adopts_on_read() {
    let store = Store::open_in_memory().unwrap();
    let id = "\u{1}".repeat(512);
    chat(&store, &id, "all", None, "x", 0, DATE);
    let header = describe(&store, &id);
    assert_eq!(
        read_chat(&store, chat_field(&header, DesktopChatField::Title, 4000)),
        (true, "x".into())
    );
    message(&store, "message", &id, "user", "body", "[]", (0, DATE));
    store
        .conn
        .lock()
        .execute(
            "DELETE FROM service_chat_message_state WHERE message_id='message'",
            [],
        )
        .unwrap();
    let current = describe(&store, &id);
    let before = snapshot(&store.conn.lock());
    assert!(matches!(
        store.service_desktop_chats(&messages(&current, 100, 65024)),
        Err(DataError::IndexUnavailable)
    ));
    assert_eq!(snapshot(&store.conn.lock()), before);
    store
        .conn
        .lock()
        .execute("DELETE FROM service_chat_state WHERE chat_id=?1", [&id])
        .unwrap();
    let before = snapshot(&store.conn.lock());
    assert!(matches!(
        store.service_desktop_chats(&DesktopChatsRequest::Describe { chat_id: id }),
        Err(DataError::IndexUnavailable)
    ));
    assert_eq!(snapshot(&store.conn.lock()), before);
    assert!(matches!(
        store.service_desktop_chats(&DesktopChatsRequest::Describe {
            chat_id: "absent".into()
        }),
        Err(DataError::NotFound)
    ));
}

#[test]
fn forged_empty_continuations_and_foreign_or_mid_utf8_field_cursors_are_rejected() {
    let store = Store::open_in_memory().unwrap();
    chat(&store, "first", "all", None, "first", 0, DATE);
    chat(&store, "last", "all", None, "last", 1, DATE);
    let mut request = list(DesktopChatScope::AllCatalog, 1, 65024);
    let DesktopChatsReply::List(page) = store.service_desktop_chats(&request).unwrap() else {
        panic!("wrong reply")
    };
    let mut forged = page.next.unwrap();
    forged.after_rowid = "1".into();
    let DesktopChatsRequest::List { cursor, .. } = &mut request else {
        unreachable!()
    };
    *cursor = Some(forged);
    assert!(matches!(
        store.service_desktop_chats(&request),
        Err(DataError::StaleCursor)
    ));
    message(
        &store,
        "first-message",
        "first",
        "role",
        &"🦀".repeat(4000),
        "[]",
        (0, DATE),
    );
    message(
        &store,
        "last-message",
        "first",
        "role",
        "last",
        "[]",
        (1, DATE),
    );
    let header = describe(&store, "first");
    let mut request = messages(&header, 1, 65024);
    let DesktopChatsReply::Messages(page) = store.service_desktop_chats(&request).unwrap() else {
        panic!("wrong reply")
    };
    let mut forged = page.next.unwrap();
    forged.after_rowid = "1".into();
    let DesktopChatsRequest::Messages { cursor, .. } = &mut request else {
        unreachable!()
    };
    *cursor = Some(forged);
    assert!(matches!(
        store.service_desktop_chats(&request),
        Err(DataError::StaleCursor)
    ));
    let mut request = message_field(
        &header,
        &page.items[0],
        DesktopChatMessageField::Content,
        2048,
    );
    let DesktopChatsReply::MessageField(page) = store.service_desktop_chats(&request).unwrap()
    else {
        panic!("wrong reply")
    };
    let mut original = page.next.unwrap();
    let DesktopChatsRequest::ReadMessageField { cursor, .. } = &mut request else {
        unreachable!()
    };
    *cursor = Some(original.clone());
    for fault in 0..4 {
        let mut changed = request.clone();
        let DesktopChatsRequest::ReadMessageField { cursor, .. } = &mut changed else {
            unreachable!()
        };
        let c = cursor.as_mut().unwrap();
        match fault {
            0 => c.library_scope = "0".repeat(32),
            1 => c.total_bytes += 1,
            2 => c.field = DesktopChatMessageField::Role,
            3 => c.max_bytes = 4096,
            _ => unreachable!(),
        }
        assert!(matches!(
            store.service_desktop_chats(&changed),
            Err(DataError::StaleCursor)
        ));
    }
    original.byte_offset = 1;
    let DesktopChatsRequest::ReadMessageField { cursor, .. } = &mut request else {
        unreachable!()
    };
    *cursor = Some(original);
    assert!(matches!(
        store.service_desktop_chats(&request),
        Err(DataError::Invalid(_))
    ));
}

#[test]
fn empty_title_kind_role_and_present_empty_scope_remain_distinct_from_null() {
    let store = Store::open_in_memory().unwrap();
    chat(&store, "present-empty", "", Some(""), "", 0, DATE);
    chat(&store, "null-scope", "", None, "", 1, DATE);
    message(
        &store,
        "empty-message",
        "present-empty",
        "",
        "",
        "",
        (0, DATE),
    );
    let header = describe(&store, "present-empty");
    assert_eq!(header.title_preview, "");
    assert_eq!(header.title_bytes, 0);
    assert_eq!(header.scope_kind_preview, "");
    assert_eq!(header.scope_kind_bytes, 0);
    assert_eq!(header.scope_id_preview, Some(String::new()));
    assert_eq!(header.scope_id_bytes, Some(0));
    assert_eq!(
        read_chat(&store, chat_field(&header, DesktopChatField::Title, 2048)),
        (true, String::new())
    );
    assert_eq!(
        read_chat(
            &store,
            chat_field(&header, DesktopChatField::ScopeKind, 2048)
        ),
        (true, String::new())
    );
    assert_eq!(
        read_chat(&store, chat_field(&header, DesktopChatField::ScopeId, 2048)),
        (true, String::new())
    );
    let null = describe(&store, "null-scope");
    assert_eq!(null.scope_id_preview, None);
    assert_eq!(null.scope_id_bytes, None);
    assert_eq!(
        read_chat(&store, chat_field(&null, DesktopChatField::ScopeId, 2048)),
        (false, String::new())
    );
    let DesktopChatsReply::Messages(page) = store
        .service_desktop_chats(&messages(&header, 100, 65024))
        .unwrap()
    else {
        panic!("wrong reply")
    };
    assert_eq!(page.items[0].role_preview, "");
    assert_eq!(page.items[0].role_bytes, 0);
    assert_eq!(
        read_message(
            &store,
            message_field(&header, &page.items[0], DesktopChatMessageField::Role, 2048)
        ),
        ""
    );
    assert_eq!(
        read_message(
            &store,
            message_field(
                &header,
                &page.items[0],
                DesktopChatMessageField::CitationsStoredJson,
                2048
            )
        ),
        ""
    );
    for scope in [None, Some("")] {
        let actual = list_all(
            &store,
            list(
                DesktopChatScope::Exact {
                    kind: String::new(),
                    scope_id: scope.map(str::to_owned),
                },
                1,
                65024,
            ),
        );
        assert_eq!(actual.len(), 1);
        assert_eq!(
            actual[0].id,
            if scope.is_some() {
                "present-empty"
            } else {
                "null-scope"
            }
        );
    }
    assert_eq!(
        list_all(&store, list(DesktopChatScope::AllCatalog, 1, 65024)).len(),
        2
    );
}

#[test]
fn native_sqlite_timestamp_formats_decode_identically_without_changing_seek_order() {
    let store = Store::open_in_memory().unwrap();
    let formats = [
        "2026-11-03 01:02:03",
        "2026-11-03T01:02:03.123456789",
        "2026-11-03 01:02:03.123+05:00",
        "2026-11-03T01:02:03Z",
    ];
    for (index, date) in formats.into_iter().enumerate() {
        let id = format!("format-{index}");
        chat(&store, &id, "all", None, "timestamp", index as i64, date);
        message(
            &store,
            &id,
            "format-0",
            "assistant",
            "text",
            "[]",
            (index as i64, date),
        );
    }
    let legacy = store.list_chats("all", None).unwrap();
    let actual = list_all(
        &store,
        list(
            DesktopChatScope::Exact {
                kind: "all".into(),
                scope_id: None,
            },
            1,
            65024,
        ),
    );
    assert_eq!(
        actual
            .iter()
            .map(|c| (&c.id, c.created_at))
            .collect::<Vec<_>>(),
        legacy
            .iter()
            .map(|c| (&c.id, c.created_at))
            .collect::<Vec<_>>()
    );
    for header in &actual {
        assert_eq!(
            header.created_at,
            store.get_chat(&header.id).unwrap().unwrap().created_at
        );
    }
    let header = describe(&store, "format-0");
    let mut request = messages(&header, 1, 65024);
    let mut actual = Vec::new();
    loop {
        let DesktopChatsReply::Messages(page) = store.service_desktop_chats(&request).unwrap()
        else {
            panic!("wrong reply")
        };
        actual.extend(page.items.into_iter().map(|m| (m.id, m.created_at)));
        let DesktopChatsRequest::Messages { cursor, .. } = &mut request else {
            unreachable!()
        };
        *cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        actual,
        store
            .messages("format-0")
            .unwrap()
            .into_iter()
            .map(|m| (m.id, m.created_at))
            .collect::<Vec<_>>()
    );
}

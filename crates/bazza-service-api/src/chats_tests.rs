use super::*;

fn scope() -> String {
    "a".repeat(32)
}
fn hash() -> String {
    "b".repeat(64)
}
fn header(id: &str, rowid: &str) -> DesktopChatHeader {
    DesktopChatHeader {
        id: id.into(),
        rowid: rowid.into(),
        generation: "chat-generation".into(),
        metadata_revision: "metadata-revision".into(),
        messages_revision: "messages-revision".into(),
        erasure_revision: "erasure-revision".into(),
        title_preview: " exact\0Орион ".into(),
        scope_kind_preview: "meeting".into(),
        scope_id_preview: None,
        title_bytes: " exact\0Орион ".len() as u64,
        scope_kind_bytes: 7,
        scope_id_bytes: None,
        created_at: "2026-11-02T00:30:00Z".parse().unwrap(),
    }
}
fn message(id: &str, rowid: &str) -> DesktopChatMessageHeader {
    DesktopChatMessageHeader {
        id: id.into(),
        rowid: rowid.into(),
        chat_id: "chat".into(),
        generation: "message-generation".into(),
        revision: "message-revision".into(),
        role_preview: "custom\0роль".into(),
        role_bytes: "custom\0роль".len() as u64,
        content_bytes: 1_000_000,
        citations_bytes: 10_000_000,
        created_at: "2026-11-02T00:30:00Z".parse().unwrap(),
    }
}
fn list_request() -> DesktopChatsRequest {
    DesktopChatsRequest::List {
        scope: DesktopChatScope::AllCatalog,
        limit: 5,
        max_bytes: MAX_CHAT_DATA_BYTES as u32,
        cursor: None,
    }
}
fn list_page() -> DesktopChatListPage {
    let mut first = header("first", "2");
    // Raw SQLite strings can sort in the opposite order to normalized instants.
    first.created_at = "2026-11-02T01:00:00+01:00".parse().unwrap();
    DesktopChatListPage {
        library_scope: scope(),
        catalog_revision: "catalog-revision".into(),
        items: vec![first, header("second", "1")],
        next: Some(DesktopChatListCursor {
            library_scope: scope(),
            catalog_revision: "catalog-revision".into(),
            query_hash: hash(),
            after_rowid: "1".into(),
        }),
    }
}
fn messages_request() -> DesktopChatsRequest {
    DesktopChatsRequest::Messages {
        chat_id: "chat".into(),
        expected_generation: "chat-generation".into(),
        expected_messages_revision: "messages-revision".into(),
        limit: 5,
        max_bytes: MAX_CHAT_DATA_BYTES as u32,
        cursor: None,
    }
}
fn messages_page() -> DesktopChatMessagesPage {
    DesktopChatMessagesPage {
        library_scope: scope(),
        chat_id: "chat".into(),
        generation: "chat-generation".into(),
        messages_revision: "messages-revision".into(),
        items: vec![message("first", "2"), message("second", "1")],
        next: Some(DesktopChatMessagesCursor {
            library_scope: scope(),
            chat_id: "chat".into(),
            generation: "chat-generation".into(),
            messages_revision: "messages-revision".into(),
            query_hash: hash(),
            after_rowid: "1".into(),
        }),
    }
}
fn field_request() -> DesktopChatsRequest {
    DesktopChatsRequest::ReadChatField {
        chat_id: "chat".into(),
        expected_generation: "chat-generation".into(),
        expected_metadata_revision: "metadata-revision".into(),
        field: DesktopChatField::ScopeId,
        cursor: None,
        max_bytes: MAX_CHAT_DATA_BYTES as u32,
    }
}
fn field_page() -> DesktopChatFieldPage {
    let text = "я\0🦀";
    let offset = text.len() as u64;
    DesktopChatFieldPage {
        library_scope: scope(),
        chat_id: "chat".into(),
        generation: "chat-generation".into(),
        metadata_revision: "metadata-revision".into(),
        field: DesktopChatField::ScopeId,
        byte_offset: 0,
        total_bytes: offset + 3,
        present: true,
        text: text.into(),
        next: Some(DesktopChatFieldCursor {
            library_scope: scope(),
            chat_id: "chat".into(),
            generation: "chat-generation".into(),
            metadata_revision: "metadata-revision".into(),
            field: DesktopChatField::ScopeId,
            byte_offset: offset,
            total_bytes: offset + 3,
            present: true,
            max_bytes: MAX_CHAT_DATA_BYTES as u32,
        }),
    }
}
fn message_field_request() -> DesktopChatsRequest {
    DesktopChatsRequest::ReadMessageField {
        chat_id: "chat".into(),
        message_id: "message".into(),
        expected_chat_generation: "chat-generation".into(),
        expected_generation: "message-generation".into(),
        expected_revision: "message-revision".into(),
        field: DesktopChatMessageField::CitationsStoredJson,
        cursor: None,
        max_bytes: MAX_CHAT_DATA_BYTES as u32,
    }
}
fn message_field_page(text: &str) -> DesktopChatMessageFieldPage {
    DesktopChatMessageFieldPage {
        library_scope: scope(),
        chat_id: "chat".into(),
        message_id: "message".into(),
        chat_generation: "chat-generation".into(),
        generation: "message-generation".into(),
        revision: "message-revision".into(),
        field: DesktopChatMessageField::CitationsStoredJson,
        byte_offset: 0,
        total_bytes: text.len() as u64,
        text: text.into(),
        next: None,
    }
}

#[test]
fn sql_ids_signed_rowids_and_exact_scope_pairs_preserve_native_keys() {
    for id in [" chat /🙂:key ", "я", "a"] {
        assert!(chat_id(id));
    }
    assert!(chat_id(&"я".repeat(256)));
    for id in [String::new(), "x".repeat(513), "id\0nul".into()] {
        assert!(!chat_id(&id));
    }
    for rowid in [
        "0",
        "-1",
        "1",
        "9007199254740993",
        "-9223372036854775808",
        "9223372036854775807",
    ] {
        assert!(chat_rowid(rowid));
    }
    for rowid in [
        "",
        "-0",
        "+1",
        "01",
        " 1",
        "1 ",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        assert!(!chat_rowid(rowid));
    }
    for kind in ["meeting", "all", "", "legacy\0вид"] {
        for scope_id in [None, Some(""), Some(" \0🙂 ")] {
            let selected = DesktopChatScope::Exact {
                kind: kind.into(),
                scope_id: scope_id.map(str::to_owned),
            };
            assert!(selected.valid());
            assert_eq!(
                serde_json::from_value::<DesktopChatScope>(
                    serde_json::to_value(&selected).unwrap()
                )
                .unwrap(),
                selected
            );
        }
    }
    assert!(DesktopChatScope::Exact {
        kind: "\0".repeat(128),
        scope_id: Some("\0".repeat(8192))
    }
    .valid());
    assert!(!DesktopChatScope::Exact {
        kind: "x".repeat(129),
        scope_id: None
    }
    .valid());
    assert!(!DesktopChatScope::Exact {
        kind: "all".into(),
        scope_id: Some("x".repeat(8193))
    }
    .valid());
}

#[test]
fn headers_bound_previews_without_restricting_giant_or_empty_legacy_fields() {
    let mut chat = header("chat", "-9223372036854775808");
    chat.title_bytes = MAX_SAFE;
    chat.scope_kind_bytes = 8_000_000;
    chat.scope_kind_preview = "giant\0kind".into();
    chat.scope_id_preview = Some(String::new());
    chat.scope_id_bytes = Some(0);
    assert!(chat.valid());
    chat.scope_id_bytes = None;
    assert!(!chat.valid());
    chat.scope_id_preview = None;
    assert!(chat.valid());
    chat.title_preview = "я".repeat(129);
    assert!(!chat.valid());
    let mut message = message("message", "0");
    message.role_preview.clear();
    message.role_bytes = 0;
    message.content_bytes = 0;
    message.citations_bytes = 0;
    assert!(
        message.valid(),
        "an empty assistant is a real row, not an inferred run state"
    );
    message.content_bytes = MAX_SAFE + 1;
    assert!(!message.valid());
    message.content_bytes = 0;
    message.role_preview = "x".into();
    assert!(!message.valid());
}

#[test]
fn closed_wire_shapes_deny_writes_authority_and_unknown_fields() {
    let request = list_request();
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["kind"], "list");
    assert_eq!(value["data"]["scope"]["mode"], "all_catalog");
    for field in ["actor", "path", "provider", "body", "sql", "grant"] {
        let mut bad = value.clone();
        bad["data"][field] = serde_json::json!("forbidden");
        assert!(serde_json::from_value::<DesktopChatsRequest>(bad).is_err());
    }
    let mut bad = value;
    bad["kind"] = serde_json::json!("send");
    assert!(serde_json::from_value::<DesktopChatsRequest>(bad).is_err());
    assert!(serde_json::from_str::<DesktopChatScope>(
        r#"{"mode":"exact","kind":"all","scope_id":null,"normalize":true}"#
    )
    .is_err());
    let mut bad = serde_json::to_value(DesktopChatsReply::Header(header("chat", "1"))).unwrap();
    bad["data"]["content"] = serde_json::json!("not header metadata");
    assert!(serde_json::from_value::<DesktopChatsReply>(bad).is_err());
}

#[test]
fn catalog_pages_correlate_rows_queries_and_anchors_without_false_timestamp_ordering() {
    let mut request = list_request();
    if let DesktopChatsRequest::List { cursor, .. } = &mut request {
        *cursor = Some(DesktopChatListCursor {
            library_scope: scope(),
            catalog_revision: "catalog-revision".into(),
            query_hash: hash(),
            after_rowid: "9".into(),
        });
    }
    let original = list_page();
    assert!(DesktopChatsReply::List(original.clone()).valid_for(&request));
    for fault in 0..14 {
        let mut bad = original.clone();
        match fault {
            0 => bad.library_scope = "c".repeat(32),
            1 => bad.catalog_revision = "other-catalog".into(),
            2 => bad.items[1].id = bad.items[0].id.clone(),
            3 => bad.items[1].rowid = bad.items[0].rowid.clone(),
            4 => bad.items[0].rowid = "+2".into(),
            5 => bad.items[0].rowid = "9".into(),
            6 => bad.next.as_mut().unwrap().after_rowid = "99".into(),
            7 => bad.next.as_mut().unwrap().library_scope = "c".repeat(32),
            8 => bad.next.as_mut().unwrap().catalog_revision = "other-catalog".into(),
            9 => bad.next.as_mut().unwrap().query_hash = "c".repeat(64),
            10 => bad.items.clear(),
            11 => bad.items[0].metadata_revision.clear(),
            12 => bad.items[0].title_bytes = 0,
            _ => {
                bad.items.clear();
                bad.next = None;
            }
        }
        assert!(
            !DesktopChatsReply::List(bad).valid_for(&request),
            "fault {fault}"
        );
    }
    let mut exact = list_request();
    if let DesktopChatsRequest::List { scope, .. } = &mut exact {
        *scope = DesktopChatScope::Exact {
            kind: "meeting".into(),
            scope_id: None,
        };
    }
    assert!(DesktopChatsReply::List(original.clone()).valid_for(&exact));
    if let DesktopChatsRequest::List {
        scope: DesktopChatScope::Exact { scope_id, .. },
        ..
    } = &mut exact
    {
        *scope_id = Some(String::new());
    }
    assert!(!DesktopChatsReply::List(original).valid_for(&exact));
}

#[test]
fn message_pages_pin_chat_generation_revision_and_last_row() {
    let mut request = messages_request();
    if let DesktopChatsRequest::Messages { cursor, .. } = &mut request {
        *cursor = Some(DesktopChatMessagesCursor {
            library_scope: scope(),
            chat_id: "chat".into(),
            generation: "chat-generation".into(),
            messages_revision: "messages-revision".into(),
            query_hash: hash(),
            after_rowid: "-5".into(),
        });
    }
    let original = messages_page();
    assert!(DesktopChatsReply::Messages(original.clone()).valid_for(&request));
    for fault in 0..15 {
        let mut bad = original.clone();
        match fault {
            0 => bad.chat_id = "other-chat".into(),
            1 => bad.generation = "other-generation".into(),
            2 => bad.messages_revision = "other-revision".into(),
            3 => bad.items[0].chat_id = "other-chat".into(),
            4 => bad.items[1].id = bad.items[0].id.clone(),
            5 => bad.items[1].rowid = bad.items[0].rowid.clone(),
            6 => bad.items[0].rowid = "-5".into(),
            7 => bad.next.as_mut().unwrap().chat_id = "other-chat".into(),
            8 => bad.next.as_mut().unwrap().generation = "other-generation".into(),
            9 => bad.next.as_mut().unwrap().messages_revision = "other-revision".into(),
            10 => bad.next.as_mut().unwrap().after_rowid = "2".into(),
            11 => bad.next.as_mut().unwrap().query_hash = "c".repeat(64),
            12 => bad.items.clear(),
            13 => bad.items[0].role_bytes = 0,
            _ => {
                bad.items.clear();
                bad.next = None;
            }
        }
        assert!(
            !DesktopChatsReply::Messages(bad).valid_for(&request),
            "fault {fault}"
        );
    }
}

#[test]
fn chat_field_continuations_pin_utf8_byte_totals_presence_and_progress() {
    let mut request = field_request();
    let original = field_page();
    assert!(DesktopChatsReply::ChatField(original.clone()).valid_for(&request));
    for fault in 0..14 {
        let mut bad = original.clone();
        match fault {
            0 => bad.chat_id = "other-chat".into(),
            1 => bad.generation = "other-generation".into(),
            2 => bad.metadata_revision = "other-revision".into(),
            3 => bad.field = DesktopChatField::Title,
            4 => bad.byte_offset = 1,
            5 => bad.present = false,
            6 => bad.next = None,
            7 => bad.text.clear(),
            8 => bad.next.as_mut().unwrap().byte_offset += 1,
            9 => bad.next.as_mut().unwrap().total_bytes += 1,
            10 => bad.next.as_mut().unwrap().present = false,
            11 => bad.next.as_mut().unwrap().max_bytes = 1024,
            12 => bad.next.as_mut().unwrap().generation = "other-generation".into(),
            _ => bad.total_bytes = u64::MAX,
        }
        assert!(
            !DesktopChatsReply::ChatField(bad).valid_for(&request),
            "fault {fault}"
        );
    }
    if let DesktopChatsRequest::ReadChatField { cursor, .. } = &mut request {
        *cursor = original.next.clone();
    }
    let mut final_page = original.clone();
    final_page.byte_offset = original.text.len() as u64;
    final_page.text = "end".into();
    final_page.next = None;
    assert!(DesktopChatsReply::ChatField(final_page.clone()).valid_for(&request));
    final_page.total_bytes += 1;
    final_page.text.push('!');
    assert!(
        !DesktopChatsReply::ChatField(final_page).valid_for(&request),
        "continuation cannot change the original total"
    );
    let mut phantom = original;
    phantom.byte_offset = phantom.text.len() as u64;
    phantom.text.clear();
    phantom.total_bytes = phantom.byte_offset;
    phantom.next = None;
    assert!(!DesktopChatsReply::ChatField(phantom).valid_for(&request));
}

#[test]
fn null_scope_empty_scope_and_empty_assistant_fields_remain_distinct_real_values() {
    let mut page = field_page();
    page.text.clear();
    page.total_bytes = 0;
    page.next = None;
    page.present = false;
    assert!(DesktopChatsReply::ChatField(page.clone()).valid_for(&field_request()));
    page.present = true;
    assert!(DesktopChatsReply::ChatField(page.clone()).valid_for(&field_request()));
    let mut request = field_request();
    if let DesktopChatsRequest::ReadChatField { field, .. } = &mut request {
        *field = DesktopChatField::Title;
    }
    page.field = DesktopChatField::Title;
    page.present = false;
    assert!(!DesktopChatsReply::ChatField(page).valid_for(&request));
    assert!(
        DesktopChatsReply::MessageField(message_field_page("")).valid_for(&message_field_request())
    );
    let mut phantom = message_field_page("");
    phantom.byte_offset = 1;
    phantom.total_bytes = 1;
    assert!(!DesktopChatsReply::MessageField(phantom).valid_for(&message_field_request()));
}

#[test]
fn message_fields_preserve_opaque_citations_and_pin_all_continuation_identities() {
    for raw in [
        "not JSON\0🙂",
        "null",
        "true",
        "{}",
        "[1,\"x\"]",
        "\"a JSON string\"",
    ] {
        assert!(DesktopChatsReply::MessageField(message_field_page(raw))
            .valid_for(&message_field_request()));
    }
    let mut page = message_field_page("я\0🦀");
    page.total_bytes += 3;
    page.next = Some(DesktopChatMessageFieldCursor {
        library_scope: page.library_scope.clone(),
        chat_id: page.chat_id.clone(),
        message_id: page.message_id.clone(),
        chat_generation: page.chat_generation.clone(),
        generation: page.generation.clone(),
        revision: page.revision.clone(),
        field: page.field,
        byte_offset: page.text.len() as u64,
        total_bytes: page.total_bytes,
        max_bytes: MAX_CHAT_DATA_BYTES as u32,
    });
    let mut request = message_field_request();
    assert!(DesktopChatsReply::MessageField(page.clone()).valid_for(&request));
    for fault in 0..11 {
        let mut bad = page.clone();
        match fault {
            0 => bad.chat_id = "other-chat".into(),
            1 => bad.message_id = "other-message".into(),
            2 => bad.chat_generation = "other-chat-generation".into(),
            3 => bad.generation = "other-message-generation".into(),
            4 => bad.revision = "other-revision".into(),
            5 => bad.next.as_mut().unwrap().message_id = "other-message".into(),
            6 => bad.next.as_mut().unwrap().chat_generation = "other-generation".into(),
            7 => bad.next.as_mut().unwrap().total_bytes += 1,
            8 => bad.next.as_mut().unwrap().byte_offset = 0,
            9 => bad.next = None,
            _ => bad.text.clear(),
        }
        assert!(
            !DesktopChatsReply::MessageField(bad).valid_for(&request),
            "fault {fault}"
        );
    }
    if let DesktopChatsRequest::ReadMessageField { cursor, .. } = &mut request {
        *cursor = page.next.clone();
    }
    page.byte_offset = page.text.len() as u64;
    page.text = "end".into();
    page.next = None;
    assert!(DesktopChatsReply::MessageField(page.clone()).valid_for(&request));
    page.total_bytes += 1;
    page.text.push('!');
    assert!(!DesktopChatsReply::MessageField(page).valid_for(&request));
}

#[test]
fn encoded_reply_budget_includes_tags_metadata_and_json_escaping() {
    let request = message_field_request();
    let mut page = message_field_page("");
    page.total_bytes = MAX_CHAT_DATA_BYTES as u64;
    let overhead = serde_json::to_vec(&DesktopChatsReply::MessageField(page.clone()))
        .unwrap()
        .len();
    page.text = "x".repeat(MAX_CHAT_DATA_BYTES - overhead);
    page.total_bytes = page.text.len() as u64;
    let reply = DesktopChatsReply::MessageField(page.clone());
    assert_eq!(
        serde_json::to_vec(&reply).unwrap().len(),
        MAX_CHAT_DATA_BYTES
    );
    assert!(reply.valid_for(&request));
    page.text.push('x');
    page.total_bytes += 1;
    assert!(!DesktopChatsReply::MessageField(page).valid_for(&request));
    let escaped = "\0".repeat(12_000);
    assert!(!DesktopChatsReply::MessageField(message_field_page(&escaped)).valid_for(&request));
    let mut request = list_request();
    if let DesktopChatsRequest::List { max_bytes, .. } = &mut request {
        *max_bytes = MAX_CHAT_DATA_BYTES as u32 + 1;
    }
    assert!(!request.valid());
    if let DesktopChatsRequest::List { max_bytes, .. } = &mut request {
        *max_bytes = 1023;
    }
    assert!(!request.valid());
}

#[test]
fn debug_hides_metadata_previews_scopes_and_message_text() {
    let private = "PRIVATE_RAW_METADATA_Орион\0🙂";
    let mut header = header("chat", "1");
    header.title_preview = private.into();
    header.title_bytes = private.len() as u64;
    header.scope_kind_preview = private.into();
    header.scope_kind_bytes = private.len() as u64;
    header.scope_id_preview = Some(private.into());
    header.scope_id_bytes = Some(private.len() as u64);
    let request = DesktopChatsRequest::List {
        scope: DesktopChatScope::Exact {
            kind: private.into(),
            scope_id: Some(private.into()),
        },
        limit: 1,
        max_bytes: MAX_CHAT_DATA_BYTES as u32,
        cursor: None,
    };
    for debug in [
        format!("{request:?}"),
        format!("{:?}", DesktopChatsReply::Header(header)),
        format!(
            "{:?}",
            DesktopChatsReply::MessageField(message_field_page(private))
        ),
    ] {
        assert!(!debug.contains("PRIVATE_RAW_METADATA"));
    }
    assert!(!format!("{:?}", message("message", "1")).contains("custom"));
}

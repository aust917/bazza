#![cfg(any(windows, target_os = "macos"))]
//! Bounded chat history over owned service processes and synthetic data only.
use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::chats::*;
use bazza_service_client::{ClientError, RepositoryClient, ServiceClient};
use bazza_service_test_support::{create_v8, READY_MEETING_ID};
use rusqlite::{params, Connection};
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{sleep, timeout, Instant};

struct Harness {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    child: Option<Child>,
    title: String,
    kind: String,
    scope: String,
    role: String,
    content: String,
    citations: String,
}
impl Harness {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("chat history fixture Орион");
        create_v8(&root).unwrap();
        let title = format!("  {}\0 exact title  ", "Орион🦀 ".repeat(10_000));
        let kind = format!("legacy-{}\0", "вид🙂".repeat(5000));
        let scope = format!(" {}\0scope ", "ключ🦀".repeat(10_000));
        let role = format!("custom-{}\0", "роль🙂".repeat(10_000));
        let content = format!(
            "  {}\0 exact body  ",
            "LargeChatAquamarine Орион🦀 ".repeat(10_000)
        );
        let citations = format!(
            r#"{{"custom":"{}","meeting_id":"{}"}}"#,
            "Орион🙂".repeat(10_000),
            READY_MEETING_ID
        );
        {
            // Seed legacy rows before explicit service migration, with no live
            // service or GUI using this disposable library.
            let conn = Connection::open(root.join("bazza.db")).unwrap();
            conn.execute_batch("DELETE FROM chat_messages; DELETE FROM chats;")
                .unwrap();
            for (id, scope_kind, scope_id, name, created) in [
                (
                    "meeting-none",
                    "meeting",
                    None,
                    "None scope",
                    "2026-11-02T10:00:00+05:00",
                ),
                (
                    "all-some",
                    "all",
                    Some(READY_MEETING_ID),
                    "Library scope with ID",
                    "2026-11-02T07:00:00Z",
                ),
                (
                    "meeting-some",
                    "meeting",
                    Some(READY_MEETING_ID),
                    "Scoped meeting",
                    "2026-11-02T06:00:00Z",
                ),
                (
                    "meeting-empty",
                    "meeting",
                    Some(""),
                    "Empty scope",
                    "2026-11-02T06:00:00Z",
                ),
                (
                    "all-nul",
                    "all",
                    Some("\0Орион"),
                    "NUL scope",
                    "2026-11-02T05:00:00Z",
                ),
                (
                    "meeting-space",
                    "meeting",
                    Some(" \t "),
                    "Whitespace scope",
                    "2026-11-02T04:00:00Z",
                ),
                (
                    "orphan-chat",
                    "meeting",
                    Some("missing-meeting"),
                    "Orphan history",
                    "2026-11-02T03:00:00Z",
                ),
                (
                    "giant-chat",
                    kind.as_str(),
                    Some(scope.as_str()),
                    title.as_str(),
                    "2026-11-02T02:00:00Z",
                ),
            ] {
                conn.execute("INSERT INTO chats(id,scope_kind,scope_id,title,created_at) VALUES(?1,?2,?3,?4,?5)", params![id,scope_kind,scope_id,name,created]).unwrap();
            }
            for (id, message_role, text, stored, created) in [
                (
                    "later-row-first",
                    role.as_str(),
                    content.as_str(),
                    citations.as_str(),
                    "2026-11-02T10:00:00+05:00",
                ),
                (
                    "offset-before",
                    "user",
                    "offset lexical order",
                    "[]",
                    "2026-11-02T07:00:00Z",
                ),
                (
                    "tie-z",
                    "assistant",
                    "first inserted tie",
                    "not valid JSON\0",
                    "2026-11-02T11:00:00Z",
                ),
                (
                    "tie-a",
                    "assistant",
                    "second inserted tie",
                    "42",
                    "2026-11-02T11:00:00Z",
                ),
            ] {
                conn.execute("INSERT INTO chat_messages(id,chat_id,role,content,citations_json,created_at) VALUES(?1,'giant-chat',?2,?3,?4,?5)",params![id,message_role,text,stored,created]).unwrap();
            }
            for chat in ["meeting-none", "all-some", "meeting-some", "orphan-chat"] {
                conn.execute("INSERT INTO chat_messages(id,chat_id,role,content,citations_json,created_at) VALUES(?1,?2,'assistant','','[]','2026-11-02T12:00:00Z')",params![format!("pending-{chat}"),chat]).unwrap();
            }
        }
        drop(Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap());
        Self {
            _temporary: temporary,
            root,
            child: None,
            title,
            kind,
            scope,
            role,
            content,
            citations,
        }
    }
    async fn start(&mut self) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-service"));
        command
            .args(["serve-fixture", "--library"])
            .arg(&self.root)
            .args(["--idle-timeout-ms", "10000"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        assert!(self.child.is_none());
        self.child = Some(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if ServiceClient::connect(&self.root).await.is_ok() {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                let mut error = String::new();
                if let Some(mut stderr) = self.child.as_mut().unwrap().stderr.take() {
                    stderr.read_to_string(&mut error).unwrap();
                }
                panic!("owned chat service exited ({status}): {error}");
            }
            assert!(
                Instant::now() < deadline,
                "owned chat service readiness timeout"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }
    async fn stop(&mut self) {
        let mut client = ServiceClient::connect(&self.root).await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    assert!(status.success());
                    self.child.take().unwrap().wait().unwrap();
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn raw_snapshot(root: &std::path::Path) -> serde_json::Value {
    let conn = Connection::open(root.join("bazza.db")).unwrap();
    let chats = conn
        .prepare("SELECT id,scope_kind,scope_id,title,created_at FROM chats ORDER BY rowid")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let messages = conn.prepare("SELECT id,chat_id,role,content,citations_json,created_at FROM chat_messages ORDER BY rowid").unwrap()
        .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    serde_json::json!({"chats":chats,"messages":messages})
}

async fn chats(repository: &RepositoryClient, scope: DesktopChatScope) -> Vec<DesktopChatHeader> {
    let mut page = repository
        .chats(DesktopChatsRequest::List {
            scope,
            limit: 1,
            max_bytes: 4096,
            cursor: None,
        })
        .await
        .unwrap();
    let mut headers = Vec::new();
    loop {
        let DesktopChatsReply::List(data) = page.data else {
            panic!("chat list expected")
        };
        headers.extend(data.items);
        if let Some(next) = page.next {
            page = repository.continue_chats(next).await.unwrap();
        } else {
            return headers;
        }
    }
}

async fn field(repository: &RepositoryClient, request: DesktopChatsRequest) -> (bool, String) {
    let mut page = repository.chats(request).await.unwrap();
    let mut text = String::new();
    let mut pinned = None;
    let mut present = None;
    loop {
        let (part, offset, total, exists) = match page.data {
            DesktopChatsReply::ChatField(data) => {
                (data.text, data.byte_offset, data.total_bytes, data.present)
            }
            DesktopChatsReply::MessageField(data) => {
                (data.text, data.byte_offset, data.total_bytes, true)
            }
            _ => panic!("field page expected"),
        };
        assert_eq!(offset, text.len() as u64);
        assert_eq!(*pinned.get_or_insert(total), total);
        assert_eq!(*present.get_or_insert(exists), exists);
        text.push_str(&part);
        if let Some(next) = page.next {
            page = repository.continue_chats(next).await.unwrap();
        } else {
            assert_eq!(text.len() as u64, total);
            return (exists, text);
        }
    }
}

fn chat_field(header: &DesktopChatHeader, field: DesktopChatField) -> DesktopChatsRequest {
    DesktopChatsRequest::ReadChatField {
        chat_id: header.id.clone(),
        expected_generation: header.generation.clone(),
        expected_metadata_revision: header.metadata_revision.clone(),
        field,
        cursor: None,
        max_bytes: 4096,
    }
}
fn message_field(
    chat: &DesktopChatHeader,
    message: &DesktopChatMessageHeader,
    field: DesktopChatMessageField,
) -> DesktopChatsRequest {
    DesktopChatsRequest::ReadMessageField {
        chat_id: chat.id.clone(),
        message_id: message.id.clone(),
        expected_chat_generation: chat.generation.clone(),
        expected_generation: message.generation.clone(),
        expected_revision: message.revision.clone(),
        field,
        cursor: None,
        max_bytes: 4096,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn giant_legacy_chats_preserve_sql_order_scopes_and_exact_fragments_over_real_ipc() {
    timeout(Duration::from_secs(45), async {
        let mut h = Harness::new();
        let before = raw_snapshot(&h.root);
        h.start().await;
        let repository = RepositoryClient::connect(&h.root).await.unwrap();
        let all = chats(&repository, DesktopChatScope::AllCatalog).await;
        assert_eq!(all.iter().map(|chat| chat.id.as_str()).collect::<Vec<_>>(), vec![
            "meeting-none", "all-some", "meeting-some", "meeting-empty", "all-nul", "meeting-space", "orphan-chat", "giant-chat"
        ]);
        for (kind, scope_id, expected) in [
            ("meeting", None, "meeting-none"),
            ("all", Some(READY_MEETING_ID), "all-some"),
            ("meeting", Some(READY_MEETING_ID), "meeting-some"),
            ("meeting", Some(""), "meeting-empty"),
            ("all", Some("\0Орион"), "all-nul"),
            ("meeting", Some(" \t "), "meeting-space"),
        ] {
            let result = chats(&repository, DesktopChatScope::Exact { kind: kind.into(), scope_id: scope_id.map(str::to_owned) }).await;
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].id, expected);
        }
        let giant = all.iter().find(|chat| chat.id == "giant-chat").unwrap();
        assert_eq!(giant.title_bytes, h.title.len() as u64);
        assert!(giant.title_preview.len() <= MAX_CHAT_PREVIEW_BYTES);
        assert_eq!(field(&repository, chat_field(giant, DesktopChatField::Title)).await, (true,h.title.clone()));
        assert_eq!(field(&repository, chat_field(giant, DesktopChatField::ScopeKind)).await, (true,h.kind.clone()));
        assert_eq!(field(&repository, chat_field(giant, DesktopChatField::ScopeId)).await, (true,h.scope.clone()));
        let alike = chats(&repository, DesktopChatScope::LikeChat { chat_id: giant.id.clone(), generation: giant.generation.clone() }).await;
        assert_eq!(alike.len(), 1);
        assert_eq!(alike[0], *giant);
        let none = all.iter().find(|chat|chat.id == "meeting-none").unwrap();
        let empty = all.iter().find(|chat|chat.id == "meeting-empty").unwrap();
        assert_eq!(field(&repository, chat_field(none, DesktopChatField::ScopeId)).await, (false,String::new()));
        assert_eq!(field(&repository, chat_field(empty, DesktopChatField::ScopeId)).await, (true,String::new()));
        let mut page = repository.chats(DesktopChatsRequest::Messages {
            chat_id: giant.id.clone(), expected_generation: giant.generation.clone(),
            expected_messages_revision: giant.messages_revision.clone(), limit: 1, max_bytes: 4096, cursor: None,
        }).await.unwrap();
        let mut messages = Vec::new();
        loop {
            let DesktopChatsReply::Messages(data) = page.data else { panic!("message list expected") };
            messages.extend(data.items);
            if let Some(next) = page.next { page = repository.continue_chats(next).await.unwrap(); } else { break; }
        }
        assert_eq!(messages.iter().map(|message|message.id.as_str()).collect::<Vec<_>>(), vec!["offset-before","later-row-first","tie-z","tie-a"]);
        assert!(messages[0].rowid.parse::<i64>().unwrap() > messages[1].rowid.parse::<i64>().unwrap(), "rowids need not increase across raw timestamps");
        assert!(messages[0].created_at > messages[1].created_at, "UTC-normalized timestamps do not define the native SQL order");
        let giant_message = messages.iter().find(|message| message.id == "later-row-first").unwrap();
        assert_eq!(field(&repository, message_field(giant, giant_message, DesktopChatMessageField::Role)).await.1, h.role);
        assert_eq!(field(&repository, message_field(giant, giant_message, DesktopChatMessageField::Content)).await.1, h.content);
        assert_eq!(field(&repository, message_field(giant, giant_message, DesktopChatMessageField::CitationsStoredJson)).await.1, h.citations);
        for (id, stored) in [("tie-z", "not valid JSON\0"), ("tie-a", "42")] {
            let message = messages.iter().find(|message|message.id == id).unwrap();
            let assembled = field(&repository, message_field(giant, message, DesktopChatMessageField::CitationsStoredJson)).await.1;
            assert_eq!(assembled, stored);
            let parsed = serde_json::from_str::<serde_json::Value>(&assembled).unwrap_or_else(|_|serde_json::json!([]));
            assert_eq!(parsed, if id == "tie-z" {serde_json::json!([])} else {serde_json::json!(42)});
        }
        let stale = repository.chats(chat_field(giant, DesktopChatField::Title)).await.unwrap().next.unwrap();
        h.stop().await;
        assert_eq!(raw_snapshot(&h.root), before, "reads and migration preserve exact legacy chat/history bytes");
        h.start().await;
        repository.reconnect().await.unwrap();
        let error = repository.continue_chats(stale).await.unwrap_err();
        assert!(matches!(error,ClientError::Remote(error) if error.code == bazza_service_api::rpc::ErrorCode::ResyncRequired));
        drop(repository);
        h.stop().await;
        assert_eq!(raw_snapshot(&h.root), before);
    }).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn erasure_rotates_pending_global_chat_fences_while_retaining_history_over_reopen() {
    timeout(Duration::from_secs(45), async {
        for whole_meeting in [false, true] {
            let mut h = Harness::new();
            let before = raw_snapshot(&h.root);
            h.start().await;
            let client = RepositoryClient::connect(&h.root).await.unwrap();
            let old = chats(&client, DesktopChatScope::AllCatalog).await;
            drop(client);
            h.stop().await;
            {
                // The service has exited and released ownership before the
                // trusted domain operation. No SQLite is opened beside it.
                let repository =
                    Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
                let context = bazza_core::data::MutationContext {
                    principal: "fixture-chat-erasure".into(),
                    idempotency_key: format!("erasure-{}", uuid::Uuid::new_v4()),
                };
                if whole_meeting {
                    let meeting = repository
                        .data()
                        .meeting(READY_MEETING_ID)
                        .unwrap()
                        .unwrap();
                    repository
                        .data()
                        .delete_meeting(&context, READY_MEETING_ID, &meeting.lifecycle_revision)
                        .unwrap();
                } else {
                    let state = repository
                        .data()
                        .transcript_state(READY_MEETING_ID)
                        .unwrap();
                    repository
                        .data()
                        .delete_transcript(&context, READY_MEETING_ID, &state.revision)
                        .unwrap();
                }
            }
            h.start().await;
            let client = RepositoryClient::connect(&h.root).await.unwrap();
            let current = chats(&client, DesktopChatScope::AllCatalog).await;
            assert_eq!(current.len(), old.len());
            for id in [
                "meeting-none",
                "all-some",
                "all-nul",
                "meeting-some",
                "giant-chat",
            ] {
                let was = old.iter().find(|chat| chat.id == id).unwrap();
                let now = current.iter().find(|chat| chat.id == id).unwrap();
                assert_eq!(now.generation, was.generation);
                assert_eq!(now.metadata_revision, was.metadata_revision);
                assert_ne!(now.messages_revision, was.messages_revision);
                assert_ne!(
                    now.erasure_revision, was.erasure_revision,
                    "empty pending [] still needs a completion fence"
                );
            }
            for id in ["orphan-chat", "meeting-empty", "meeting-space"] {
                assert_eq!(
                    current.iter().find(|chat| chat.id == id),
                    old.iter().find(|chat| chat.id == id)
                );
            }
            for id in ["meeting-none", "all-some", "meeting-some"] {
                let header = current.iter().find(|chat| chat.id == id).unwrap();
                let page = client
                    .chats(DesktopChatsRequest::Messages {
                        chat_id: id.into(),
                        expected_generation: header.generation.clone(),
                        expected_messages_revision: header.messages_revision.clone(),
                        limit: 10,
                        max_bytes: 4096,
                        cursor: None,
                    })
                    .await
                    .unwrap();
                let DesktopChatsReply::Messages(page) = page.data else {
                    panic!("messages expected")
                };
                assert_eq!(page.items.len(), 1);
                let message = &page.items[0];
                assert_eq!(
                    field(
                        &client,
                        message_field(header, message, DesktopChatMessageField::Content)
                    )
                    .await
                    .1,
                    ""
                );
                assert_eq!(
                    field(
                        &client,
                        message_field(
                            header,
                            message,
                            DesktopChatMessageField::CitationsStoredJson
                        )
                    )
                    .await
                    .1,
                    "[]"
                );
            }
            drop(client);
            h.stop().await;
            assert_eq!(
                raw_snapshot(&h.root),
                before,
                "erase fences do not delete chat text or history"
            );
        }
    })
    .await
    .unwrap();
}

#![cfg(any(windows, target_os = "macos"))]

use bazza_core::{
    data::{MutationContext, NewNote},
    MigrationPolicy, Repository,
};
use bazza_service_api::{
    external_read::{IssueExternalReadGrant, RevokeExternalReadGrant},
    validate_tool_input, validate_tool_output_for_input,
};
use bazza_service_client::ServiceClient;
use bazza_service_test_support::{create_v8, READY_MEETING_ID};
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
    time::{sleep, timeout},
};

const DEADLINE: Duration = Duration::from_secs(10);

fn adapter(root: &Path) -> Command {
    adapter_mode(root, "serve-fixture")
}

fn adapter_mode(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-mcp"));
    command
        .args([mode, "--library"])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: u64,
    modern: bool,
}

impl Session {
    async fn initialize(root: &Path, revision: &str) -> Self {
        Self::initialize_child(adapter(root), revision).await
    }

    async fn initialize_child(mut command: Command, revision: &str) -> Self {
        let mut child = command.spawn().unwrap();
        let mut session = Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            id: 0,
            modern: revision == "2026-07-28",
        };
        let response = if session.modern {
            let response = session.request("server/discover", json!({})).await;
            assert_eq!(response["result"]["resultType"], "complete", "{response}");
            assert!(response["result"]["supportedVersions"]
                .as_array()
                .is_some_and(|versions| versions.contains(&json!(revision))));
            response
        } else {
            let response = session
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": revision,
                        "capabilities": {},
                        "clientInfo": {"name": "Bazza stdio integration test", "version": "1"}
                    }),
                )
                .await;
            assert_eq!(
                response["result"]["protocolVersion"], revision,
                "{response}"
            );
            session
                .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await;
            response
        };
        assert!(response["result"]["capabilities"]["tools"].is_object());
        session
    }

    async fn send(&mut self, value: Value) {
        let line = format!("{value}\n");
        timeout(DEADLINE, self.input.write_all(line.as_bytes()))
            .await
            .expect("write to MCP child timed out")
            .unwrap();
    }

    async fn request(&mut self, method: &str, mut params: Value) -> Value {
        if self.modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "Bazza stdio integration test", "version": "1"
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            });
        }
        self.id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}))
            .await;
        let mut line = String::new();
        let bytes = timeout(DEADLINE, self.output.read_line(&mut line))
            .await
            .expect("MCP response timed out")
            .unwrap();
        assert_ne!(bytes, 0, "MCP child exited without a response to {method}");
        let reply: Value = serde_json::from_str(&line).expect("stdout must be JSON-RPC");
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], self.id);
        reply
    }

    async fn tool(&mut self, name: &str, arguments: Value) -> Value {
        validate_tool_input(name, &arguments).unwrap();
        let reply = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        let result = &reply["result"];
        let structured = &result["structuredContent"];
        validate_tool_output_for_input(name, &arguments, structured)
            .unwrap_or_else(|error| panic!("{name}: {error}: {reply}"));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *structured);
        assert_eq!(
            result["isError"].as_bool().unwrap_or_default(),
            structured.get("error").is_some()
        );
        structured.clone()
    }

    async fn eof(self) {
        let Self {
            mut child,
            input,
            mut output,
            ..
        } = self;
        drop(input);
        let exit = timeout(Duration::from_secs(3), child.wait())
            .await
            .expect("adapter did not exit on stdin EOF")
            .unwrap();
        assert!(exit.success(), "adapter exited with {exit}");
        assert!(output.fill_buf().await.unwrap().is_empty());
    }
}

async fn start_service(root: &Path) -> JoinHandle<()> {
    start_service_mode(root, false).await
}

async fn start_service_mode(root: &Path, external: bool) -> JoinHandle<()> {
    let owned_root = root.to_owned();
    let service = tokio::spawn(async move {
        let idle = Duration::from_secs(30);
        if external {
            bazza_service::serve_fixture_with_external_read(&owned_root, idle)
                .await
                .unwrap();
        } else {
            bazza_service::serve_fixture(&owned_root, idle)
                .await
                .unwrap();
        }
    });
    timeout(DEADLINE, async {
        while ServiceClient::connect(root).await.is_err() {
            assert!(!service.is_finished(), "fixture service failed at startup");
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture service readiness timed out");
    service
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn granted_stdio_reads_over_external_pipe_and_loses_access_after_revoke() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("synthetic granted library");
    create_v8(&root).unwrap();
    drop(Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap());
    let service = start_service_mode(&root, true).await;
    let mut admin = ServiceClient::connect(&root).await.unwrap();
    let grant = admin
        .issue_fixture_external_read_grant(IssueExternalReadGrant {
            client_label: "stdio acceptance".into(),
        })
        .await
        .unwrap();
    let mut command = adapter_mode(&root, "serve-fixture-granted");
    command
        .arg("--connection-id")
        .arg(grant.connection_id.to_string())
        .env("BAZZA_MCP_READ_SECRET", grant.secret.expose());
    let mut session = Session::initialize_child(command, "2026-07-28").await;
    let arguments = json!({"meeting_id": READY_MEETING_ID});
    let allowed = session.tool("bazza_get_meeting", arguments.clone()).await;
    assert_eq!(allowed["data"]["meeting_id"], READY_MEETING_ID);
    admin
        .revoke_fixture_external_read_grant(RevokeExternalReadGrant {
            connection_id: grant.connection_id,
        })
        .await
        .unwrap();
    let denied = session.tool("bazza_get_meeting", arguments).await;
    assert_eq!(denied["error"]["code"], "ACCESS_REVOKED");
    assert_eq!(denied["library_id"], grant.library_id.to_string());
    assert!(denied.get("data").is_none());
    session.eof().await;
    drop(admin);
    stop_service(&root, service).await;
}

async fn stop_service(root: &Path, service: JoinHandle<()>) {
    let mut client = ServiceClient::connect(root).await.unwrap();
    client.shutdown().await.unwrap();
    drop(client);
    timeout(DEADLINE, service).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_reads_owned_fixture_and_keeps_service_alive_after_eof() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("synthetic library Орион");
    create_v8(&root).unwrap();
    let library = {
        let owner = Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap();
        owner
            .data()
            .create_note(
                &MutationContext {
                    principal: "fixture-test".into(),
                    idempotency_key: "stdio-fixture-note".into(),
                },
                &NewNote {
                    meeting_id: Some(READY_MEETING_ID.into()),
                    title: "Synthetic planning note".into(),
                    markdown: "The synthetic CeruleanAnchor is only in this note. Орион 🛰".into(),
                },
            )
            .unwrap();
        owner
            .data()
            .create_note(
                &MutationContext {
                    principal: "fixture-test".into(),
                    idempotency_key: "stdio-fixture-standalone-note".into(),
                },
                &NewNote {
                    meeting_id: None,
                    title: "Synthetic standalone note".into(),
                    markdown: String::new(),
                },
            )
            .unwrap();
        owner.status().library_id.to_string()
    };
    let service = start_service(&root).await;

    let mut modern = Session::initialize(&root, "2026-07-28").await;
    let response = modern.request("tools/list", json!({})).await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"bazza_get_status"));
    assert!(names.contains(&"bazza_list_meetings"));
    assert!(names.contains(&"bazza_get_meeting"));
    assert!(names.contains(&"bazza_search"));
    assert!(names.contains(&"bazza_get_transcript"));
    assert!(names.contains(&"bazza_get_summary"));
    assert!(names.contains(&"bazza_get_library_structure"));
    assert!(names.contains(&"bazza_list_notes"));
    assert!(names.contains(&"bazza_get_note"));
    assert!(tools.iter().all(|tool| {
        tool["inputSchema"]["type"] == "object"
            && tool["outputSchema"]["type"] == "object"
            && tool["annotations"]["readOnlyHint"] == true
    }));
    assert!(!names.contains(&"bazza_delete_meeting"));
    let blocked = modern
        .request(
            "tools/call",
            json!({"name":"bazza_delete_meeting","arguments":{"meeting_id": READY_MEETING_ID}}),
        )
        .await;
    assert!(blocked.get("error").is_some() || blocked["result"]["isError"] == true);

    let status = modern.tool("bazza_get_status", json!({})).await;
    assert_eq!(status["library_id"], library);
    assert_eq!(status["data"]["available_tools"], json!(names));
    let first = modern.tool("bazza_list_meetings", json!({"limit":1})).await;
    let next = first["data"]["page"]["next_cursor"].as_str().unwrap();
    let second = modern
        .tool("bazza_list_meetings", json!({"limit":1, "cursor":next}))
        .await;
    assert_ne!(first["data"]["items"][0], second["data"]["items"][0]);
    let meeting = modern
        .tool("bazza_get_meeting", json!({"meeting_id":READY_MEETING_ID}))
        .await;
    assert_eq!(meeting["data"]["meeting_id"], READY_MEETING_ID);
    assert_eq!(meeting["data"]["tag_ids"], json!(["fixture-tag"]));
    assert_eq!(meeting["data"]["title"], "Орион / Budget review");
    let structure = modern.tool("bazza_get_library_structure", json!({})).await;
    assert_eq!(
        structure["data"]["folders"][0]["folder_id"],
        "fixture-folder"
    );
    assert_eq!(structure["data"]["tags"][0]["tag_id"], "fixture-tag");
    let mut summary_cursor = Value::Null;
    let mut markdown = String::new();
    for _ in 0..8 {
        let summary = modern
            .tool(
                "bazza_get_summary",
                json!({"meeting_id":READY_MEETING_ID,"max_bytes":16,"cursor":summary_cursor}),
            )
            .await;
        assert_eq!(
            summary["data"]["revision"],
            meeting["data"]["summary"]["revision"]
        );
        markdown.push_str(summary["data"]["content"].as_str().unwrap());
        summary_cursor = summary["data"]["page"]["next_cursor"].clone();
        if summary_cursor.is_null() {
            break;
        }
    }
    assert!(summary_cursor.is_null());
    assert_eq!(markdown, "# Итоги\n\nБюджет: 14. Launch next week.");
    let absent = modern
        .tool("bazza_get_meeting", json!({"meeting_id":"missing-meeting"}))
        .await;
    assert_eq!(absent["error"]["code"], "NOT_FOUND");
    let invalid = modern
        .request(
            "tools/call",
            json!({"name":"bazza_get_meeting","arguments":{"path":"synthetic-path-sentinel"}}),
        )
        .await;
    assert_eq!(invalid["result"]["isError"], true);
    assert_eq!(
        invalid["result"]["structuredContent"]["error"]["code"],
        "INVALID_ARGUMENT"
    );
    assert!(!invalid.to_string().contains("synthetic-path-sentinel"));
    let search = modern
        .tool(
            "bazza_search",
            json!({"query":"Бюджет","fields":["transcript"],"representation":"effective"}),
        )
        .await;
    let hit = &search["data"]["hits"][0];
    assert_eq!(hit["meeting_id"], READY_MEETING_ID);
    assert_eq!(hit["start_ms"], 1000);
    assert!(hit["excerpt"].as_str().unwrap().contains("Бюджет"));
    assert!(!search.to_string().contains("mix.wav"));
    assert!(!search.to_string().contains("bazza.db"));
    let source = hit["source_id"].as_str().unwrap();
    let mut cursor = Value::Null;
    let (mut raw, mut effective) = (String::new(), String::new());
    for _ in 0..20 {
        let transcript = modern
            .tool(
                "bazza_get_transcript",
                json!({
                    "meeting_id":READY_MEETING_ID, "source_id":source,
                    "representation":"both", "max_bytes":16, "limit":1, "cursor":cursor
                }),
            )
            .await;
        let data = &transcript["data"];
        assert_eq!(data["source"]["source_id"], source, "{transcript}");
        let segment = &data["segments"][0];
        if segment["segment_id"] == "102" {
            raw.push_str(segment["raw"].as_str().unwrap());
            effective.push_str(segment["effective"].as_str().unwrap());
            assert_eq!(segment["clean_text_manual"], true);
        }
        let next = &data["page"]["next_cursor"];
        if next.is_null() {
            cursor = Value::Null;
            break;
        }
        assert_ne!(next, &cursor);
        cursor = next.clone();
    }
    assert!(
        cursor.is_null(),
        "the transcript did not finish within 20 pages"
    );
    assert_eq!(raw, "budget maybe fourteen");
    assert_eq!(effective, "Бюджет проекта — 14.");
    for (field, query, kind) in [("title", "Орион", "title"), ("summary", "Итоги", "summary")]
    {
        let search = modern
            .tool("bazza_search", json!({"query":query,"fields":[field]}))
            .await;
        assert_eq!(search["data"]["hits"][0]["kind"], kind);
        assert_eq!(search["data"]["hits"][0]["meeting_id"], READY_MEETING_ID);
    }
    let standalone = modern
        .tool("bazza_list_notes", json!({"scope":{"kind":"standalone"}}))
        .await;
    assert_eq!(standalone["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        standalone["data"]["items"][0]["title"],
        "Synthetic standalone note"
    );
    assert!(standalone["data"]["items"][0]["meeting_id"].is_null());
    let notes = modern
        .tool(
            "bazza_list_notes",
            json!({"scope":{"kind":"meeting","meeting_id":READY_MEETING_ID}}),
        )
        .await;
    assert_eq!(notes["data"]["items"].as_array().unwrap().len(), 1);
    let search = modern
        .tool(
            "bazza_search",
            json!({"query":"CeruleanAnchor","fields":["notes"]}),
        )
        .await;
    let hit = &search["data"]["hits"][0];
    assert_eq!(hit["kind"], "note");
    assert_eq!(hit["meeting_id"], READY_MEETING_ID);
    let note_id = hit["note_id"].as_str().unwrap();
    assert_eq!(notes["data"]["items"][0]["note_id"], note_id);
    let mut note_cursor = Value::Null;
    let mut note_markdown = String::new();
    for _ in 0..8 {
        let note = modern
            .tool(
                "bazza_get_note",
                json!({"note_id":note_id,"max_bytes":12,"cursor":note_cursor}),
            )
            .await;
        assert_eq!(note["data"]["note"]["revision"], hit["source_revision"]);
        note_markdown.push_str(note["data"]["content"].as_str().unwrap());
        note_cursor = note["data"]["page"]["next_cursor"].clone();
        if note_cursor.is_null() {
            break;
        }
    }
    assert!(note_cursor.is_null());
    assert_eq!(
        note_markdown,
        "The synthetic CeruleanAnchor is only in this note. Орион 🛰"
    );
    let no_match = modern
        .tool(
            "bazza_search",
            json!({"query":"Бюджет","fields":["transcript"],"representation":"raw"}),
        )
        .await;
    assert_eq!(no_match["data"]["diagnostics"]["no_matches"], true);

    sleep(Duration::from_millis(5200)).await;
    assert_eq!(
        modern.tool("bazza_get_status", json!({})).await["library_id"],
        library
    );
    modern.eof().await;

    let mut legacy = Session::initialize(&root, "2025-11-25").await;
    assert_eq!(
        legacy.tool("bazza_get_status", json!({})).await["library_id"],
        library
    );
    stop_service(&root, service).await;
    let unavailable = legacy.tool("bazza_get_status", json!({})).await;
    assert_eq!(unavailable["library_id"], library);
    assert_eq!(unavailable["error"]["code"], "SERVICE_UNAVAILABLE");
    let restarted = start_service(&root).await;
    let stale = legacy
        .tool("bazza_list_meetings", json!({"limit":1, "cursor":next}))
        .await;
    assert_eq!(stale["error"]["code"], "CURSOR_STALE");
    assert_eq!(
        legacy.tool("bazza_get_status", json!({})).await["library_id"],
        library
    );
    legacy.eof().await;
    stop_service(&root, restarted).await;
}

#[tokio::test]
async fn ordinary_unmarked_directory_is_refused_before_discovery() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let output = timeout(DEADLINE, adapter(root).output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(root.to_string_lossy().as_ref()));
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

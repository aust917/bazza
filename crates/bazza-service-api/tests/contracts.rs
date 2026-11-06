use bazza_service_api::{
    slice_utf8_page, tool_permission, tool_schemas, validate_tool_input, validate_tool_output,
    validate_tool_output_for_input, ContractValidationError, LibraryPermission, BASE_TOOL_NAMES,
    DEDUP_RETENTION_SECONDS,
};
use serde_json::{json, Value};

const DATE: &str = "2026-11-02T00:00:00Z";

fn segment() -> Value {
    json!({"start_ms":0,"end_ms":1000,"text":"Привет"})
}

fn valid_input(name: &str) -> Value {
    match name {
        "bazza_get_status"
        | "bazza_list_meetings"
        | "bazza_get_library_structure"
        | "bazza_list_notes"
        | "bazza_list_shares" => json!({}),
        "bazza_get_meeting" | "bazza_get_transcript" | "bazza_get_summary" => {
            json!({"meeting_id":"meeting"})
        }
        "bazza_create_meeting" => {
            json!({"idempotency_key":"key","title":"Встреча","started_at":DATE})
        }
        "bazza_update_meeting" => {
            json!({"meeting_id":"meeting","idempotency_key":"key","expected_revision":"r1","patch":{"title":"New title"}})
        }
        "bazza_delete_meeting" | "bazza_delete_transcript" | "bazza_delete_summary" => {
            json!({"meeting_id":"meeting","idempotency_key":"key","expected_revision":"r1"})
        }
        "bazza_import_transcript" | "bazza_replace_transcript" => {
            json!({"meeting_id":"meeting","idempotency_key":"key","expected_revision":"r1","segments":[segment()]})
        }
        "bazza_set_transcript_corrections" => {
            json!({"meeting_id":"meeting","idempotency_key":"key","expected_revision":"r1","corrections":[{"operation":"set","segment_id":"segment","text":""}]})
        }
        "bazza_write_summary" => {
            json!({"meeting_id":"meeting","idempotency_key":"key","expected_revision":"r1","mode":"create","markdown":"# Итоги","structured_json":"{\"budget\":14}"})
        }
        "bazza_get_note" => json!({"note_id":"note"}),
        "bazza_create_note" => json!({"idempotency_key":"key","title":"Заметка","markdown":""}),
        "bazza_update_note" => {
            json!({"note_id":"note","idempotency_key":"key","expected_revision":"r1","patch":{"association":{"operation":"clear"}}})
        }
        "bazza_delete_note" => {
            json!({"note_id":"note","idempotency_key":"key","expected_revision":"r1"})
        }
        "bazza_search" => json!({"query":"Орион"}),
        "bazza_list_calendar_events" => json!({"from":DATE,"to":"2026-11-03T00:00:00Z"}),
        "bazza_refresh_calendar" => json!({"idempotency_key":"key"}),
        "bazza_create_share" => {
            json!({"meeting_id":"meeting","idempotency_key":"key","kind":{"kind":"meeting"},"fields":{"summary":true}})
        }
        "bazza_update_share" => {
            json!({"share_id":"share","idempotency_key":"key","expected_revision":"r1","patch":{"enabled":false}})
        }
        "bazza_delete_share" => {
            json!({"share_id":"share","idempotency_key":"key","expected_revision":"r1"})
        }
        "bazza_get_job" => json!({"job_id":"job"}),
        "bazza_cancel_job" => {
            json!({"job_id":"job","idempotency_key":"key","expected_revision":"r1"})
        }
        _ => panic!("missing input example: {name}"),
    }
}

fn receipt() -> Value {
    json!({"entity_id":"entity","revision":"r2","change_sequence":1,"job_id":null,"dedup_expires_at":"2026-11-09T00:00:00Z","replayed":false})
}

fn page() -> Value {
    json!({"next_cursor":null,"truncated":false})
}
fn text_page(bytes: usize) -> Value {
    json!({"next_cursor":null,"truncated":false,"returned_utf8_bytes":bytes})
}
fn provenance() -> Value {
    json!({"actor_kind":"human","actor_label":null,"source_label":null,"created_at":DATE,"updated_at":DATE})
}
fn state() -> Value {
    json!({"state":"present","revision":"r1","source_id":"source"})
}
fn note() -> Value {
    json!({"note_id":"note","meeting_id":null,"title":"Заметка","revision":"r1","provenance":provenance()})
}

fn meeting() -> Value {
    json!({"meeting_id":"meeting","title":"Встреча","started_at":DATE,"ended_at":null,"duration_ms":1000,"content_kind":"recorded","processing_state":"ready","participants":["Анна"],"folder_id":null,"tag_ids":[],"calendar_uid":null,"metadata_revision":"r1","lifecycle_revision":"r1","transcript":state(),"summary":state(),"media":{"audio":"available","video":"none"}})
}

fn job() -> Value {
    json!({"job_id":"job","revision":"r1","kind":"calendar_refresh","executor_kind":"service","status":"queued","stage":"queued","cancel_requested":false,"cancellable":true,"cancellation_reason":null,"error":null})
}

fn share() -> Value {
    json!({"share_id":"share","meeting_id":"meeting","revision":"r1","kind":{"kind":"meeting"},"mode":"snapshot","fields":{"transcript":null,"summary":true,"note_ids":[],"audio":false,"video":false},"state":"active","expires_at":null,"listener_available":true,"url":null})
}

fn success(data: Value) -> Value {
    json!({"schema_version":1,"library_id":"library","data":data,"warnings":[]})
}

fn error() -> Value {
    json!({"schema_version":1,"library_id":null,"error":{"code":"REVISION_CONFLICT","message":"The state changed","retryable":false,"retry_after_ms":null,"current_revision":"r2"},"warnings":[]})
}

fn valid_output(name: &str) -> Value {
    let data = match name {
        "bazza_get_status" => {
            json!({"service_version":"0.1.0","api_major":1,"api_minor":0,"schema_version":9,"library_epoch":"epoch1","index":{"lexical":"ready","search_generation":"r1","semantic_coverage":"none"},"jobs":{"queued":0,"running":0,"waiting_for_worker":0},"shares":{"active_count":0,"listener_available":false},"available_tools":[]})
        }
        "bazza_list_meetings" => json!({"items":[meeting()],"page":page()}),
        "bazza_get_meeting" => meeting(),
        "bazza_get_library_structure" => json!({"folders":[],"tags":[],"page":page()}),
        "bazza_create_meeting"
        | "bazza_update_meeting"
        | "bazza_create_note"
        | "bazza_update_note" => receipt(),
        "bazza_delete_meeting" => {
            json!({"receipt":receipt(),"purge":"pending","retained_voice_sample_count":1})
        }
        "bazza_get_transcript" => {
            json!({"meeting_id":"meeting","state":"present","revision":"r1","current_source_id":"source","source_id":"source","source":{"source_id":"source","origin":"captured","source_label":null,"provider_label":"soniox","model_label":"synthetic-model","language":"ru","created_at":DATE},"representation":"effective","segments":[{"segment_id":"segment","start_ms":0,"end_ms":1000,"speaker":null,"raw":null,"effective":"Привет","clean_text_manual":false,"text_complete":true}],"continuation":null,"page":text_page("Привет".len())})
        }
        "bazza_import_transcript"
        | "bazza_replace_transcript"
        | "bazza_set_transcript_corrections" => {
            json!({"receipt":receipt(),"source_id":"source","index":"ready","affected_segments":1})
        }
        "bazza_delete_transcript"
        | "bazza_delete_summary"
        | "bazza_delete_note"
        | "bazza_delete_share" => {
            json!({"receipt":receipt(),"purge":"pending","retained_entities":["media"]})
        }
        "bazza_get_summary" => {
            json!({"meeting_id":"meeting","state":"present","revision":"r1","origin":"manual","auto_replace":false,"automatic_generation_suppressed":false,"source_missing":false,"provenance":provenance(),"format":"markdown","content":"Summary","page":text_page(7)})
        }
        "bazza_write_summary" => {
            json!({"receipt":receipt(),"origin":"external_agent","auto_replace":false})
        }
        "bazza_list_notes" => json!({"items":[note()],"page":page()}),
        "bazza_get_note" => json!({"note":note(),"content":"","page":text_page(0)}),
        "bazza_search" => {
            json!({"hits":[],"diagnostics":{"lexical":"ready","semantic_coverage":"none","mode":"lexical","no_matches":true,"search_generation":"r1"},"page":page()})
        }
        "bazza_list_calendar_events" => json!({"items":[],"sources":[],"page":page()}),
        "bazza_refresh_calendar" | "bazza_cancel_job" => json!({"receipt":receipt(),"job":job()}),
        "bazza_list_shares" => json!({"items":[share()],"page":page()}),
        "bazza_create_share" | "bazza_update_share" => json!({"receipt":receipt(),"share":share()}),
        "bazza_get_job" => job(),
        _ => panic!("missing output example: {name}"),
    };
    success(data)
}

#[test]
fn all_29_schemas_compile_and_accept_success_and_domain_error() {
    let catalog = tool_schemas();
    assert_eq!(
        catalog.iter().map(|tool| tool.name).collect::<Vec<_>>(),
        BASE_TOOL_NAMES
    );
    assert_eq!(
        serde_json::to_value(tool_schemas()).unwrap(),
        serde_json::to_value(tool_schemas()).unwrap()
    );
    for tool in catalog {
        for schema in [&tool.input_schema, &tool.output_schema] {
            assert_eq!(
                schema["$schema"],
                "https://json-schema.org/draft/2020-12/schema"
            );
            assert_eq!(schema["type"], "object");
            assert!(
                jsonschema::draft202012::meta::is_valid(schema),
                "invalid metaschema: {}",
                tool.name
            );
            jsonschema::draft202012::options()
                .should_validate_formats(true)
                .build(schema)
                .unwrap_or_else(|error| panic!("{}: {error}", tool.name));
        }
        validate_tool_input(tool.name, &valid_input(tool.name))
            .unwrap_or_else(|error| panic!("{} input: {error}", tool.name));
        validate_tool_output(tool.name, &valid_output(tool.name))
            .unwrap_or_else(|error| panic!("{} output: {error}", tool.name));
        validate_tool_output_for_input(
            tool.name,
            &valid_input(tool.name),
            &valid_output(tool.name),
        )
        .unwrap_or_else(|error| panic!("{} exchange: {error}", tool.name));
        validate_tool_output(tool.name, &error()).unwrap();
        let mut both = valid_output(tool.name);
        both["error"] = error()["error"].clone();
        assert!(
            validate_tool_output(tool.name, &both).is_err(),
            "success/error must be exclusive: {}",
            tool.name
        );
    }
}

#[test]
fn all_inputs_deny_unrecognized_root_properties_and_schema_outputs_deny_private_fields() {
    for name in BASE_TOOL_NAMES {
        let mut input = valid_input(name);
        input["confirmed"] = json!(true);
        assert!(
            validate_tool_input(name, &input).is_err(),
            "unknown root property: {name}"
        );
        let mut output = valid_output(name);
        output["data"]["private_settings"] = json!({"api_key":"synthetic-secret"});
        assert!(
            validate_tool_output(name, &output).is_err(),
            "private field leak: {name}"
        );
    }
    let nested = json!({"query":"Орион","filter":{"private_calendar_url":"https://example.invalid/private"}});
    assert!(validate_tool_input("bazza_search", &nested).is_err());
    assert_eq!(
        validate_tool_input("invoke", &json!({"method":"delete_all"})),
        Err(ContractValidationError::UnknownTool)
    );
    for reserved in [
        "bazza_list_shared_meetings",
        "bazza_get_shared_meeting",
        "bazza_refresh_shared_source",
    ] {
        assert_eq!(
            validate_tool_input(reserved, &json!({})),
            Err(ContractValidationError::UnknownTool)
        );
    }
}

#[test]
fn writes_require_idempotency_and_updates_deletes_require_revision_including_absent_states() {
    let mut writes = 0;
    let mut revisions = 0;
    for name in BASE_TOOL_NAMES {
        let input = valid_input(name);
        if input.get("idempotency_key").is_some() {
            writes += 1;
            let mut missing = input.clone();
            missing.as_object_mut().unwrap().remove("idempotency_key");
            assert!(
                validate_tool_input(name, &missing).is_err(),
                "key missing: {name}"
            );
            missing["idempotency_key"] = json!("");
            assert!(
                validate_tool_input(name, &missing).is_err(),
                "empty key: {name}"
            );
        }
        if input.get("expected_revision").is_some() {
            revisions += 1;
            let mut missing = input;
            missing.as_object_mut().unwrap().remove("expected_revision");
            assert!(
                validate_tool_input(name, &missing).is_err(),
                "revision missing: {name}"
            );
        }
    }
    assert_eq!(writes, 17);
    assert_eq!(revisions, 13);
    let mut replacement = valid_input("bazza_write_summary");
    replacement["mode"] = json!("replace");
    validate_tool_input("bazza_write_summary", &replacement).unwrap();
    replacement["origin"] = json!("configured_generation");
    assert!(validate_tool_input("bazza_write_summary", &replacement).is_err());
    let mut auto_replace = valid_output("bazza_write_summary");
    auto_replace["data"]["auto_replace"] = json!(true);
    assert!(validate_tool_output("bazza_write_summary", &auto_replace).is_err());
    auto_replace["data"]["auto_replace"] = json!(false);
    auto_replace["data"]["origin"] = json!("configured_generation");
    assert!(validate_tool_output("bazza_write_summary", &auto_replace).is_err());
}

#[test]
fn ids_are_not_paths_and_arrays_strings_and_page_limits_are_bounded() {
    for id in [
        "../bazza.db",
        "C:\\Users\\someone\\bazza.db",
        "/tmp/library",
        "",
        "a/b",
    ] {
        assert!(validate_tool_input("bazza_get_meeting", &json!({"meeting_id":id})).is_err());
    }
    for limit in [0, 101, 1000] {
        assert!(validate_tool_input("bazza_list_meetings", &json!({"limit":limit})).is_err());
    }
    for bytes in [0, 65_537] {
        assert!(validate_tool_input(
            "bazza_get_note",
            &json!({"note_id":"note","max_bytes":bytes})
        )
        .is_err());
    }
    let mut long_title = valid_input("bazza_create_meeting");
    long_title["title"] = json!("a".repeat(513));
    assert!(validate_tool_input("bazza_create_meeting", &long_title).is_err());
    let mut long_batch = valid_input("bazza_import_transcript");
    long_batch["segments"] = json!(vec![segment(); 1001]);
    assert!(validate_tool_input("bazza_import_transcript", &long_batch).is_err());
    let duplicate_filters = json!({"query":"Орион","filter":{"tag_ids":["tag","tag"]}});
    assert!(validate_tool_input("bazza_search", &duplicate_filters).is_err());
    let duplicate_fields = json!({"query":"Орион","fields":["title","title"]});
    assert!(validate_tool_input("bazza_search", &duplicate_fields).is_err());
    validate_tool_input("bazza_search", &json!({"query":"Орион","fields":["notes"]})).unwrap();
    assert!(
        validate_tool_input("bazza_search", &json!({"query":"Орион","fields":["note"]})).is_err()
    );
}

#[test]
fn timed_payloads_are_real_intervals_and_corrections_are_atomic_typed_operations() {
    for end in [0, 999] {
        let mut bad = valid_input("bazza_import_transcript");
        bad["segments"][0]["start_ms"] = json!(1000);
        bad["segments"][0]["end_ms"] = json!(end);
        assert!(validate_tool_input("bazza_import_transcript", &bad).is_err());
    }
    let mut plain = valid_input("bazza_create_meeting");
    plain["transcript"] = json!("plain text without actual timings");
    assert!(validate_tool_input("bazza_create_meeting", &plain).is_err());
    let mut corrections = valid_input("bazza_set_transcript_corrections");
    validate_tool_input("bazza_set_transcript_corrections", &corrections).unwrap();
    corrections["corrections"] = json!([{"operation":"reset","segment_id":"segment"}]);
    validate_tool_input("bazza_set_transcript_corrections", &corrections).unwrap();
    corrections["corrections"][0]["text"] = json!("raw overwrite");
    assert!(validate_tool_input("bazza_set_transcript_corrections", &corrections).is_err());
    corrections["corrections"] = json!([{"operation":"set","segment_id":"segment","text":""},{"operation":"reset","segment_id":"segment"}]);
    assert!(validate_tool_input("bazza_set_transcript_corrections", &corrections).is_err());
    corrections["corrections"] = json!(vec![
        json!({"operation":"reset","segment_id":"segment"});
        101
    ]);
    assert!(validate_tool_input("bazza_set_transcript_corrections", &corrections).is_err());
}

#[test]
fn patches_are_targeted_and_empty_or_conflicting_operations_are_rejected() {
    for name in [
        "bazza_update_meeting",
        "bazza_update_note",
        "bazza_update_share",
    ] {
        let mut input = valid_input(name);
        input["patch"] = json!({});
        assert!(validate_tool_input(name, &input).is_err());
        input["patch"] = json!({"title":null});
        assert!(validate_tool_input(name, &input).is_err());
    }
    let mut metadata = valid_input("bazza_update_meeting");
    metadata["patch"] = json!({"folder":{"operation":"clear"},"tag_operations":[{"operation":"add","tag_id":"tag"}]});
    validate_tool_input("bazza_update_meeting", &metadata).unwrap();
    metadata["patch"]["tag_operations"] =
        json!([{"operation":"add","tag_id":"tag"},{"operation":"remove","tag_id":"tag"}]);
    assert!(validate_tool_input("bazza_update_meeting", &metadata).is_err());
    let mut note = valid_input("bazza_update_note");
    note["patch"] = json!({"association":{"operation":"set","value":"meeting"}});
    validate_tool_input("bazza_update_note", &note).unwrap();
    note["patch"] = json!({"association":{"operation":"clear","value":"meeting"}});
    assert!(validate_tool_input("bazza_update_note", &note).is_err());
}

#[test]
fn utf8_byte_budgets_are_not_confused_with_schema_character_counts() {
    let mut transcript = valid_input("bazza_import_transcript");
    transcript["segments"][0]["text"] = json!("я".repeat(32_769));
    assert_eq!(
        validate_tool_input("bazza_import_transcript", &transcript),
        Err(ContractValidationError::InvalidInputSemantics)
    );
    let mut note = valid_input("bazza_create_note");
    note["markdown"] = json!("я".repeat(131_073));
    assert_eq!(
        validate_tool_input("bazza_create_note", &note),
        Err(ContractValidationError::InvalidInputSemantics)
    );
    transcript["segments"] = json!(vec![
        json!({"start_ms":0,"end_ms":1000,"text":"x".repeat(2000)});
        1000
    ]);
    assert_eq!(
        validate_tool_input("bazza_import_transcript", &transcript),
        Err(ContractValidationError::InputTooLarge)
    );
    let mut output = valid_output("bazza_get_transcript");
    output["data"]["page"]["returned_utf8_bytes"] = json!(6);
    assert!(validate_tool_output("bazza_get_transcript", &output).is_err());
}

#[test]
fn utf8_page_slicing_progresses_and_preserves_multibyte_text() {
    let original = "Орион 🌍 and budget";
    let mut rebuilt = String::new();
    let mut offset = 0;
    loop {
        let page = slice_utf8_page(original, offset, 5).unwrap();
        assert!(page.text.len() <= 5);
        rebuilt.push_str(page.text);
        let Some(next) = page.next_offset else {
            break;
        };
        assert!(next > offset && original.is_char_boundary(next));
        offset = next;
    }
    assert_eq!(rebuilt, original);
    assert!(slice_utf8_page("я", 0, 1).is_err());
    assert!(slice_utf8_page("я", 1, 10).is_err());
    assert!(slice_utf8_page("", 0, 0).is_err());
    assert_eq!(slice_utf8_page("", 0, 10).unwrap().text, "");
}

#[test]
fn calendar_ranges_require_real_offset_timestamps_and_no_private_source_urls() {
    for from in ["2026-11-02", "2026-11-02T00:00:00", "2026-99-02T00:00:00Z"] {
        let mut input = valid_input("bazza_list_calendar_events");
        input["from"] = json!(from);
        assert!(validate_tool_input("bazza_list_calendar_events", &input).is_err());
    }
    let mut reversed = valid_input("bazza_list_calendar_events");
    reversed["to"] = json!("2026-11-01T23:00:00Z");
    assert!(validate_tool_input("bazza_list_calendar_events", &reversed).is_err());
    let mut refresh = valid_input("bazza_refresh_calendar");
    refresh["url"] = json!("https://example.invalid/private.ics");
    assert!(validate_tool_input("bazza_refresh_calendar", &refresh).is_err());
}

#[test]
fn request_dependent_output_bounds_and_explicit_share_urls_are_checked() {
    let input = valid_input("bazza_list_shares");
    let mut output = valid_output("bazza_list_shares");
    output["data"]["items"][0]["url"] = json!("http://127.0.0.1:9000/s/synthetic");
    assert!(validate_tool_output_for_input("bazza_list_shares", &input, &output).is_err());
    let mut explicit = input;
    explicit["include_url"] = json!(true);
    validate_tool_output_for_input("bazza_list_shares", &explicit, &output).unwrap();
    output["data"]["items"][0]["token"] = json!("synthetic-bearer");
    assert!(validate_tool_output("bazza_list_shares", &output).is_err());
    let mut transcript_input = valid_input("bazza_get_transcript");
    transcript_input["max_bytes"] = json!(4);
    assert!(validate_tool_output_for_input(
        "bazza_get_transcript",
        &transcript_input,
        &valid_output("bazza_get_transcript")
    )
    .is_err());
    let mut meeting_input = valid_input("bazza_list_meetings");
    meeting_input["limit"] = json!(1);
    let mut meetings = valid_output("bazza_list_meetings");
    meetings["data"]["items"] = json!([meeting(), meeting()]);
    assert!(
        validate_tool_output_for_input("bazza_list_meetings", &meeting_input, &meetings).is_err()
    );
    let mut invalid_page = valid_output("bazza_list_meetings");
    invalid_page["data"]["page"]["truncated"] = json!(true);
    assert!(validate_tool_output("bazza_list_meetings", &invalid_page).is_err());
    invalid_page["data"]["page"]["next_cursor"] = json!("cursor-2");
    validate_tool_output("bazza_list_meetings", &invalid_page).unwrap();
}

#[test]
fn deleted_summary_generations_and_public_job_cancellation_cannot_be_misrepresented() {
    let mut summary = valid_output("bazza_get_summary");
    summary["data"]["state"] = json!("deleted");
    summary["data"]["content"] = Value::Null;
    summary["data"]["page"] = text_page(0);
    assert!(validate_tool_output("bazza_get_summary", &summary).is_err());
    summary["data"]["automatic_generation_suppressed"] = json!(true);
    validate_tool_output("bazza_get_summary", &summary).unwrap();
    summary["data"].as_object_mut().unwrap().remove("revision");
    assert!(validate_tool_output("bazza_get_summary", &summary).is_err());
    let mut job = valid_output("bazza_get_job");
    job["data"]["kind"] = json!("capture");
    assert!(validate_tool_output("bazza_get_job", &job).is_err());
    job["data"]["kind"] = json!("meeting_deletion");
    assert!(validate_tool_output("bazza_get_job", &job).is_err());
    job["data"]["cancellable"] = json!(false);
    validate_tool_output("bazza_get_job", &job).unwrap();
}

#[test]
fn permission_catalog_is_complete_and_retention_is_a_fixed_contract_target() {
    assert_eq!(DEDUP_RETENTION_SECONDS, 604_800);
    for name in BASE_TOOL_NAMES {
        assert!(tool_permission(name).is_some(), "{name}");
    }
    assert!(tool_permission("admin_revoke").is_none());
    assert_eq!(
        tool_permission("bazza_write_summary").unwrap().required,
        LibraryPermission::Update
    );
    assert_eq!(
        tool_permission("bazza_list_shares")
            .unwrap()
            .include_url_requires,
        Some(LibraryPermission::Share)
    );
    assert!(
        tool_permission("bazza_cancel_job")
            .unwrap()
            .recheck_job_authority
    );
}

#[test]
fn validation_errors_do_not_repeat_supplied_payloads() {
    let secret = "SYNTHETIC-SENSITIVE-TEXT";
    let invalid = json!({"meeting_id":format!("../{secret}")});
    let error = validate_tool_input("bazza_get_meeting", &invalid).unwrap_err();
    assert!(!error.to_string().contains(secret));
    let mut summary = valid_input("bazza_write_summary");
    summary["structured_json"] = json!("{broken json}");
    assert!(validate_tool_input("bazza_write_summary", &summary).is_err());
}

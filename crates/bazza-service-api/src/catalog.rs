use crate::{inputs::*, outputs::*};
use schemars::{generate::SchemaSettings, JsonSchema};
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;

pub const BASE_TOOL_NAMES: [&str; 29] = [
    "bazza_get_status",
    "bazza_list_meetings",
    "bazza_get_meeting",
    "bazza_get_library_structure",
    "bazza_create_meeting",
    "bazza_update_meeting",
    "bazza_delete_meeting",
    "bazza_get_transcript",
    "bazza_import_transcript",
    "bazza_replace_transcript",
    "bazza_set_transcript_corrections",
    "bazza_delete_transcript",
    "bazza_get_summary",
    "bazza_write_summary",
    "bazza_delete_summary",
    "bazza_list_notes",
    "bazza_get_note",
    "bazza_create_note",
    "bazza_update_note",
    "bazza_delete_note",
    "bazza_search",
    "bazza_list_calendar_events",
    "bazza_refresh_calendar",
    "bazza_list_shares",
    "bazza_create_share",
    "bazza_update_share",
    "bazza_delete_share",
    "bazza_get_job",
    "bazza_cancel_job",
];

#[derive(Clone, Debug, Serialize)]
pub struct ToolSchema {
    pub name: &'static str,
    pub input_schema: Value,
    pub output_schema: Value,
}

fn schema<T: JsonSchema>(output: bool) -> Value {
    let settings = if output {
        SchemaSettings::draft2020_12().for_serialize()
    } else {
        SchemaSettings::draft2020_12().for_deserialize()
    };
    let root = settings.into_generator().into_root_schema_for::<T>();
    let mut schema = serde_json::to_value(root).expect("schema is a JSON value");
    // MCP tool schemas are object-shaped even when the result is an untagged union.
    schema
        .as_object_mut()
        .expect("root object")
        .insert("type".into(), Value::String("object".into()));
    mark_unique_arrays(&mut schema);
    schema
}

fn mark_unique_arrays(schema: &mut Value) {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, value) in properties {
            if [
                "meeting_ids",
                "folder_ids",
                "tag_ids",
                "note_ids",
                "source_ids",
                "fields",
                "available_tools",
                "retained_entities",
            ]
            .contains(&name.as_str())
            {
                make_array_unique(value);
            }
        }
    }
    match schema {
        Value::Object(map) => {
            for value in map.values_mut() {
                mark_unique_arrays(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                mark_unique_arrays(value);
            }
        }
        _ => {}
    }
}

fn make_array_unique(schema: &mut Value) {
    if schema.get("type").and_then(Value::as_str) == Some("array")
        || schema
            .get("type")
            .and_then(Value::as_array)
            .is_some_and(|types| types.iter().any(|kind| kind == "array"))
    {
        schema
            .as_object_mut()
            .unwrap()
            .insert("uniqueItems".into(), Value::Bool(true));
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get_mut(key).and_then(Value::as_array_mut) {
            for branch in branches {
                make_array_unique(branch);
            }
        }
    }
}

fn tool<I: JsonSchema, O: JsonSchema>(name: &'static str) -> ToolSchema {
    ToolSchema {
        name,
        input_schema: schema::<I>(false),
        output_schema: schema::<ToolResult<O>>(true),
    }
}

/// Deterministic future API catalog. Calling this does not advertise runtime support.
pub fn tool_schemas() -> Vec<ToolSchema> {
    static CATALOG: OnceLock<Vec<ToolSchema>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            vec![
                tool::<GetStatusInput, ServiceStatus>(BASE_TOOL_NAMES[0]),
                tool::<ListMeetingsInput, MeetingsPage>(BASE_TOOL_NAMES[1]),
                tool::<GetMeetingInput, MeetingView>(BASE_TOOL_NAMES[2]),
                tool::<GetLibraryStructureInput, LibraryStructurePage>(BASE_TOOL_NAMES[3]),
                tool::<CreateMeetingInput, MutationReceipt>(BASE_TOOL_NAMES[4]),
                tool::<UpdateMeetingInput, MutationReceipt>(BASE_TOOL_NAMES[5]),
                tool::<DeleteMeetingInput, DeleteMeetingOutcome>(BASE_TOOL_NAMES[6]),
                tool::<GetTranscriptInput, TranscriptPage>(BASE_TOOL_NAMES[7]),
                tool::<ImportTranscriptInput, TranscriptMutationOutcome>(BASE_TOOL_NAMES[8]),
                tool::<ReplaceTranscriptInput, TranscriptMutationOutcome>(BASE_TOOL_NAMES[9]),
                tool::<SetTranscriptCorrectionsInput, TranscriptMutationOutcome>(
                    BASE_TOOL_NAMES[10],
                ),
                tool::<DeleteTranscriptInput, DeleteContentOutcome>(BASE_TOOL_NAMES[11]),
                tool::<GetSummaryInput, SummaryPage>(BASE_TOOL_NAMES[12]),
                tool::<WriteSummaryInput, SummaryMutationOutcome>(BASE_TOOL_NAMES[13]),
                tool::<DeleteSummaryInput, DeleteContentOutcome>(BASE_TOOL_NAMES[14]),
                tool::<ListNotesInput, NotesPage>(BASE_TOOL_NAMES[15]),
                tool::<GetNoteInput, NotePage>(BASE_TOOL_NAMES[16]),
                tool::<CreateNoteInput, MutationReceipt>(BASE_TOOL_NAMES[17]),
                tool::<UpdateNoteInput, MutationReceipt>(BASE_TOOL_NAMES[18]),
                tool::<DeleteNoteInput, DeleteContentOutcome>(BASE_TOOL_NAMES[19]),
                tool::<SearchInput, SearchPage>(BASE_TOOL_NAMES[20]),
                tool::<ListCalendarEventsInput, CalendarEventsPage>(BASE_TOOL_NAMES[21]),
                tool::<RefreshCalendarInput, JobMutationOutcome>(BASE_TOOL_NAMES[22]),
                tool::<ListSharesInput, SharesPage>(BASE_TOOL_NAMES[23]),
                tool::<CreateShareInput, ShareMutationOutcome>(BASE_TOOL_NAMES[24]),
                tool::<UpdateShareInput, ShareMutationOutcome>(BASE_TOOL_NAMES[25]),
                tool::<DeleteShareInput, DeleteContentOutcome>(BASE_TOOL_NAMES[26]),
                tool::<GetJobInput, JobView>(BASE_TOOL_NAMES[27]),
                tool::<CancelJobInput, JobMutationOutcome>(BASE_TOOL_NAMES[28]),
            ]
        })
        .clone()
}

use crate::{
    tool_schemas, MAX_DOCUMENT_UTF8_BYTES, MAX_PAGE_UTF8_BYTES, MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES,
};
use chrono::DateTime;
use serde_json::Value;
use std::{collections::BTreeSet, io, sync::OnceLock};

/// Validation errors deliberately omit supplied text, tokens and schema internals.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractValidationError {
    #[error("unknown tool contract")]
    UnknownTool,
    #[error("tool input exceeds the byte budget")]
    InputTooLarge,
    #[error("tool output exceeds the byte budget")]
    OutputTooLarge,
    #[error("tool contract could not be compiled")]
    InvalidSchema,
    #[error("tool input does not satisfy its schema")]
    InvalidInput,
    #[error("tool output does not satisfy its schema")]
    InvalidOutput,
    #[error("tool input violates a semantic constraint")]
    InvalidInputSemantics,
    #[error("tool output violates a semantic constraint")]
    InvalidOutputSemantics,
    #[error("invalid UTF-8 page boundary or byte budget")]
    InvalidPageBoundary,
}

struct Compiled {
    name: &'static str,
    input: jsonschema::Validator,
    output: jsonschema::Validator,
}

fn validators() -> Result<&'static Vec<Compiled>, ContractValidationError> {
    static VALIDATORS: OnceLock<Result<Vec<Compiled>, ()>> = OnceLock::new();
    VALIDATORS
        .get_or_init(|| {
            tool_schemas()
                .into_iter()
                .map(|tool| {
                    let input = jsonschema::draft202012::options()
                        .should_validate_formats(true)
                        .build(&tool.input_schema)
                        .map_err(|_| ())?;
                    let output = jsonschema::draft202012::options()
                        .should_validate_formats(true)
                        .build(&tool.output_schema)
                        .map_err(|_| ())?;
                    Ok(Compiled {
                        name: tool.name,
                        input,
                        output,
                    })
                })
                .collect()
        })
        .as_ref()
        .map_err(|_| ContractValidationError::InvalidSchema)
}

fn compiled(name: &str) -> Result<&'static Compiled, ContractValidationError> {
    validators()?
        .iter()
        .find(|validator| validator.name == name)
        .ok_or(ContractValidationError::UnknownTool)
}

pub fn validate_tool_input(name: &str, input: &Value) -> Result<(), ContractValidationError> {
    if !within_json_budget(input, MAX_REQUEST_BYTES) {
        return Err(ContractValidationError::InputTooLarge);
    }
    if !compiled(name)?.input.is_valid(input) {
        return Err(ContractValidationError::InvalidInput);
    }
    if !payload_budgets(input) || !intervals_valid(input) || !input_semantics(name, input) {
        return Err(ContractValidationError::InvalidInputSemantics);
    }
    Ok(())
}

pub fn validate_tool_output(name: &str, output: &Value) -> Result<(), ContractValidationError> {
    if !within_json_budget(output, MAX_RESPONSE_BYTES) {
        return Err(ContractValidationError::OutputTooLarge);
    }
    if !compiled(name)?.output.is_valid(output) {
        return Err(ContractValidationError::InvalidOutput);
    }
    if !payload_budgets(output)
        || !intervals_valid(output)
        || !pages_valid(output)
        || !output_semantics(name, output)
    {
        return Err(ContractValidationError::InvalidOutputSemantics);
    }
    Ok(())
}

/// Also enforce request-dependent privacy, representation and pagination bounds.
pub fn validate_tool_output_for_input(
    name: &str,
    input: &Value,
    output: &Value,
) -> Result<(), ContractValidationError> {
    validate_tool_input(name, input)?;
    validate_tool_output(name, output)?;
    let Some(data) = output.get("data") else {
        return Ok(());
    };
    if matches!(
        name,
        "bazza_list_shares" | "bazza_create_share" | "bazza_update_share"
    ) && input.get("include_url").and_then(Value::as_bool) != Some(true)
        && has_url(data)
    {
        return Err(ContractValidationError::InvalidOutputSemantics);
    }
    if let Some(bytes) = data
        .pointer("/page/returned_utf8_bytes")
        .and_then(Value::as_u64)
    {
        if bytes
            > input
                .get("max_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(MAX_PAGE_UTF8_BYTES as u64)
        {
            return Err(ContractValidationError::InvalidOutputSemantics);
        }
    }
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    for key in ["items", "hits", "segments"] {
        if data
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() > limit)
        {
            return Err(ContractValidationError::InvalidOutputSemantics);
        }
    }
    if name == "bazza_get_library_structure" {
        let count = ["folders", "tags"]
            .iter()
            .filter_map(|key| data.get(*key).and_then(Value::as_array))
            .map(Vec::len)
            .sum::<usize>();
        if count > limit {
            return Err(ContractValidationError::InvalidOutputSemantics);
        }
    }
    if name == "bazza_get_transcript"
        && data.get("representation").and_then(Value::as_str)
            != Some(
                input
                    .get("representation")
                    .and_then(Value::as_str)
                    .unwrap_or("effective"),
            )
    {
        return Err(ContractValidationError::InvalidOutputSemantics);
    }
    if name == "bazza_get_summary"
        && data.get("format").and_then(Value::as_str)
            != Some(
                input
                    .get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("markdown"),
            )
    {
        return Err(ContractValidationError::InvalidOutputSemantics);
    }
    Ok(())
}

fn within_json_budget(value: &Value, limit: usize) -> bool {
    struct Counter {
        remaining: usize,
    }
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "JSON byte budget exceeded",
                ));
            }
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Counter { remaining: limit }, value).is_ok()
}

fn pages_valid(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if let (Some(truncated), Some(cursor)) = (
                map.get("truncated").and_then(Value::as_bool),
                map.get("next_cursor"),
            ) {
                if truncated == cursor.is_null() {
                    return false;
                }
            }
            map.values().all(pages_valid)
        }
        Value::Array(array) => array.iter().all(pages_valid),
        _ => true,
    }
}

fn has_url(value: &Value) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| (key == "url" && !value.is_null()) || has_url(value)),
        Value::Array(array) => array.iter().any(has_url),
        _ => false,
    }
}

fn payload_budgets(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().all(|(key, value)| {
            let bounded = match (key.as_str(), value.as_str()) {
                ("text" | "raw" | "effective" | "content", Some(text)) => {
                    text.len() <= MAX_PAGE_UTF8_BYTES
                }
                ("markdown" | "structured_json", Some(text)) => {
                    text.len() <= MAX_DOCUMENT_UTF8_BYTES
                }
                _ => true,
            };
            bounded && payload_budgets(value)
        }),
        Value::Array(array) => array.iter().all(payload_budgets),
        _ => true,
    }
}

fn intervals_valid(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if let (Some(start), Some(end)) = (
                map.get("start_ms").and_then(Value::as_u64),
                map.get("end_ms").and_then(Value::as_u64),
            ) {
                if end <= start {
                    return false;
                }
            }
            for (from, to) in [("from", "to"), ("start", "end"), ("started_at", "ended_at")] {
                if let (Some(start), Some(end)) = (
                    map.get(from).and_then(Value::as_str),
                    map.get(to).and_then(Value::as_str),
                ) {
                    let (Ok(start), Ok(end)) = (
                        DateTime::parse_from_rfc3339(start),
                        DateTime::parse_from_rfc3339(end),
                    ) else {
                        return false;
                    };
                    if end < start || (from != "started_at" && end == start) {
                        return false;
                    }
                }
            }
            map.values().all(intervals_valid)
        }
        Value::Array(array) => array.iter().all(intervals_valid),
        _ => true,
    }
}

fn unique_field(array: &Value, field: &str) -> bool {
    let Some(array) = array.as_array() else {
        return false;
    };
    let mut seen = BTreeSet::new();
    array.iter().all(|item| {
        item.get(field)
            .and_then(Value::as_str)
            .is_some_and(|id| seen.insert(id))
    })
}

fn nonempty_patch(value: &Value) -> bool {
    value.as_object().is_some_and(|patch| {
        patch
            .values()
            .any(|value| !value.is_null() && value.as_array().is_none_or(|array| !array.is_empty()))
    })
}

fn nonempty_share_fields(value: &Value) -> bool {
    value
        .get("transcript")
        .is_some_and(|field| !field.is_null())
        || ["summary", "audio", "video"]
            .iter()
            .any(|key| value.get(*key).and_then(Value::as_bool) == Some(true))
        || value
            .get("note_ids")
            .and_then(Value::as_array)
            .is_some_and(|array| !array.is_empty())
}

fn input_semantics(name: &str, input: &Value) -> bool {
    match name {
        "bazza_update_meeting" => {
            nonempty_patch(&input["patch"])
                && input["patch"]
                    .get("tag_operations")
                    .is_none_or(|operations| unique_field(operations, "tag_id"))
        }
        "bazza_update_note" => nonempty_patch(&input["patch"]),
        "bazza_update_share" => {
            nonempty_patch(&input["patch"])
                && input["patch"]
                    .get("fields")
                    .is_none_or(|fields| fields.is_null() || nonempty_share_fields(fields))
        }
        "bazza_create_share" => nonempty_share_fields(&input["fields"]),
        "bazza_set_transcript_corrections" => unique_field(&input["corrections"], "segment_id"),
        "bazza_write_summary" => input
            .get("structured_json")
            .and_then(Value::as_str)
            .is_none_or(|text| serde_json::from_str::<Value>(text).is_ok()),
        "bazza_search" => input["query"]
            .as_str()
            .is_some_and(|query| !query.trim().is_empty()),
        _ => true,
    }
}

fn output_semantics(name: &str, output: &Value) -> bool {
    let Some(data) = output.get("data") else {
        return true;
    };
    match name {
        "bazza_get_transcript" => {
            let Some(segments) = data["segments"].as_array() else {
                return false;
            };
            let bytes: usize = segments
                .iter()
                .map(|segment| {
                    ["raw", "effective"]
                        .iter()
                        .filter_map(|key| segment[*key].as_str())
                        .map(str::len)
                        .sum::<usize>()
                })
                .sum();
            if bytes > MAX_PAGE_UTF8_BYTES
                || data
                    .pointer("/page/returned_utf8_bytes")
                    .and_then(Value::as_u64)
                    != Some(bytes as u64)
            {
                return false;
            }
            if data["state"] != "present"
                && (!segments.is_empty()
                    || !data["source_id"].is_null()
                    || !data["current_source_id"].is_null()
                    || !data["source"].is_null()
                    || !data["continuation"].is_null())
            {
                return false;
            }
            if data["state"] == "present"
                && (data["source_id"].is_null()
                    || data["source"].is_null()
                    || data.pointer("/source/source_id") != data.get("source_id"))
            {
                return false;
            }
            let partial: Vec<_> = segments
                .iter()
                .filter(|segment| segment["text_complete"] == false)
                .collect();
            if partial.len() > 1 {
                return false;
            }
            if !data["continuation"].is_null()
                && (partial.len() != 1
                    || data.pointer("/continuation/segment_id") != partial[0].get("segment_id")
                    || data
                        .pointer("/continuation/offset_utf8_bytes")
                        .and_then(Value::as_u64)
                        == Some(0))
            {
                return false;
            }
            for segment in segments {
                let has_raw = segment["raw"].is_string();
                let has_effective = segment["effective"].is_string();
                let valid = match data["representation"].as_str() {
                    Some("raw") => has_raw && !has_effective,
                    Some("effective") => !has_raw && has_effective,
                    Some("both") => has_raw && has_effective,
                    _ => false,
                };
                if !valid {
                    return false;
                }
                if segment["text_complete"] == false
                    && (data["continuation"].is_null()
                        || data.pointer("/page/truncated") != Some(&Value::Bool(true)))
                {
                    return false;
                }
            }
            true
        }
        "bazza_get_summary" | "bazza_get_note" => {
            let bytes = data["content"].as_str().map_or(0, str::len);
            if data
                .pointer("/page/returned_utf8_bytes")
                .and_then(Value::as_u64)
                != Some(bytes as u64)
            {
                return false;
            }
            if name == "bazza_get_summary" {
                if data["state"] != "present" && !data["content"].is_null() {
                    return false;
                }
                if data["state"] == "deleted" && data["automatic_generation_suppressed"] != true {
                    return false;
                }
                if matches!(data["origin"].as_str(), Some("manual" | "external_agent"))
                    && data["auto_replace"] != false
                {
                    return false;
                }
            }
            true
        }
        "bazza_search" => {
            if data.pointer("/diagnostics/lexical").and_then(Value::as_str) != Some("ready") {
                return false;
            }
            let Some(hits) = data["hits"].as_array() else {
                return false;
            };
            if data
                .pointer("/diagnostics/no_matches")
                .and_then(Value::as_bool)
                != Some(hits.is_empty())
            {
                return false;
            }
            hits.iter().all(|hit| {
                if hit["kind"] == "note" {
                    if hit["note_id"].is_null() {
                        return false;
                    }
                } else if hit["meeting_id"].is_null() {
                    return false;
                }
                if hit["start_ms"].is_null() != hit["end_ms"].is_null() {
                    return false;
                }
                hit["kind"] == "transcript"
                    || (hit["start_ms"].is_null() && hit["end_ms"].is_null())
            })
        }
        "bazza_get_job" => job_valid(data),
        "bazza_cancel_job" | "bazza_refresh_calendar" => job_valid(&data["job"]),
        "bazza_write_summary" => {
            !matches!(data["origin"].as_str(), Some("manual" | "external_agent"))
                || data["auto_replace"] == false
        }
        _ => true,
    }
}

fn job_valid(job: &Value) -> bool {
    !matches!(
        job["kind"].as_str(),
        Some("meeting_deletion" | "content_deletion")
    ) || job["cancellable"] == false
}

#[derive(Debug, PartialEq, Eq)]
pub struct Utf8Page<'a> {
    pub text: &'a str,
    pub next_offset: Option<usize>,
}

/// Slice content at UTF-8 byte boundaries; a too-small budget cannot stall a cursor.
pub fn slice_utf8_page(
    text: &str,
    offset: usize,
    max_bytes: usize,
) -> Result<Utf8Page<'_>, ContractValidationError> {
    if max_bytes == 0
        || max_bytes > MAX_PAGE_UTF8_BYTES
        || offset > text.len()
        || !text.is_char_boundary(offset)
    {
        return Err(ContractValidationError::InvalidPageBoundary);
    }
    let mut end = offset.saturating_add(max_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == offset && offset < text.len() {
        return Err(ContractValidationError::InvalidPageBoundary);
    }
    Ok(Utf8Page {
        text: &text[offset..end],
        next_offset: (end < text.len()).then_some(end),
    })
}

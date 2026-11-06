use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryPermission {
    Read,
    Create,
    Update,
    Delete,
    Share,
}

/// Contract policy, not a caller-supplied authority or an implemented grant checker.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ToolPermission {
    pub required: LibraryPermission,
    pub include_url_requires: Option<LibraryPermission>,
    /// Cancellation additionally checks the original job's grant and public policy.
    pub recheck_job_authority: bool,
}

pub fn tool_permission(name: &str) -> Option<ToolPermission> {
    use LibraryPermission::*;
    let required = match name {
        "bazza_get_status"
        | "bazza_list_meetings"
        | "bazza_get_meeting"
        | "bazza_get_library_structure"
        | "bazza_get_transcript"
        | "bazza_get_summary"
        | "bazza_list_notes"
        | "bazza_get_note"
        | "bazza_search"
        | "bazza_list_calendar_events"
        | "bazza_list_shares"
        | "bazza_get_job" => Read,
        "bazza_create_meeting" | "bazza_create_note" => Create,
        "bazza_update_meeting"
        | "bazza_import_transcript"
        | "bazza_replace_transcript"
        | "bazza_set_transcript_corrections"
        | "bazza_write_summary"
        | "bazza_update_note"
        | "bazza_refresh_calendar"
        | "bazza_cancel_job" => Update,
        "bazza_delete_meeting"
        | "bazza_delete_transcript"
        | "bazza_delete_summary"
        | "bazza_delete_note" => Delete,
        "bazza_create_share" | "bazza_update_share" | "bazza_delete_share" => Share,
        _ => return None,
    };
    Some(ToolPermission {
        required,
        include_url_requires: (name == "bazza_list_shares").then_some(Share),
        recheck_job_authority: name == "bazza_cancel_job",
    })
}

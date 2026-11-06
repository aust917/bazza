use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub default_template_id: Option<String>,
    pub meeting_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub meeting_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Share {
    pub token: String,
    pub meeting_id: String,
    pub meeting_title: String,
    /// "meeting" | "clip"
    pub kind: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

/// Filter for `list_meetings_filtered`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MeetingFilter {
    /// `Some(None)` = unfiled meetings only; `Some(Some(id))` = folder subtree; `None` = everything.
    #[serde(default, deserialize_with = "deserialize_folder_filter")]
    pub folder: Option<Option<String>>,
    pub tag_id: Option<String>,
}

fn deserialize_folder_filter<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Folder {
        LegacyId(String),
        Picker([Option<String>; 1]),
    }
    Ok(match Option::<Folder>::deserialize(deserializer)? {
        None => None,
        Some(Folder::LegacyId(id)) => Some(Some(id)),
        Some(Folder::Picker([id])) => Some(id),
    })
}

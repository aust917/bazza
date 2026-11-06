use super::*;
use crate::{Meeting, Store};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewTextMeeting {
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub participants: Vec<String>,
    pub folder_id: Option<String>,
    pub tag_ids: Vec<String>,
    pub calendar_uid: Option<String>,
    pub transcript: Option<Vec<crate::NewSegment>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AssociationPatch {
    Set(String),
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TagMutation {
    Add(String),
    Remove(String),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MeetingUpdate {
    pub title: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub participants: Option<Vec<String>>,
    pub folder: Option<AssociationPatch>,
    pub calendar: Option<AssociationPatch>,
    pub tags: Vec<TagMutation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceMeeting {
    pub meeting: Meeting,
    pub content_kind: String,
    pub metadata_revision: String,
    pub lifecycle_revision: String,
    /// none for text meetings; missing media never changes recorded content kind.
    pub media_state: String,
}

pub(super) fn initialize_meeting_on(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    kind: &str,
) -> DataResult<(String, String)> {
    let metadata = revision();
    let lifecycle = revision();
    tx.execute(
        "INSERT INTO service_meeting_state VALUES(?1,?2,?3,?4,0)",
        params![id, kind, metadata, lifecycle],
    )?;
    tx.execute(
        "INSERT INTO service_transcript_state VALUES(?1,?2,NULL,'absent',0)",
        params![id, revision()],
    )?;
    tx.execute("INSERT INTO service_summary_state(meeting_id,revision,document_state,origin,auto_replace) VALUES(?1,?2,'absent','manual',0)",params![id,revision()])?;
    Ok((metadata, lifecycle))
}

fn participants(values: &[String]) -> DataResult<()> {
    if values.len() > 64 {
        return Err(DataError::Invalid("participants"));
    }
    for p in values {
        bounded_text(p, 1, 256)?;
    }
    Ok(())
}

fn relation(conn: &Connection, table: &str, id: &str) -> DataResult<()> {
    safe_id(id)?;
    let sql = match table {
        "folders" => "SELECT 1 FROM folders WHERE id=?1",
        "tags" => "SELECT 1 FROM tags WHERE id=?1",
        _ => unreachable!(),
    };
    if conn
        .query_row(sql, [id], |r| r.get::<_, i64>(0))
        .optional()?
        .is_none()
    {
        return Err(DataError::NotFound);
    }
    Ok(())
}

impl Store {
    pub fn service_create_text_meeting(
        &self,
        ctx: &MutationContext,
        input: &NewTextMeeting,
    ) -> DataResult<MutationOutcome> {
        bounded_text(&input.title, 1, 512)?;
        participants(&input.participants)?;
        if input.tag_ids.len() > 100 {
            return Err(DataError::Invalid("tags"));
        }
        if let Some(uid) = &input.calendar_uid {
            bounded_text(uid, 1, 512)?;
        }
        consistency::mutate(self, ctx, "create_meeting", input, |tx| {
            // mutate has already bounded the entire serialized request before
            // this optional copy or any database writes.
            let transcript = input.transcript.as_ref().map(|segments| TranscriptWrite {
                expected_revision: String::new(),
                segments: segments.clone(),
                source_label: None,
                language: None,
                origin: TranscriptImportOrigin::ExternalAgent,
            });
            if let Some(transcript) = &transcript {
                documents::validate_segments(transcript)?;
            }
            if let Some(id) = &input.folder_id {
                relation(tx, "folders", id)?;
            }
            let mut seen = std::collections::HashSet::new();
            for id in &input.tag_ids {
                relation(tx, "tags", id)?;
                if !seen.insert(id) {
                    return Err(DataError::Invalid("duplicate tag"));
                }
            }
            let id = revision();
            tx.execute("INSERT INTO meetings(id,title,started_at,status,created_at,title_auto,participants_json,folder_id,calendar_uid) VALUES(?1,?2,?3,'ready',?4,0,?5,?6,?7)",
                params![id,input.title,input.started_at,Utc::now(),serde_json::to_string(&input.participants)?,input.folder_id,input.calendar_uid])?;
            let (rev, _) = initialize_meeting_on(tx, &id, "text")?;
            for tag in &input.tag_ids {
                tx.execute("INSERT INTO meeting_tags VALUES(?1,?2)", params![id, tag])?;
            }
            if let Some(transcript) = &transcript {
                let state = documents::transcript_state_on(tx, &id)?;
                let transcript_rev = documents::replace_transcript_on(tx, &id, &state, transcript)?;
                consistency::change(tx, "transcript", &id, &transcript_rev, &ctx.principal)?;
            } else {
                search::rebuild_meeting_on(tx, &id)?;
            }
            consistency::outcome(tx, "meeting", &id, &rev, ctx)
        })
    }

    pub fn service_update_meeting(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        patch: &MeetingUpdate,
    ) -> DataResult<MutationOutcome> {
        safe_id(id)?;
        if let Some(t) = &patch.title {
            bounded_text(t, 1, 512)?;
        }
        if let Some(p) = &patch.participants {
            participants(p)?;
        }
        if patch.tags.len() > 100 {
            return Err(DataError::Invalid("tags"));
        }
        if patch.title.is_none()
            && patch.started_at.is_none()
            && patch.participants.is_none()
            && patch.folder.is_none()
            && patch.calendar.is_none()
            && patch.tags.is_empty()
        {
            return Err(DataError::Invalid("empty patch"));
        }
        consistency::mutate(self, ctx, "update_meeting", &(id, expected, patch), |tx| {
            visible_meeting(tx, id)?;
            let actual: String = tx.query_row(
                "SELECT metadata_revision FROM service_meeting_state WHERE meeting_id=?1",
                [id],
                |r| r.get(0),
            )?;
            check_revision(&actual, expected)?;
            if let Some(t) = &patch.title {
                tx.execute(
                    "UPDATE meetings SET title=?2,title_auto=0 WHERE id=?1",
                    params![id, t],
                )?;
            }
            if let Some(t) = &patch.started_at {
                tx.execute(
                    "UPDATE meetings SET started_at=?2 WHERE id=?1",
                    params![id, t],
                )?;
            }
            if let Some(p) = &patch.participants {
                tx.execute(
                    "UPDATE meetings SET participants_json=?2 WHERE id=?1",
                    params![id, serde_json::to_string(p)?],
                )?;
            }
            if let Some(p) = &patch.folder {
                let value = match p {
                    AssociationPatch::Set(v) => {
                        relation(tx, "folders", v)?;
                        Some(v)
                    }
                    AssociationPatch::Clear => None,
                };
                tx.execute(
                    "UPDATE meetings SET folder_id=?2 WHERE id=?1",
                    params![id, value],
                )?;
            }
            if let Some(p) = &patch.calendar {
                let value = match p {
                    AssociationPatch::Set(v) => {
                        bounded_text(v, 1, 512)?;
                        Some(v)
                    }
                    AssociationPatch::Clear => None,
                };
                tx.execute(
                    "UPDATE meetings SET calendar_uid=?2 WHERE id=?1",
                    params![id, value],
                )?;
            }
            let mut seen = std::collections::HashSet::new();
            for tag in &patch.tags {
                let tag_id = match tag {
                    TagMutation::Add(t) | TagMutation::Remove(t) => t,
                };
                if !seen.insert(tag_id) {
                    return Err(DataError::Invalid("duplicate tag operation"));
                }
                relation(tx, "tags", tag_id)?;
                match tag {
                    TagMutation::Add(t) => {
                        tx.execute(
                            "INSERT OR IGNORE INTO meeting_tags VALUES(?1,?2)",
                            params![id, t],
                        )?;
                    }
                    TagMutation::Remove(t) => {
                        tx.execute(
                            "DELETE FROM meeting_tags WHERE meeting_id=?1 AND tag_id=?2",
                            params![id, t],
                        )?;
                    }
                }
            }
            let rev = revision();
            tx.execute(
                "UPDATE service_meeting_state SET metadata_revision=?2 WHERE meeting_id=?1",
                params![id, rev],
            )?;
            search::rebuild_meeting_on(tx, id)?;
            consistency::outcome(tx, "meeting", id, &rev, ctx)
        })
    }

    pub fn service_meeting(&self, id: &str) -> DataResult<Option<ServiceMeeting>> {
        safe_id(id)?;
        // Keep state and metadata from one lock/snapshot; the legacy getter locks independently.
        let conn = self.conn.lock();
        let state:Option<(String,String,String)>=conn.query_row("SELECT content_kind,metadata_revision,lifecycle_revision FROM service_meeting_state WHERE meeting_id=?1 AND tombstoned=0",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((kind, metadata, lifecycle)) = state else {
            return Ok(None);
        };
        let meeting = conn.query_row(
            "SELECT * FROM meetings WHERE id=?1",
            [id],
            crate::meetings::meeting_from_row,
        )?;
        let media_state = if kind == "text" {
            "none"
        } else {
            let path = self.root().join("recordings").join(id).join("mix.wav");
            if path
                .symlink_metadata()
                .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            {
                "available"
            } else {
                "missing"
            }
        };
        Ok(Some(ServiceMeeting {
            meeting,
            content_kind: kind,
            metadata_revision: metadata,
            lifecycle_revision: lifecycle,
            media_state: media_state.into(),
        }))
    }
}

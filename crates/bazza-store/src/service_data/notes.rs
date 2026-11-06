use super::*;
use crate::Store;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserNote {
    pub id: String,
    pub meeting_id: Option<String>,
    pub title: String,
    pub markdown: String,
    pub revision: String,
    pub origin: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewNote {
    pub title: String,
    pub markdown: String,
    pub meeting_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteListEntry {
    pub id: String,
    pub meeting_id: Option<String>,
    pub title: String,
    pub revision: String,
    pub origin: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NoteUpdate {
    pub title: Option<String>,
    pub markdown: Option<String>,
    pub association: Option<AssociationPatch>,
}

fn note_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserNote> {
    Ok(UserNote {
        id: r.get("id")?,
        meeting_id: r.get("meeting_id")?,
        title: r.get("title")?,
        markdown: r.get("markdown")?,
        revision: r.get("revision")?,
        origin: r.get("origin")?,
        actor: r.get("actor")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

impl Store {
    pub fn service_create_note(
        &self,
        ctx: &MutationContext,
        input: &NewNote,
    ) -> DataResult<MutationOutcome> {
        bounded_text(&input.title, 1, 512)?;
        bounded_text(&input.markdown, 0, 262144)?;
        consistency::mutate(self, ctx, "create_note", input, |tx| {
            if let Some(id) = &input.meeting_id {
                safe_id(id)?;
                live_meeting(tx, id)?;
            }
            let id = revision();
            let rev = revision();
            let now = Utc::now();
            tx.execute(
                "INSERT INTO service_notes VALUES(?1,?2,?3,?4,?5,'external_agent',?6,?7,?7)",
                params![
                    id,
                    input.meeting_id,
                    input.title,
                    input.markdown,
                    rev,
                    ctx.principal,
                    now
                ],
            )?;
            search::rebuild_note_on(tx, &id)?;
            consistency::outcome(tx, "note", &id, &rev, ctx)
        })
    }
    pub fn service_note(&self, id: &str) -> DataResult<Option<UserNote>> {
        safe_id(id)?;
        Ok(self.conn.lock().query_row("SELECT n.* FROM service_notes n LEFT JOIN service_meeting_state m ON n.meeting_id=m.meeting_id WHERE n.id=?1 AND (n.meeting_id IS NULL OR m.tombstoned=0)",[id],note_row).optional()?)
    }
    pub fn service_notes(
        &self,
        meeting_id: Option<&str>,
        standalone: bool,
        after: Option<&str>,
        limit: usize,
    ) -> DataResult<Vec<NoteListEntry>> {
        if !(1..=100).contains(&limit) || (standalone && meeting_id.is_some()) {
            return Err(DataError::Invalid("note page"));
        }
        if let Some(id) = meeting_id {
            safe_id(id)?;
        }
        if let Some(id) = after {
            safe_id(id)?;
        }
        let conn = self.conn.lock();
        let mut st=conn.prepare("SELECT n.id,n.meeting_id,n.title,n.revision,n.origin,n.created_at,n.updated_at FROM service_notes n LEFT JOIN service_meeting_state m ON n.meeting_id=m.meeting_id WHERE (n.meeting_id IS NULL OR m.tombstoned=0) AND (?1 IS NULL OR n.meeting_id=?1) AND (?2=0 OR n.meeting_id IS NULL) AND (?3 IS NULL OR n.id>?3) ORDER BY n.id LIMIT ?4")?;
        let rows = st
            .query_map(params![meeting_id, standalone, after, limit as i64], |r| {
                Ok(NoteListEntry {
                    id: r.get(0)?,
                    meeting_id: r.get(1)?,
                    title: r.get(2)?,
                    revision: r.get(3)?,
                    origin: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn service_update_note(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        patch: &NoteUpdate,
    ) -> DataResult<MutationOutcome> {
        safe_id(id)?;
        if patch.title.is_none() && patch.markdown.is_none() && patch.association.is_none() {
            return Err(DataError::Invalid("empty note patch"));
        }
        if let Some(t) = &patch.title {
            bounded_text(t, 1, 512)?;
        }
        if let Some(t) = &patch.markdown {
            bounded_text(t, 0, 262144)?;
        }
        consistency::mutate(self, ctx, "update_note", &(id, expected, patch), |tx| {
            let old = tx
                .query_row("SELECT * FROM service_notes WHERE id=?1", [id], note_row)
                .optional()?
                .ok_or(DataError::NotFound)?;
            check_revision(&old.revision, expected)?;
            if let Some(m) = &old.meeting_id {
                live_meeting(tx, m)?;
            }
            if let Some(p) = &patch.association {
                let value = match p {
                    AssociationPatch::Set(v) => {
                        safe_id(v)?;
                        live_meeting(tx, v)?;
                        Some(v)
                    }
                    AssociationPatch::Clear => None,
                };
                tx.execute(
                    "UPDATE service_notes SET meeting_id=?2 WHERE id=?1",
                    params![id, value],
                )?;
            }
            if let Some(t) = &patch.title {
                tx.execute(
                    "UPDATE service_notes SET title=?2 WHERE id=?1",
                    params![id, t],
                )?;
            }
            if let Some(t) = &patch.markdown {
                tx.execute(
                    "UPDATE service_notes SET markdown=?2 WHERE id=?1",
                    params![id, t],
                )?;
            }
            let rev = revision();
            tx.execute("UPDATE service_notes SET revision=?2,origin='external_agent',actor=?3,updated_at=?4 WHERE id=?1",params![id,rev,ctx.principal,Utc::now()])?;
            search::rebuild_note_on(tx, id)?;
            consistency::outcome(tx, "note", id, &rev, ctx)
        })
    }
    pub fn service_delete_note(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        safe_id(id)?;
        consistency::mutate(self, ctx, "delete_note", &(id, expected), |tx| {
            let old = tx
                .query_row("SELECT * FROM service_notes WHERE id=?1", [id], note_row)
                .optional()?
                .ok_or(DataError::NotFound)?;
            check_revision(&old.revision, expected)?;
            if let Some(m) = &old.meeting_id {
                live_meeting(tx, m)?;
            }
            execution::invalidate_publications_on(tx, "note", id)?;
            tx.execute("DELETE FROM service_notes WHERE id=?1", [id])?;
            search::rebuild_note_on(tx, id)?;
            consistency::outcome(tx, "note", id, &revision(), ctx)
        })
    }
}

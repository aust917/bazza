//! Folders, tags and share tokens.

use crate::{Meeting, Result, Store};
#[cfg(test)]
use chrono::DateTime;
use chrono::Utc;
use rusqlite::{params, params_from_iter, OptionalExtension};

pub use bazza_domain::{Folder, MeetingFilter, Share, Tag};

fn meeting_filter_query(filter: &MeetingFilter) -> (String, Vec<String>) {
    let mut parameters = Vec::new();
    let mut sql = String::new();
    if let Some(Some(folder)) = &filter.folder {
        // UNION terminates on malformed folder cycles. The seed retains meetings
        // assigned directly to a missing folder, as the old subtree helper did.
        sql.push_str("WITH RECURSIVE selected_folders(id) AS (
            SELECT ?1 UNION SELECT f.id FROM folders f JOIN selected_folders p ON f.parent_id=p.id
        ) ");
        parameters.push(folder.clone());
    }
    sql.push_str("SELECT m.* FROM meetings m");
    if let Some(tag) = &filter.tag_id {
        parameters.push(tag.clone());
        sql.push_str(&format!(" JOIN meeting_tags mt ON mt.meeting_id=m.id AND mt.tag_id=?{}", parameters.len()));
    }
    match &filter.folder {
        None => {},
        Some(None) => sql.push_str(" WHERE m.folder_id IS NULL"),
        Some(Some(_)) => sql.push_str(" WHERE m.folder_id IN (SELECT id FROM selected_folders)"),
    }
    // The old load-all filter preserved row order for equal timestamps.
    sql.push_str(" ORDER BY m.started_at DESC,m.rowid ASC");
    (sql, parameters)
}

impl Store {
    // ---- folders ----
    pub fn list_folders(&self) -> Result<Vec<Folder>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT f.id, f.name, f.parent_id, f.default_template_id, (SELECT COUNT(*) FROM meetings m WHERE m.folder_id = f.id)
             FROM folders f ORDER BY f.name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Folder { id: r.get(0)?, name: r.get(1)?, parent_id: r.get(2)?, default_template_id: r.get(3)?, meeting_count: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_folder(&self, name: &str, parent_id: Option<&str>) -> Result<Folder> {
        let f = Folder { id: uuid::Uuid::new_v4().to_string(), name: name.trim().to_string(), parent_id: parent_id.map(str::to_string), default_template_id: None, meeting_count: 0 };
        self.conn.lock().execute(
            "INSERT INTO folders(id, name, parent_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![f.id, f.name, f.parent_id, Utc::now()],
        )?;
        Ok(f)
    }

    pub fn update_folder(&self, id: &str, name: &str, parent_id: Option<&str>, default_template_id: Option<&str>) -> Result<()> {
        if parent_id == Some(id) {
            return Ok(());
        }
        self.conn.lock().execute(
            "UPDATE folders SET name = ?2, parent_id = ?3, default_template_id = ?4 WHERE id = ?1",
            params![id, name.trim(), parent_id, default_template_id],
        )?;
        Ok(())
    }

    /// Delete a folder; its meetings and subfolders move to its parent.
    pub fn delete_folder(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let parent: Option<String> = tx.query_row("SELECT parent_id FROM folders WHERE id = ?1", params![id], |r| r.get(0)).optional()?.flatten();
        tx.execute("UPDATE meetings SET folder_id = ?2 WHERE folder_id = ?1", params![id, parent])?;
        tx.execute("UPDATE folders SET parent_id = ?2 WHERE parent_id = ?1", params![id, parent])?;
        tx.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// The folder and all its descendants.
    pub fn folder_subtree(&self, id: &str) -> Result<Vec<String>> {
        let folders = self.list_folders()?;
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            for f in folders.iter().filter(|f| f.parent_id.as_deref() == Some(cur.as_str())) {
                if !out.contains(&f.id) {
                    out.push(f.id.clone());
                }
            }
            i += 1;
        }
        Ok(out)
    }

    pub fn move_meeting(&self, meeting_id: &str, folder_id: Option<&str>) -> Result<()> {
        self.conn.lock().execute("UPDATE meetings SET folder_id = ?2 WHERE id = ?1", params![meeting_id, folder_id])?;
        Ok(())
    }

    pub fn list_meetings_filtered(&self, filter: &MeetingFilter) -> Result<Vec<Meeting>> {
        let (sql, parameters) = meeting_filter_query(filter);
        let conn = self.conn.lock();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters.iter()), crate::meetings::meeting_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ---- tags ----
    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare("SELECT t.id, t.name, (SELECT COUNT(*) FROM meeting_tags mt WHERE mt.tag_id = t.id) FROM tags t ORDER BY t.name")?;
        let rows = st.query_map([], |r| Ok(Tag { id: r.get(0)?, name: r.get(1)?, meeting_count: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Attach a tag by name (created if new). Returns the tag.
    pub fn tag_meeting(&self, meeting_id: &str, name: &str) -> Result<Tag> {
        let name = name.trim();
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let id: Option<String> = tx.query_row("SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE", params![name], |r| r.get(0)).optional()?;
        let id = match id {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute("INSERT INTO tags(id, name) VALUES (?1, ?2)", params![id, name])?;
                id
            }
        };
        tx.execute("INSERT OR IGNORE INTO meeting_tags(meeting_id, tag_id) VALUES (?1, ?2)", params![meeting_id, id])?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM meeting_tags WHERE tag_id = ?1", params![id], |r| r.get(0))?;
        let tag_name: String = tx.query_row("SELECT name FROM tags WHERE id = ?1", params![id], |r| r.get(0))?;
        tx.commit()?;
        Ok(Tag { id, name: tag_name, meeting_count: count })
    }

    pub fn untag_meeting(&self, meeting_id: &str, tag_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM meeting_tags WHERE meeting_id = ?1 AND tag_id = ?2", params![meeting_id, tag_id])?;
        conn.execute("DELETE FROM tags WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM meeting_tags WHERE tag_id = ?1)", params![tag_id])?;
        Ok(())
    }

    pub fn meeting_tags(&self, meeting_id: &str) -> Result<Vec<Tag>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT t.id, t.name, (SELECT COUNT(*) FROM meeting_tags x WHERE x.tag_id = t.id) FROM tags t
             JOIN meeting_tags mt ON mt.tag_id = t.id WHERE mt.meeting_id = ?1 ORDER BY t.name",
        )?;
        let rows = st.query_map(params![meeting_id], |r| Ok(Tag { id: r.get(0)?, name: r.get(1)?, meeting_count: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ---- shares ----
    pub fn create_share(&self, meeting_id: &str, kind: &str, start_ms: Option<i64>, end_ms: Option<i64>) -> Result<Share> {
        let token: String = uuid::Uuid::new_v4().simple().to_string();
        let title: String = self.conn.lock().query_row("SELECT title FROM meetings WHERE id = ?1", params![meeting_id], |r| r.get(0))?;
        let s = Share { token, meeting_id: meeting_id.into(), meeting_title: title, kind: kind.into(), start_ms, end_ms, enabled: true, created_at: Utc::now() };
        self.conn.lock().execute(
            "INSERT INTO shares(token, meeting_id, kind, start_ms, end_ms, enabled, created_at) VALUES (?1,?2,?3,?4,?5,1,?6)",
            params![s.token, s.meeting_id, s.kind, s.start_ms, s.end_ms, s.created_at],
        )?;
        Ok(s)
    }

    pub fn list_shares(&self) -> Result<Vec<Share>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT s.token, s.meeting_id, m.title, s.kind, s.start_ms, s.end_ms, s.enabled, s.created_at FROM shares s
             JOIN meetings m ON m.id = s.meeting_id ORDER BY s.created_at DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Share { token: r.get(0)?, meeting_id: r.get(1)?, meeting_title: r.get(2)?, kind: r.get(3)?, start_ms: r.get(4)?, end_ms: r.get(5)?, enabled: r.get::<_, i64>(6)? != 0, created_at: r.get(7)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_share(&self, token: &str) -> Result<Option<Share>> {
        Ok(self.list_shares()?.into_iter().find(|s| s.token == token && s.enabled))
    }

    pub fn set_share_enabled(&self, token: &str, enabled: bool) -> Result<()> {
        self.conn.lock().execute("UPDATE shares SET enabled = ?2 WHERE token = ?1", params![token, enabled as i64])?;
        Ok(())
    }

    pub fn delete_share(&self, token: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM shares WHERE token = ?1", params![token])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_filter_wire_preserves_legacy_all_and_distinguishes_unfiled() {
        for (wire, expected) in [
            (serde_json::json!({}), None),
            (serde_json::json!({"folder": null}), None),
            (serde_json::json!({"folder": "folder-id"}), Some(Some("folder-id".into()))),
            (serde_json::json!({"folder": [null]}), Some(None)),
            (serde_json::json!({"folder": ["folder-id"]}), Some(Some("folder-id".into()))),
        ] {
            let filter: MeetingFilter = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(filter.folder, expected, "wire={wire}");
        }
        for invalid in [
            serde_json::json!([]), serde_json::json!([null,null]), serde_json::json!(["a","b"]),
            serde_json::json!([1]), serde_json::json!([[null]]), serde_json::json!({"id":"folder-id"}),
            serde_json::json!(true), serde_json::json!(1),
        ] {
            assert!(serde_json::from_value::<MeetingFilter>(serde_json::json!({"folder":invalid})).is_err());
        }
        let store = Store::open_in_memory().unwrap();
        let unfiled = Meeting::new_recording("Unfiled", None);
        let mut filed = Meeting::new_recording("Filed", None);
        filed.folder_id = Some("folder-id".into());
        store.create_meeting(&unfiled).unwrap();
        store.create_meeting(&filed).unwrap();
        let all: MeetingFilter = serde_json::from_value(serde_json::json!({"folder":null})).unwrap();
        let unfiled_only: MeetingFilter = serde_json::from_value(serde_json::json!({"folder":[null]})).unwrap();
        let folder_only: MeetingFilter = serde_json::from_value(serde_json::json!({"folder":["folder-id"]})).unwrap();
        assert_eq!(store.list_meetings_filtered(&all).unwrap().len(), 2);
        assert_eq!(store.list_meetings_filtered(&unfiled_only).unwrap(), [unfiled]);
        assert_eq!(store.list_meetings_filtered(&folder_only).unwrap(), [filed]);
    }

    #[test]
    fn folders_nest_and_delete_reparents() {
        let st = Store::open_in_memory().unwrap();
        let root = st.create_folder("Clients", None).unwrap();
        let sub = st.create_folder("Acme", Some(&root.id)).unwrap();
        let m = Meeting::new_recording("call", None);
        st.create_meeting(&m).unwrap();
        st.move_meeting(&m.id, Some(&sub.id)).unwrap();
        assert_eq!(st.folder_subtree(&root.id).unwrap().len(), 2);
        let in_root = st.list_meetings_filtered(&MeetingFilter { folder: Some(Some(root.id.clone())), tag_id: None }).unwrap();
        assert_eq!(in_root.len(), 1);
        let unfiled = st.list_meetings_filtered(&MeetingFilter { folder: Some(None), tag_id: None }).unwrap();
        assert!(unfiled.is_empty());
        st.delete_folder(&sub.id).unwrap();
        assert_eq!(st.get_meeting(&m.id).unwrap().unwrap().folder_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(st.list_folders().unwrap()[0].meeting_count, 1);
    }

    #[test]
    fn tags_and_shares() {
        let st = Store::open_in_memory().unwrap();
        let m = Meeting::new_recording("call", None);
        st.create_meeting(&m).unwrap();
        let t = st.tag_meeting(&m.id, " Sales ").unwrap();
        assert_eq!(t.name, "Sales");
        let same = st.tag_meeting(&m.id, "sales").unwrap();
        assert_eq!(same.id, t.id);
        assert_eq!(st.list_tags().unwrap().len(), 1);
        assert_eq!(st.list_meetings_filtered(&MeetingFilter { folder: None, tag_id: Some(t.id.clone()) }).unwrap().len(), 1);
        st.untag_meeting(&m.id, &t.id).unwrap();
        assert!(st.list_tags().unwrap().is_empty(), "orphan tags are removed");

        let s = st.create_share(&m.id, "clip", Some(1000), Some(5000)).unwrap();
        assert_eq!(s.token.len(), 32);
        assert!(st.get_share(&s.token).unwrap().is_some());
        st.set_share_enabled(&s.token, false).unwrap();
        assert!(st.get_share(&s.token).unwrap().is_none());
        st.delete_share(&s.token).unwrap();
        assert!(st.list_shares().unwrap().is_empty());
    }

    #[test]
    fn sql_filters_preserve_all_unfiled_subtrees_tags_order_and_meeting_fields() {
        let store = Store::open_in_memory().unwrap();
        let root = store.create_folder("Projects", None).unwrap();
        let child = store.create_folder("Client", Some(&root.id)).unwrap();
        let grandchild = store.create_folder("Planning", Some(&child.id)).unwrap();
        let other = store.create_folder("Other", None).unwrap();
        let orphan_child = store.create_folder("Legacy", Some("missing-folder")).unwrap();
        let destinations = [None, Some(root.id.as_str()), Some(child.id.as_str()), Some(grandchild.id.as_str()), Some(other.id.as_str()), Some("missing-folder"), Some(orphan_child.id.as_str())];
        let mut meetings = Vec::new();
        for (index, folder) in destinations.iter().cycle().take(14).enumerate() {
            let mut meeting = Meeting::new_recording(&format!("Meeting {index}"), Some("zoom"));
            meeting.started_at = DateTime::from_timestamp(1_780_000_000 + (index / 2) as i64, 0).unwrap();
            meeting.folder_id = folder.map(str::to_string);
            meeting.participants = vec![format!("Participant {index}"), "Анна".into()];
            meeting.error = Some(format!("optional warning {index}"));
            meeting.calendar_uid = Some(format!("event-{index}"));
            meeting.title_auto = index % 2 == 0;
            meeting.duration_ms = index as i64 * 1000;
            store.create_meeting(&meeting).unwrap();
            meetings.push(meeting);
        }
        let mut tag_a = None;
        let mut tag_b = None;
        let mut tagged_a = Vec::new();
        let mut tagged_b = Vec::new();
        // Reverse membership insertion to exercise equal-timestamp order across joins.
        for (index, meeting) in meetings.iter().enumerate().rev() {
            if index % 2 == 0 {
                tag_a = Some(store.tag_meeting(&meeting.id, "A").unwrap().id);
                tagged_a.push(meeting.id.clone());
            }
            if index % 3 == 0 {
                tag_b = Some(store.tag_meeting(&meeting.id, "B").unwrap().id);
                tagged_b.push(meeting.id.clone());
            }
        }
        let folders = [None, Some(None), Some(Some(root.id.clone())), Some(Some(child.id.clone())), Some(Some(other.id.clone())), Some(Some("missing-folder".into())), Some(Some("no-folder".into()))];
        let tags = [None, tag_a.clone(), tag_b.clone(), Some("no-tag".into())];
        let all = store.list_meetings().unwrap();
        for folder in &folders {
            for tag in &tags {
                let filter = MeetingFilter {folder:folder.clone(),tag_id:tag.clone()};
                // The original load-all algorithm is retained only as a compatibility oracle.
                let subtree = folder.as_ref().and_then(|id| id.as_ref()).map(|id| store.folder_subtree(id).unwrap());
                let expected: Vec<_> = all.iter().filter(|meeting| match folder {
                    None => true,
                    Some(None) => meeting.folder_id.is_none(),
                    Some(Some(_)) => meeting.folder_id.as_ref().is_some_and(|id| subtree.as_ref().unwrap().contains(id)),
                }).filter(|meeting| match tag {
                    None => true,
                    Some(id) if Some(id) == tag_a.as_ref() => tagged_a.contains(&meeting.id),
                    Some(id) if Some(id) == tag_b.as_ref() => tagged_b.contains(&meeting.id),
                    Some(_) => false,
                }).cloned().collect();
                assert_eq!(store.list_meetings_filtered(&filter).unwrap(), expected, "filter={filter:?}");
            }
        }
    }

    #[test]
    fn folder_filter_terminates_on_cycles_without_duplicate_meetings() {
        let store = Store::open_in_memory().unwrap();
        let root = store.create_folder("Root", None).unwrap();
        let child = store.create_folder("Child", Some(&root.id)).unwrap();
        store.update_folder(&root.id, "Root", Some(&child.id), None).unwrap();
        for folder in [&root, &child] {
            let mut meeting = Meeting::new_recording(&folder.name, None);
            meeting.folder_id = Some(folder.id.clone());
            store.create_meeting(&meeting).unwrap();
        }
        let actual = store.list_meetings_filtered(&MeetingFilter {folder:Some(Some(root.id)),tag_id:None}).unwrap();
        assert_eq!(actual, store.list_meetings().unwrap());
    }

    #[test]
    fn filtered_meeting_queries_use_folder_and_tag_indexes() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn.lock();
        for (filter, index) in [
            (MeetingFilter {folder:Some(Some("folder".into())),tag_id:None}, "meetings_folder"),
            (MeetingFilter {folder:Some(None),tag_id:None}, "meetings_folder"),
            (MeetingFilter {folder:None,tag_id:Some("tag".into())}, "meeting_tags_tag"),
        ] {
            let (sql, parameters) = meeting_filter_query(&filter);
            let plan: Vec<String> = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap()
                .query_map(params_from_iter(parameters.iter()), |row| row.get(3)).unwrap()
                .collect::<rusqlite::Result<_>>().unwrap();
            assert!(plan.iter().any(|line| line.contains(index)), "filter={filter:?}, plan={plan:?}");
            assert!(!plan.iter().any(|line| line.starts_with("SCAN m")), "filter={filter:?}, plan={plan:?}");
        }
    }
}

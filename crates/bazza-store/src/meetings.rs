use crate::{Result, Store};
#[cfg(test)]
use chrono::Utc;
use rusqlite::{params, Row};

pub use bazza_domain::{Meeting, MeetingStatus};

fn parse_status(s: &str) -> MeetingStatus {
    match s {
        "recording" => MeetingStatus::Recording,
        "processing" => MeetingStatus::Processing,
        "ready" => MeetingStatus::Ready,
        _ => MeetingStatus::Failed,
    }
}

pub(crate) fn meeting_from_row(r: &Row) -> rusqlite::Result<Meeting> {
    Ok(Meeting {
        id: r.get("id")?,
        title: r.get("title")?,
        app: r.get("app")?,
        started_at: r.get("started_at")?,
        ended_at: r.get("ended_at")?,
        duration_ms: r.get("duration_ms")?,
        has_video: r.get::<_, i64>("has_video")? != 0,
        status: parse_status(&r.get::<_, String>("status")?),
        folder_id: r.get("folder_id")?,
        created_at: r.get("created_at")?,
        error: r.get("error")?,
        title_auto: r.get::<_, i64>("title_auto")? != 0,
        participants: serde_json::from_str(&r.get::<_, String>("participants_json")?)
            .unwrap_or_default(),
        calendar_uid: r.get("calendar_uid")?,
    })
}

const COLS: &str = "id, title, app, started_at, ended_at, duration_ms, has_video, status, folder_id, created_at, error, title_auto, participants_json, calendar_uid";

impl Store {
    pub fn create_meeting(&self, m: &Meeting) -> Result<()> {
        self.conn.lock().execute(
            &format!("INSERT INTO meetings({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"),
            params![m.id, m.title, m.app, m.started_at, m.ended_at, m.duration_ms, m.has_video as i64,
                m.status.as_str(), m.folder_id, m.created_at, m.error, m.title_auto as i64,
                serde_json::to_string(&m.participants).unwrap_or_else(|_| "[]".into()), m.calendar_uid],
        )?;
        Ok(())
    }

    pub fn update_meeting(&self, m: &Meeting) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE meetings SET title=?2, app=?3, started_at=?4, ended_at=?5, duration_ms=?6, has_video=?7,
             status=?8, folder_id=?9, error=?10, title_auto=?11, participants_json=?12, calendar_uid=?13 WHERE id=?1",
            params![m.id, m.title, m.app, m.started_at, m.ended_at, m.duration_ms, m.has_video as i64,
                m.status.as_str(), m.folder_id, m.error, m.title_auto as i64,
                serde_json::to_string(&m.participants).unwrap_or_else(|_| "[]".into()), m.calendar_uid],
        )?;
        Ok(())
    }

    /// Publish capture/pipeline state from a potentially stale job snapshot.
    /// User edits and source metadata remain owned by their explicit operations.
    /// An UPDATE never recreates a meeting deleted while the job was running.
    pub fn update_processing_state(&self, m: &Meeting) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE meetings SET ended_at=?2,duration_ms=?3,has_video=?4,status=?5,error=?6 WHERE id=?1",
            params![m.id,m.ended_at,m.duration_ms,m.has_video as i64,m.status.as_str(),m.error],
        )?;
        Ok(())
    }

    pub fn set_meeting_error(&self, id: &str, error: Option<&str>) -> Result<()> {
        self.conn.lock().execute("UPDATE meetings SET error=?2 WHERE id=?1", params![id, error])?;
        Ok(())
    }

    /// Replace the attendee list (deduplicated, order kept).
    pub fn set_participants(&self, id: &str, names: &[String]) -> Result<()> {
        let mut out: Vec<String> = Vec::new();
        for n in names {
            let n = n.trim();
            if !n.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(n)) {
                out.push(n.to_string());
            }
        }
        self.conn.lock().execute(
            "UPDATE meetings SET participants_json=?2 WHERE id=?1",
            params![id, serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())],
        )?;
        Ok(())
    }

    /// Set the title. `by_user` pins it so auto-titling never overwrites it again;
    /// an automatic title only lands while the meeting still has an auto title.
    pub fn set_title(&self, id: &str, title: &str, by_user: bool) -> Result<bool> {
        let n = if by_user {
            self.conn.lock().execute("UPDATE meetings SET title=?2, title_auto=0 WHERE id=?1", params![id, title])?
        } else {
            self.conn.lock().execute("UPDATE meetings SET title=?2 WHERE id=?1 AND title_auto=1", params![id, title])?
        };
        Ok(n > 0)
    }

    pub fn get_meeting(&self, id: &str) -> Result<Option<Meeting>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(&format!("SELECT {COLS} FROM meetings WHERE id=?1"))?;
        let mut rows = st.query(params![id])?;
        Ok(match rows.next()? {
            Some(r) => Some(meeting_from_row(r)?),
            None => None,
        })
    }

    /// Newest first.
    pub fn list_meetings(&self) -> Result<Vec<Meeting>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(&format!("SELECT {COLS} FROM meetings ORDER BY started_at DESC"))?;
        let rows = st.query_map([], meeting_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_meeting(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM meetings WHERE id=?1", params![id])?;
        crate::settings::clear_meeting_voice_queries_on(&tx, id)?;
        crate::transcript_versions::delete_versions_on(&tx, id)?;
        tx.execute("DELETE FROM settings WHERE key IN (?1,?2,?3)",params![format!("stt:{id}"),format!("stt_previous:{id}"),format!("highlights:{id}")])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_sql_status_still_reads_as_failed_without_relaxing_wire_status() {
        let store = Store::open_in_memory().unwrap();
        let meeting = Meeting::new_recording("Legacy status", None);
        store.create_meeting(&meeting).unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE meetings SET status='unknown-legacy-status' WHERE id=?1",
                [&meeting.id],
            )
            .unwrap();
        assert_eq!(
            store.get_meeting(&meeting.id).unwrap().unwrap().status,
            MeetingStatus::Failed
        );
        assert_eq!(
            store.list_meetings().unwrap()[0].status,
            MeetingStatus::Failed
        );
        assert!(serde_json::from_str::<MeetingStatus>("\"unknown-legacy-status\"").is_err());
    }

    #[test]
    fn stale_processing_snapshot_preserves_user_edits_and_does_not_restore_deleted_meetings() {
        let store = Store::open_in_memory().unwrap();
        let original = Meeting::new_recording("Original title", Some("zoom"));
        let untouched = Meeting::new_recording("Another meeting", None);
        store.create_meeting(&original).unwrap();
        store.create_meeting(&untouched).unwrap();
        let mut job_snapshot = original.clone();

        let mut edited = original.clone();
        edited.title = "User title".into();
        edited.title_auto = false;
        edited.participants = vec!["Анна".into(),"Sam".into()];
        edited.app = Some("teams".into());
        edited.calendar_uid = Some("updated-calendar-event".into());
        store.update_meeting(&edited).unwrap();
        let folder = store.create_folder("User destination", None).unwrap();
        store.move_meeting(&edited.id, Some(&folder.id)).unwrap();
        let mut expected = store.get_meeting(&edited.id).unwrap().unwrap();

        job_snapshot.status = MeetingStatus::Ready;
        job_snapshot.ended_at = Some(Utc::now());
        job_snapshot.duration_ms = 42_000;
        job_snapshot.has_video = true;
        job_snapshot.error = Some("Optional processing warning".into());
        // Even unexpected source metadata in a stale snapshot is not published.
        job_snapshot.started_at -= chrono::Duration::days(30);
        job_snapshot.created_at -= chrono::Duration::days(30);
        store.update_processing_state(&job_snapshot).unwrap();
        expected.status = job_snapshot.status;
        expected.ended_at = job_snapshot.ended_at;
        expected.duration_ms = job_snapshot.duration_ms;
        expected.has_video = job_snapshot.has_video;
        expected.error = job_snapshot.error.clone();
        assert_eq!(store.get_meeting(&edited.id).unwrap(), Some(expected));
        assert_eq!(store.get_meeting(&untouched.id).unwrap(), Some(untouched));

        store.delete_meeting(&edited.id).unwrap();
        store.update_processing_state(&job_snapshot).unwrap();
        assert!(store.get_meeting(&edited.id).unwrap().is_none());
    }

    #[test]
    fn errors_participants_and_title_pinning() {
        let s = Store::open_in_memory().unwrap();
        let m = Meeting::new_recording("Zoom call", Some("zoom"));
        s.create_meeting(&m).unwrap();
        s.set_meeting_error(&m.id, Some("summary: boom")).unwrap();
        s.set_participants(&m.id, &["Ann".into(), " ann ".into(), "Bob".into()]).unwrap();
        assert!(s.set_title(&m.id, "Auto title", false).unwrap());
        let g = s.get_meeting(&m.id).unwrap().unwrap();
        assert_eq!(g.error.as_deref(), Some("summary: boom"));
        assert_eq!(g.participants, vec!["Ann", "Bob"]);
        assert_eq!(g.title, "Auto title");
        assert!(g.title_auto);
        assert!(s.set_title(&m.id, "Mine", true).unwrap());
        assert!(!s.set_title(&m.id, "Auto again", false).unwrap());
        let g = s.get_meeting(&m.id).unwrap().unwrap();
        assert_eq!(g.title, "Mine");
        assert!(!g.title_auto);
    }

    #[test]
    fn create_list_get_delete_meeting() {
        let s = Store::open_in_memory().unwrap();
        let m = Meeting::new_recording("Zoom call", Some("zoom"));
        s.create_meeting(&m).unwrap();
        assert_eq!(s.list_meetings().unwrap().len(), 1);
        let got = s.get_meeting(&m.id).unwrap().unwrap();
        assert_eq!(got.title, "Zoom call");
        assert_eq!(got.status, MeetingStatus::Recording);
        s.delete_meeting(&m.id).unwrap();
        assert!(s.get_meeting(&m.id).unwrap().is_none());
    }

    #[test]
    fn update_persists_status_and_duration() {
        let s = Store::open_in_memory().unwrap();
        let mut m = Meeting::new_recording("x", None);
        s.create_meeting(&m).unwrap();
        m.status = MeetingStatus::Ready;
        m.duration_ms = 4200;
        m.has_video = true;
        s.update_meeting(&m).unwrap();
        let got = s.get_meeting(&m.id).unwrap().unwrap();
        assert_eq!(got.status, MeetingStatus::Ready);
        assert_eq!(got.duration_ms, 4200);
        assert!(got.has_video);
    }

    #[test]
    fn deletion_removes_meeting_settings_but_preserves_other_meetings_and_saved_voices() {
        let store = Store::open_in_memory().unwrap();
        let meeting = Meeting::new_recording("delete me",None);
        store.create_meeting(&meeting).unwrap();
        let selected = format!("stt:{}",meeting.id);
        let previous = format!("stt_previous:{}",meeting.id);
        let voice = format!("speaker_embeddings:{}", meeting.id);
        let versioned_voice = format!("speaker_embeddings:old-policy:{}", meeting.id);
        let highlights = format!("highlights:{}", meeting.id);
        let other_voice = format!("speaker_embeddings:old-policy:other-{}", meeting.id);
        for key in [&selected,&previous,&voice,&versioned_voice,&highlights,&other_voice,"stt:another-meeting","highlights:another-meeting","private:soniox_api_key"] {
            store.set_setting(key,&"synthetic stored value").unwrap();
        }
        let speaker = crate::Speaker { id: "saved-voice".into(), name: "Saved voice".into(), embedding: vec![1.0, 0.0] };
        store.upsert_speaker(&speaker).unwrap();
        let profiles = store.list_voice_profiles().unwrap();
        let samples = store.list_voice_samples(&speaker.id).unwrap();
        store.delete_meeting(&meeting.id).unwrap();
        assert!(store.get_meeting(&meeting.id).unwrap().is_none());
        assert!(store.get_setting::<String>(&selected).unwrap().is_none());
        assert!(store.get_setting::<String>(&previous).unwrap().is_none());
        for key in [&voice,&versioned_voice,&highlights] {
            assert!(store.get_setting::<String>(key).unwrap().is_none());
        }
        assert!(store.get_setting::<String>(&other_voice).unwrap().is_some());
        assert!(store.get_setting::<String>("stt:another-meeting").unwrap().is_some());
        assert!(store.get_setting::<String>("highlights:another-meeting").unwrap().is_some());
        assert!(store.get_setting::<String>("private:soniox_api_key").unwrap().is_some());
        assert_eq!(store.list_voice_profiles().unwrap(), profiles);
        assert_eq!(store.list_voice_samples(&speaker.id).unwrap(), samples);
    }

    #[test]
    fn meeting_delete_rolls_back_cascades_and_settings_on_cache_cleanup_failure() {
        let store = Store::open_in_memory().unwrap();
        let meeting = Meeting::new_recording("rollback deletion", None);
        store.create_meeting(&meeting).unwrap();
        store.replace_segments(&meeting.id, &[crate::NewSegment {
            start_ms: 0, end_ms: 1000, speaker: None, text: "Original raw".into(),
        }]).unwrap();
        let before = store.segments(&meeting.id).unwrap();
        let keys = [
            format!("speaker_embeddings:policy:{}", meeting.id),
            format!("highlights:{}", meeting.id),
            format!("stt:{}", meeting.id),
        ];
        for key in &keys {
            store.set_setting(key, &"rollback observation").unwrap();
        }
        store.conn.lock().execute_batch("CREATE TRIGGER fail_meeting_settings_cleanup BEFORE DELETE ON settings BEGIN SELECT RAISE(ABORT,'injected cache cleanup failure'); END;").unwrap();
        assert!(store.delete_meeting(&meeting.id).is_err());
        assert_eq!(store.get_meeting(&meeting.id).unwrap(), Some(meeting));
        assert_eq!(store.segments(&before[0].meeting_id).unwrap(), before);
        for key in &keys {
            assert_eq!(store.get_setting::<String>(key).unwrap().as_deref(), Some("rollback observation"));
        }
    }
}

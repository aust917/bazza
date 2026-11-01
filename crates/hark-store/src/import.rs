//! One transaction for imported metadata, with recoverable media installation.

use crate::{Meeting, MeetingSpeaker, Result, Segment, Store, StoreError, Summary};
use rusqlite::{params, OptionalExtension};
use std::path::Path;

const PENDING: &str = ".hark-import-pending";

/// Imported speaker labels deliberately do not teach local voice memory.
pub struct MeetingImport<'a> {
    pub meeting: &'a Meeting,
    pub segments: &'a [Segment],
    pub speakers: &'a [MeetingSpeaker],
    pub summary: Option<&'a Summary>,
    pub highlights: &'a [u64],
    pub tags: &'a [String],
    /// Provider provenance already reduced to the documented transcript fields.
    pub transcript: Option<&'a serde_json::Value>,
}

impl Store {
    /// Install already-validated staged media and all metadata together.
    /// Returns false for an existing ID. Failures roll back metadata and remove
    /// only the directory created by this call. A pending marker permits retry
    /// after a process interruption between file installation and DB commit.
    pub fn import_meeting(&self, data: &MeetingImport<'_>, staged: &Path) -> Result<bool> {
        let m = data.meeting;
        let uuid = uuid::Uuid::parse_str(&m.id).map_err(|_| StoreError::Other("invalid imported meeting ID".into()))?;
        if uuid.to_string() != m.id {
            return Err(StoreError::Other("imported meeting ID must be a canonical UUID".into()));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        if tx.query_row("SELECT 1 FROM meetings WHERE id=?1", params![m.id], |_| Ok(())).optional()?.is_some() {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO meetings(id,title,app,started_at,ended_at,duration_ms,has_video,status,folder_id,created_at,error,title_auto,participants_json,calendar_uid)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,?9,?10,?11,?12,?13)",
            params![m.id,m.title,m.app,m.started_at,m.ended_at,m.duration_ms,m.has_video as i64,m.status.as_str(),
                m.created_at,m.error,m.title_auto as i64,serde_json::to_string(&m.participants)?,m.calendar_uid],
        )?;
        for s in data.segments {
            tx.execute(
                "INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text,clean_text) VALUES (?1,?2,?3,?4,?5,?6)",
                params![m.id,s.start_ms,s.end_ms,s.speaker,s.text,s.clean_text],
            )?;
        }
        for s in data.speakers {
            tx.execute("INSERT INTO meeting_speakers(meeting_id,label) VALUES (?1,?2)", params![m.id,s.label])?;
        }
        if let Some(s) = data.summary {
            tx.execute(
                "INSERT INTO summaries(meeting_id,template_id,markdown,structured_json,model,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![m.id,s.template_id,s.markdown,serde_json::to_string(&s.structured)?,s.model,s.created_at],
            )?;
        }
        if !data.highlights.is_empty() {
            tx.execute("INSERT INTO settings(key,value) VALUES (?1,?2)",
                params![format!("highlights:{}",m.id),serde_json::to_string(data.highlights)?])?;
        }
        if let Some(transcript) = data.transcript {
            tx.execute("INSERT INTO settings(key,value) VALUES (?1,?2)",
                params![format!("stt:{}",m.id),serde_json::to_string(transcript)?])?;
        }
        for name in data.tags {
            let name = name.trim();
            if name.is_empty() { continue; }
            let id: Option<String> = tx.query_row("SELECT id FROM tags WHERE name=?1 COLLATE NOCASE", params![name], |r| r.get(0)).optional()?;
            let id = match id {
                Some(id) => id,
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute("INSERT INTO tags(id,name) VALUES (?1,?2)",params![id,name])?;
                    id
                }
            };
            tx.execute("INSERT OR IGNORE INTO meeting_tags(meeting_id,tag_id) VALUES (?1,?2)",params![m.id,id])?;
        }
        for (start, end, text) in crate::ai::build_chunks(data.segments) {
            tx.execute("INSERT INTO chunks(meeting_id,start_ms,end_ms,text) VALUES (?1,?2,?3,?4)",params![m.id,start,end,text])?;
        }

        let recordings = self.root().join("recordings");
        std::fs::create_dir_all(&recordings)?;
        let recordings = recordings.canonicalize()?;
        let dest = recordings.join(&m.id);
        if std::fs::symlink_metadata(&dest).is_ok() {
            // Never remove an unrelated orphan recording. Only our durable marker
            // authorizes cleanup after an interrupted import.
            let md = std::fs::symlink_metadata(&dest)?;
            if md.file_type().is_symlink() || !md.is_dir() || dest.canonicalize()?.parent() != Some(recordings.as_path())
                || std::fs::read_to_string(dest.join(PENDING)).ok().as_deref() != Some(m.id.as_str()) {
                return Err(StoreError::Other("recording directory already exists; existing files were preserved".into()));
            }
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::create_dir(&dest)?;
        let install = (|| -> Result<()> {
            std::fs::write(dest.join(PENDING), &m.id)?;
            for entry in std::fs::read_dir(staged)? {
                let entry = entry?;
                let name = entry.file_name();
                if !matches!(name.to_str(), Some("mix.wav" | "mic.wav" | "sys.wav" | "screen.mp4")) || !entry.file_type()?.is_file() {
                    return Err(StoreError::Other("invalid staged media".into()));
                }
                std::fs::rename(entry.path(), dest.join(name))?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = install {
            if let Err(cleanup) = std::fs::remove_dir_all(&dest) {
                return Err(StoreError::Other(format!("{error}; import cleanup failed: {cleanup}")));
            }
            return Err(error);
        }
        // If interrupted here, the committed DB entry still makes retry a skip.
        let _ = std::fs::remove_file(dest.join(PENDING));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_failure_rolls_back_everything_and_retry_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("hark.db")).unwrap();
        let meeting = Meeting::new_recording("import",None);
        let staged = tempfile::tempdir_in(root.path()).unwrap();
        std::fs::write(staged.path().join("mix.wav"),b"audio").unwrap();
        let speaker = MeetingSpeaker {meeting_id:meeting.id.clone(),label:"Ann".into(),speaker_id:None,suggested_id:None,suggested_name:None,suggested_score:None};
        let duplicate = [speaker.clone(),speaker];
        let mut data = MeetingImport {meeting:&meeting,segments:&[],speakers:&duplicate,summary:None,highlights:&[],tags:&[],transcript:None};
        assert!(store.import_meeting(&data,staged.path()).is_err());
        assert!(store.get_meeting(&meeting.id).unwrap().is_none());
        assert!(!root.path().join("recordings").join(&meeting.id).exists());
        data.speakers = &[];
        assert!(store.import_meeting(&data,staged.path()).unwrap());
        assert!(!store.import_meeting(&data,staged.path()).unwrap());
    }

    #[test]
    fn existing_orphan_is_preserved_and_interrupted_import_is_retryable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("hark.db")).unwrap();
        let meeting = Meeting::new_recording("import",None);
        let staged = tempfile::tempdir_in(root.path()).unwrap();
        std::fs::write(staged.path().join("mix.wav"),b"audio").unwrap();
        let dest = store.recordings_dir(&meeting.id);
        std::fs::write(dest.join("mic.wav"),b"original").unwrap();
        let data = MeetingImport {meeting:&meeting,segments:&[],speakers:&[],summary:None,highlights:&[],tags:&[],transcript:None};
        assert!(store.import_meeting(&data,staged.path()).is_err());
        assert_eq!(std::fs::read(dest.join("mic.wav")).unwrap(),b"original");
        assert!(store.get_meeting(&meeting.id).unwrap().is_none());
        std::fs::write(dest.join(PENDING),&meeting.id).unwrap();
        assert!(store.import_meeting(&data,staged.path()).unwrap());
        assert_eq!(std::fs::read(dest.join("mix.wav")).unwrap(),b"audio");
        assert!(!dest.join(PENDING).exists());
    }
}

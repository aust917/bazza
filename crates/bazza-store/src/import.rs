//! One transaction for imported metadata, with recoverable media installation.

use crate::{Meeting, MeetingSpeaker, Result, Segment, Store, StoreError, Summary};
use rusqlite::{params, OptionalExtension};
use std::path::Path;

const PENDING: &str = ".bazza-import-pending";

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
        self.import_meeting_with_versions(data, staged, None)
    }

    /// Import a typed version history in the same transaction as media and the
    /// canonical transcript. Existing callers and v1 bundles may omit it.
    pub fn import_meeting_with_versions(&self, data: &MeetingImport<'_>, staged: &Path,
        versions: Option<&bazza_domain::transcript_versions::TranscriptVersionArchive>) -> Result<bool> {
        let m = data.meeting;
        let uuid = uuid::Uuid::parse_str(&m.id).map_err(|_| StoreError::Other("invalid imported meeting ID".into()))?;
        if uuid.to_string() != m.id {
            return Err(StoreError::Other("imported meeting ID must be a canonical UUID".into()));
        }
        for segment in data.segments {
            segment.validate_manual_correction().map_err(|error| StoreError::Other(error.to_string()))?;
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
                "INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text,clean_text,clean_text_manual) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![m.id,s.start_ms,s.end_ms,s.speaker,s.text,s.clean_text,s.clean_text_manual],
            )?;
        }
        if let Some(archive) = versions {
            crate::transcript_versions::import_archive_on(&tx, &m.id, archive)
                .map_err(|error| StoreError::Other(error.to_string()))?;
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
    fn version_history_failure_rolls_back_canonical_rows_versions_and_media_then_retries() {
        use bazza_domain::transcript_versions::{NewTranscriptVersion,NewTranscriptVersionSegment,TranscriptSegmentProvenance,TranscriptSegmentSource,TranscriptVersionSource};
        let src_root = tempfile::tempdir().unwrap();
        let source = Store::open(&src_root.path().join("source.sqlite")).unwrap();
        let mut meeting = Meeting::new_recording("Versioned import",None);
        source.create_meeting(&meeting).unwrap();
        source.publish_initial_live_transcript(&meeting.id,"whisper-small",None,&[crate::NewSegment {
            start_ms:0,end_ms:100,speaker:Some("Alex".into()),text:"initial bot".into(),
        }]).unwrap();
        meeting.status = crate::MeetingStatus::Processing;
        source.update_processing_state(&meeting).unwrap();
        let input = NewTranscriptVersion { id:uuid::Uuid::new_v4().to_string(),source:TranscriptVersionSource::Combined,
            model:"whisper-small".into(),language:None,partial:false,segments:vec![NewTranscriptVersionSegment {
                segment:crate::NewSegment { start_ms:100,end_ms:200,speaker:Some("System".into()),text:"additional speech".into() },playback:None,
                provenance:Some(TranscriptSegmentProvenance { source:TranscriptSegmentSource::System,provider:None,participant_id:None,
                    source_start_ms:100,source_end_ms:200,uncertain:false }),
            }] };
        source.save_transcript_version(&meeting.id,&input).unwrap();
        meeting.status = crate::MeetingStatus::Ready;
        source.update_processing_state(&meeting).unwrap();
        let archive = source.transcript_version_archive(&meeting.id).unwrap().unwrap();
        let rows = source.segments(&meeting.id).unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
        let staged = tempfile::tempdir_in(root.path()).unwrap();
        std::fs::write(staged.path().join("mix.wav"),b"fixture audio").unwrap();
        let data = MeetingImport { meeting:&meeting,segments:&rows,speakers:&[],summary:None,highlights:&[],tags:&[],transcript:None };
        store.conn.lock().execute_batch("CREATE TRIGGER reject_version_archive BEFORE INSERT ON settings WHEN new.key LIKE 'transcript-version-publication:%' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        assert!(store.import_meeting_with_versions(&data,staged.path(),Some(&archive)).is_err());
        assert!(store.get_meeting(&meeting.id).unwrap().is_none());
        assert!(store.segments(&meeting.id).unwrap().is_empty());
        assert_eq!(store.conn.lock().query_row("SELECT count(*) FROM settings WHERE key LIKE 'transcript-version-%'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
        assert!(!root.path().join("recordings").join(&meeting.id).exists());
        assert!(staged.path().join("mix.wav").is_file());
        store.conn.lock().execute_batch("DROP TRIGGER reject_version_archive;").unwrap();
        assert!(store.import_meeting_with_versions(&data,staged.path(),Some(&archive)).unwrap());
        assert_eq!(store.transcript_versions(&meeting.id).unwrap().effective_id.as_deref(),Some(input.id.as_str()));
    }

    #[test]
    fn db_failure_rolls_back_everything_and_retry_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
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
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
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

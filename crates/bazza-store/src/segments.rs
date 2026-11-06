use crate::{Result, Store};
pub use bazza_domain::{
    ManualCorrectionError, NewSegment, SearchHit, Segment, MAX_TRANSCRIPT_CORRECTION_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, thiserror::Error)]
pub enum TranscriptCorrectionError {
    #[error("meeting not found")]
    MeetingNotFound,
    #[error("finish processing before editing the transcript")]
    NotReady,
    #[error("transcript segment does not belong to this meeting")]
    SegmentNotFound,
    #[error("transcript correction exceeds 65536 UTF-8 bytes")]
    TooLarge,
    #[error("invalid transcript correction")]
    Invalid,
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl From<ManualCorrectionError> for TranscriptCorrectionError {
    fn from(error: ManualCorrectionError) -> Self {
        match error {
            ManualCorrectionError::TooLarge => Self::TooLarge,
            ManualCorrectionError::Invalid => Self::Invalid,
        }
    }
}

fn validate_correction(text: Option<&str>) -> std::result::Result<(), TranscriptCorrectionError> {
    bazza_domain::validate_transcript_correction(text).map_err(Into::into)
}

fn segment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        start_ms: row.get(2)?,
        end_ms: row.get(3)?,
        speaker: row.get(4)?,
        text: row.get(5)?,
        clean_text: row.get(6)?,
        clean_text_manual: row.get(7)?,
    })
}

pub(crate) fn segments_on(conn: &Connection, meeting_id: &str) -> rusqlite::Result<Vec<Segment>> {
    let mut statement = conn.prepare(
        "SELECT id, meeting_id, start_ms, end_ms, speaker, text, clean_text, clean_text_manual FROM segments WHERE meeting_id=?1 ORDER BY start_ms, id",
    )?;
    let rows = statement.query_map(params![meeting_id], segment_from_row)?;
    rows.collect()
}

impl Store {
    /// Replace all segments of a meeting (used after each transcription pass).
    pub fn replace_segments(&self, meeting_id: &str, segs: &[NewSegment]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        if crate::transcript_versions::has_combined_publication_on(&tx, meeting_id)? {
            return Err(crate::StoreError::Other("A combined transcript is already published. Create a new transcript version instead of replacing its raw text.".into()));
        }
        tx.execute("DELETE FROM segments WHERE meeting_id=?1", params![meeting_id])?;
        {
            let mut st = tx.prepare(
                "INSERT INTO segments(meeting_id, start_ms, end_ms, speaker, text) VALUES (?1,?2,?3,?4,?5)",
            )?;
            for s in segs {
                st.execute(params![meeting_id, s.start_ms, s.end_ms, s.speaker, s.text])?;
            }
        }
        crate::settings::clear_meeting_voice_queries_on(&tx, meeting_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Replace the speaker label of individual segments.
    pub fn set_segment_speakers(&self, meeting_id: &str, updates: &[(i64, Option<String>)]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare("UPDATE segments SET speaker = ?3 WHERE meeting_id = ?1 AND id = ?2")?;
            for (id, sp) in updates {
                st.execute(params![meeting_id, id, sp])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn segments(&self, meeting_id: &str) -> Result<Vec<Segment>> {
        let conn = self.conn.lock();
        Ok(segments_on(&conn, meeting_id)?)
    }

    /// Store/reset one manual cleaned-text overlay and lexical indexes atomically.
    /// Raw text, provider captions, timings, speaker labels and IDs stay untouched.
    pub fn set_transcript_correction(&self, meeting_id: &str, segment_id: i64, text: Option<&str>) -> std::result::Result<Segment, TranscriptCorrectionError> {
        validate_correction(text)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let status: Option<String> = tx.query_row("SELECT status FROM meetings WHERE id=?1", params![meeting_id], |row| row.get(0)).optional()?;
        match status.as_deref() {
            None => return Err(TranscriptCorrectionError::MeetingNotFound),
            Some("ready") => {},
            _ => return Err(TranscriptCorrectionError::NotReady),
        }
        if tx.execute("UPDATE segments SET clean_text=?3, clean_text_manual=?4 WHERE meeting_id=?1 AND id=?2",
            params![meeting_id, segment_id, text, text.is_some()])? == 0 {
            return Err(TranscriptCorrectionError::SegmentNotFound);
        }
        crate::ai::rebuild_chunks_on(&tx, meeting_id)?;
        let corrected = tx.query_row(
            "SELECT id, meeting_id, start_ms, end_ms, speaker, text, clean_text, clean_text_manual FROM segments WHERE meeting_id=?1 AND id=?2",
            params![meeting_id, segment_id], segment_from_row,
        )?;
        tx.commit()?;
        Ok(corrected)
    }

    /// Full-text search over transcript segments (raw and clean text) and meeting titles.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT m.id, m.title, s.id, s.start_ms,
                    snippet(segments_fts, 0, '<b>', '</b>', '...', 12)
             FROM segments_fts
             JOIN segments s ON s.id = segments_fts.rowid
             JOIN meetings m ON m.id = s.meeting_id
             WHERE segments_fts MATCH ?1
             ORDER BY bm25(segments_fts) LIMIT ?2",
        )?;
        let mut hits: Vec<SearchHit> = st
            .query_map(params![q, limit as i64], |r| {
                Ok(SearchHit {
                    meeting_id: r.get(0)?,
                    meeting_title: r.get(1)?,
                    segment_id: r.get(2)?,
                    start_ms: r.get(3)?,
                    snippet: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Title matches (no segment).
        let mut st = conn.prepare("SELECT id, title FROM meetings WHERE title LIKE ?1 LIMIT ?2")?;
        let like = format!("%{}%", query.trim());
        let titles = st
            .query_map(params![like, limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, title) in titles {
            if !hits.iter().any(|h| h.meeting_id == id) {
                hits.push(SearchHit { meeting_id: id, meeting_title: title.clone(), segment_id: 0, start_ms: 0, snippet: title });
            }
        }
        hits.truncate(limit);
        Ok(hits)
    }
}

/// Retrieval flavour: words OR-ed (BM25 ranks the best chunk), stop-words dropped.
pub(crate) fn fts_query_or(input: &str) -> String {
    const STOP: &[&str] = &["the", "a", "an", "and", "or", "of", "to", "in", "on", "is", "was", "what", "did", "we", "about", "for", "it", "that", "this", "with", "be", "are", "were", "do", "does", "how", "who", "when", "where", "why", "say", "said", "i", "you", "they", "he", "she", "at", "by", "from", "as", "our", "my", "me", "us"];
    input
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| w.len() > 1 && !STOP.contains(&w.as_str()))
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Turn free text into a safe FTS5 query: each word quoted, prefix-matched, AND-ed.
fn fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Meeting;

    fn ready_fixture() -> (Store, Meeting, Vec<Segment>) {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("correction fixture", None);
        meeting.status = crate::MeetingStatus::Ready;
        meeting.duration_ms = 2000;
        store.create_meeting(&meeting).unwrap();
        store.replace_segments(&meeting.id, &[
            NewSegment { start_ms:0,end_ms:1000,speaker:Some("Sam".into()),text:"raw telescope".into() },
            NewSegment { start_ms:1000,end_ms:2000,speaker:Some("Sam".into()),text:"second sentence".into() },
        ]).unwrap();
        store.rebuild_chunks(&meeting.id).unwrap();
        let segments = store.segments(&meeting.id).unwrap();
        (store, meeting, segments)
    }

    #[test]
    fn manual_correction_and_reset_keep_raw_provenance_and_refresh_search() {
        let (store, meeting, segments) = ready_fixture();
        let provenance = serde_json::json!({"provider":"soniox","captions":[{"text":"raw telescope","start_ms":0,"end_ms":1000,"is_final":true}]});
        let key = format!("stt:{}", meeting.id);
        store.set_setting(&key, &provenance).unwrap();
        let voice_key = format!("speaker_embeddings:current-policy:{}", meeting.id);
        store.set_setting(&voice_key, &vec![1.0_f32, 0.0]).unwrap();
        let corrected = store.set_transcript_correction(&meeting.id, segments[0].id, Some("corrected comet")).unwrap();
        assert_eq!((&corrected.id,&corrected.meeting_id,corrected.start_ms,corrected.end_ms,&corrected.speaker,&corrected.text),
            (&segments[0].id,&segments[0].meeting_id,segments[0].start_ms,segments[0].end_ms,&segments[0].speaker,&segments[0].text));
        assert_eq!(corrected.clean_text.as_deref(), Some("corrected comet")); assert!(corrected.clean_text_manual);
        assert_eq!(store.get_setting::<serde_json::Value>(&key).unwrap(), Some(provenance.clone()));
        assert_eq!(store.search("comet", 5).unwrap().len(), 1);
        assert_eq!(store.search_chunks("comet", Some(&meeting.id), 5).unwrap().len(), 1);
        assert!(store.search_chunks("telescope", Some(&meeting.id), 5).unwrap().is_empty());
        let reset = store.set_transcript_correction(&meeting.id, segments[0].id, None).unwrap();
        assert_eq!(reset, segments[0]);
        assert!(store.search("comet", 5).unwrap().is_empty());
        assert_eq!(store.search_chunks("telescope", Some(&meeting.id), 5).unwrap().len(), 1);
        assert_eq!(store.get_setting::<serde_json::Value>(&key).unwrap(), Some(provenance));
        assert_eq!(store.get_setting::<Vec<f32>>(&voice_key).unwrap(), Some(vec![1.0, 0.0]));
    }

    #[test]
    fn replace_segments_clears_only_meeting_voice_queries_and_keeps_provider_history() {
        let (store, meeting, segments) = ready_fixture();
        store.set_transcript_correction(&meeting.id, segments[0].id, Some("Old cleaned overlay")).unwrap();
        let voice_keys = [
            format!("speaker_embeddings:{}", meeting.id),
            format!("speaker_embeddings:old-policy:{}", meeting.id),
            format!("speaker_embeddings:future-policy:{}", meeting.id),
        ];
        let retained_keys = [
            format!("stt:{}", meeting.id),
            format!("stt_previous:{}", meeting.id),
            format!("highlights:{}", meeting.id),
            format!("speaker_embeddings:old-policy:other-{}", meeting.id),
        ];
        for key in voice_keys.iter().chain(&retained_keys) {
            store.set_setting(key, &"synthetic preserved value").unwrap();
        }
        store.replace_segments(&meeting.id, &[NewSegment {
            start_ms: 0, end_ms: 1000, speaker: Some("New speaker".into()), text: "New raw transcript".into(),
        }]).unwrap();
        let rows = store.segments(&meeting.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "New raw transcript");
        assert_eq!(rows[0].clean_text, None);
        assert!(!rows[0].clean_text_manual);
        for key in &voice_keys {
            assert!(store.get_setting::<String>(key).unwrap().is_none());
        }
        for key in &retained_keys {
            assert_eq!(store.get_setting::<String>(key).unwrap().as_deref(), Some("synthetic preserved value"));
        }
    }

    #[test]
    fn replace_segments_rolls_back_raw_clean_and_cache_on_cleanup_failure() {
        let (store, meeting, segments) = ready_fixture();
        store.set_transcript_correction(&meeting.id, segments[0].id, Some("")).unwrap();
        let before = store.segments(&meeting.id).unwrap();
        let cache_key = format!("speaker_embeddings:policy:{}", meeting.id);
        store.set_setting(&cache_key, &vec![1.0_f32, 0.0]).unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER fail_voice_cleanup BEFORE DELETE ON settings BEGIN SELECT RAISE(ABORT,'injected cache cleanup failure'); END;").unwrap();
        assert!(store.replace_segments(&meeting.id, &[NewSegment {
            start_ms: 0, end_ms: 1000, speaker: None, text: "Uncommitted raw".into(),
        }]).is_err());
        assert_eq!(store.segments(&meeting.id).unwrap(), before);
        assert_eq!(store.get_setting::<Vec<f32>>(&cache_key).unwrap(), Some(vec![1.0, 0.0]));
        assert!(store.search("Uncommitted", 5).unwrap().is_empty());
        assert_eq!(store.search("telescope", 5).unwrap().len(), 1);
    }

    #[test]
    fn correction_is_ready_only_and_scoped_to_the_owning_meeting() {
        let (store, mut meeting, segments) = ready_fixture();
        let mut other = Meeting::new_recording("other", None); other.status = crate::MeetingStatus::Ready;
        store.create_meeting(&other).unwrap();
        assert!(matches!(store.set_transcript_correction(&other.id, segments[0].id, Some("foreign")), Err(TranscriptCorrectionError::SegmentNotFound)));
        assert!(matches!(store.set_transcript_correction("missing", segments[0].id, Some("missing")), Err(TranscriptCorrectionError::MeetingNotFound)));
        meeting.status = crate::MeetingStatus::Processing; store.update_meeting(&meeting).unwrap();
        assert!(matches!(store.set_transcript_correction(&meeting.id, segments[0].id, Some("busy")), Err(TranscriptCorrectionError::NotReady)));
        assert_eq!(store.segments(&meeting.id).unwrap(), segments);
    }

    #[test]
    fn manual_text_preserves_empty_whitespace_and_utf8_boundaries() {
        let (store, meeting, segments) = ready_fixture();
        for text in ["", " \n\t "] {
            let saved = store.set_transcript_correction(&meeting.id, segments[0].id, Some(text)).unwrap();
            assert_eq!(saved.clean_text.as_deref(), Some(text)); assert!(saved.clean_text_manual);
        }
        let boundary = "я".repeat(MAX_TRANSCRIPT_CORRECTION_BYTES / 2);
        let saved = store.set_transcript_correction(&meeting.id, segments[0].id, Some(&boundary)).unwrap();
        assert_eq!(saved.clean_text.as_deref(), Some(boundary.as_str()));
        assert!(matches!(store.set_transcript_correction(&meeting.id, segments[0].id, Some(&(boundary + "я"))), Err(TranscriptCorrectionError::TooLarge)));
        assert!(matches!(store.set_transcript_correction(&meeting.id, segments[0].id, Some("safe\0hidden")), Err(TranscriptCorrectionError::Invalid)));
        assert_eq!(store.segments(&meeting.id).unwrap()[0], saved);
    }

    #[test]
    fn cleanup_and_speaker_rename_preserve_manual_text_including_blank() {
        let (store, meeting, segments) = ready_fixture();
        let cache_key = format!("speaker_embeddings:policy:{}", meeting.id);
        store.set_setting(&cache_key, &vec![1.0_f32, 0.0]).unwrap();
        store.set_transcript_correction(&meeting.id, segments[0].id, Some("")).unwrap();
        store.set_clean_text(&meeting.id, &[(segments[0].id,"must not overwrite".into()),(segments[1].id,"AI cleanup".into())]).unwrap();
        let saved = store.segments(&meeting.id).unwrap();
        assert_eq!(saved[0].clean_text.as_deref(), Some("")); assert!(saved[0].clean_text_manual);
        assert_eq!(saved[1].clean_text.as_deref(), Some("AI cleanup")); assert!(!saved[1].clean_text_manual);
        assert_eq!(store.search_chunks("cleanup", Some(&meeting.id), 5).unwrap().len(), 1);
        store.clear_clean_text(&meeting.id).unwrap();
        assert_eq!(store.get_setting::<Vec<f32>>(&cache_key).unwrap(), Some(vec![1.0, 0.0]));
        store.rename_speaker(&meeting.id, "Sam", "Alice", None).unwrap();
        let after = store.segments(&meeting.id).unwrap();
        assert_eq!(after[0].clean_text.as_deref(), Some("")); assert!(after[0].clean_text_manual);
        assert_eq!(after[1].clean_text, None); assert_eq!(after[0].text, segments[0].text);
        assert_eq!(after[0].speaker.as_deref(), Some("Alice"));
    }

    #[test]
    fn correction_rolls_back_text_fts_chunks_and_vectors_on_index_failure() {
        let (store, meeting, segments) = ready_fixture();
        let todo = store.chunks_without_vectors(&meeting.id).unwrap();
        store.set_chunk_vectors(&[(todo[0].0,vec![1.0,0.0])]).unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER correction_fixture_failure BEFORE INSERT ON chunks BEGIN SELECT RAISE(ABORT,'fixture index failure'); END;").unwrap();
        assert!(matches!(store.set_transcript_correction(&meeting.id, segments[0].id, Some("uncommitted comet")), Err(TranscriptCorrectionError::Sqlite(_))));
        assert_eq!(store.segments(&meeting.id).unwrap(), segments);
        assert!(store.search("comet", 5).unwrap().is_empty());
        assert!(store.search_chunks("comet", Some(&meeting.id), 5).unwrap().is_empty());
        assert_eq!(store.search_chunks("telescope", Some(&meeting.id), 5).unwrap().len(), 1);
        assert!(store.has_chunk_vectors(&meeting.id).unwrap());
    }

    #[test]
    fn segment_without_manual_field_defaults_to_nonmanual_overlay() {
        let (_, _, segments) = ready_fixture();
        let mut value = serde_json::to_value(&segments[0]).unwrap();
        value.as_object_mut().unwrap().remove("clean_text_manual");
        let decoded: Segment = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, segments[0]);
        let mut invalid = decoded;
        invalid.clean_text_manual = true;
        assert!(matches!(invalid.validate_manual_correction(), Err(ManualCorrectionError::Invalid)));
    }

    #[test]
    fn segments_roundtrip_and_fts() {
        let s = Store::open_in_memory().unwrap();
        let m = Meeting::new_recording("Budget review", None);
        s.create_meeting(&m).unwrap();
        s.replace_segments(
            &m.id,
            &[NewSegment { start_ms: 0, end_ms: 1500, speaker: None, text: "we need to cut the marketing budget".into() }],
        )
        .unwrap();
        assert_eq!(s.segments(&m.id).unwrap().len(), 1);
        let hits = s.search("marketing", 10).unwrap();
        assert_eq!(hits[0].meeting_id, m.id);
        assert!(hits[0].snippet.contains("<b>marketing</b>"));
        assert!(s.search("zebra", 10).unwrap().is_empty());
    }

    #[test]
    fn title_search_and_replace_clears_old() {
        let s = Store::open_in_memory().unwrap();
        let m = Meeting::new_recording("Quarterly planning", None);
        s.create_meeting(&m).unwrap();
        assert_eq!(s.search("quarterly", 5).unwrap().len(), 1);
        s.replace_segments(&m.id, &[NewSegment { start_ms: 0, end_ms: 1, speaker: None, text: "alpha".into() }]).unwrap();
        s.replace_segments(&m.id, &[NewSegment { start_ms: 0, end_ms: 1, speaker: None, text: "beta".into() }]).unwrap();
        assert!(s.search("alpha", 5).unwrap().is_empty());
        assert_eq!(s.search("beta", 5).unwrap().len(), 1);
    }

    #[test]
    fn fts_query_or_drops_stopwords() {
        assert_eq!(fts_query_or("What did we say about the budget?"), "\"budget\"*");
        assert_eq!(fts_query_or("the a of"), "");
    }

    #[test]
    fn fts_query_quotes_and_prefixes() {
        assert_eq!(fts_query("hello wor\"ld"), "\"hello\"* \"world\"*");
        assert_eq!(fts_query("   "), "");
    }
}

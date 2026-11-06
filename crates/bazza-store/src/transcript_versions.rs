//! Durable recognition versions. Complete combined results may atomically
//! become the canonical projection while the initial transcript is archived.
use crate::service_data::{DataError, DataResult};
use crate::{MeetingSpeaker, NewSegment, Result, Segment, Store, StoreError};
use bazza_domain::{
    local_asr::{model_id_valid, MAX_LOCAL_ASR_BODY_BYTES},
    transcript_versions::{
        manual_transcript_version_id_valid, valid_transcript_timing_text, NewTranscriptVersion,
        TranscriptVersion, TranscriptVersionSegment, TranscriptVersionSource, TranscriptVersions,
        INITIAL_TRANSCRIPT_VERSION_ID,
    },
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_MANUAL_VERSIONS: u32 = 128;

#[path = "transcript_versions_archive.rs"]
mod archive;
pub(crate) use archive::import_archive_on;

fn info_prefix(meeting_id: &str) -> String {
    format!("transcript-version-info:{}:{meeting_id}:", meeting_id.len())
}

fn data_prefix(meeting_id: &str) -> String {
    format!("transcript-version-data:{}:{meeting_id}:", meeting_id.len())
}

fn publication_key(meeting_id: &str) -> String {
    format!("transcript-version-publication:{}:{meeting_id}", meeting_id.len())
}

pub(crate) fn has_combined_publication_on(conn: &Connection, meeting_id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key=?1)",[publication_key(meeting_id)],|row|row.get(0))
}

#[derive(Serialize, Deserialize)]
struct Publication {
    effective_id: String,
    original: Vec<TranscriptVersionSegment>,
    canonical_ids: Vec<i64>,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
}

pub(crate) fn bind_combined_job_on(tx: &Transaction<'_>, meeting_id: &str, job_id: &str, source_id: &str) -> DataResult<()> {
    let key = publication_key(meeting_id);
    let mut state: Publication = setting(tx, &key)?.ok_or(DataError::Invalid("missing combined publication"))?;
    if state.job_id.is_some() || state.source_id.is_some() {
        return Err(DataError::Forbidden);
    }
    state.job_id = Some(job_id.into());
    state.source_id = Some(source_id.into());
    tx.execute("UPDATE settings SET value=?2 WHERE key=?1",params![key,serde_json::to_string(&state)?])?;
    Ok(())
}

/// A desktop publication registers one complete source with its original
/// captured job; arbitrary transcript replacement cannot acquire worker rights.
pub(crate) fn registered_combined_on(conn: &Connection, meeting_id: &str, job_id: &str, source_id: &str) -> DataResult<bool> {
    let Some(state) = setting::<Publication>(conn, &publication_key(meeting_id))? else { return Ok(false); };
    if state.job_id.as_deref() != Some(job_id) || state.source_id.as_deref() != Some(source_id) {
        return Ok(false);
    }
    let input: Option<NewTranscriptVersion> = setting(conn, &format!("{}{}",data_prefix(meeting_id),state.effective_id))?;
    let source_matches: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM service_transcript_sources WHERE id=?1 AND meeting_id=?2 AND origin='combined' AND source_label=?3)",
        params![source_id,meeting_id,state.effective_id], |row| row.get(0))?;
    Ok(source_matches && input.is_some_and(|input| input.source == TranscriptVersionSource::Combined && !input.partial && input.valid())
        && effective_id_on(conn, meeting_id)?.as_deref() == Some(state.effective_id.as_str()))
}

pub(crate) fn archived_original_on(
    conn: &Connection,
    meeting_id: &str,
) -> DataResult<Option<Vec<TranscriptVersionSegment>>> {
    Ok(setting::<Publication>(conn, &publication_key(meeting_id))?.map(|state| state.original))
}

pub(crate) fn effective_id_on(conn: &Connection, meeting_id: &str) -> DataResult<Option<String>> {
    let Some(publication) = setting::<Publication>(conn, &publication_key(meeting_id))? else {
        return Ok(None);
    };
    let current = crate::segments::segments_on(conn, meeting_id)?;
    let Some(version) = setting::<NewTranscriptVersion>(conn, &format!("{}{}", data_prefix(meeting_id), publication.effective_id))? else {
        return Ok(None);
    };
    let mut expected: Vec<_> = version.segments.iter().collect();
    expected.sort_by_key(|row| (row.segment.start_ms, row.segment.end_ms));
    Ok((current.len() == publication.canonical_ids.len()
        && current.len() == expected.len()
        && current.iter().zip(&publication.canonical_ids).zip(expected).all(|((row, id), expected)| {
            row.id == *id && row.start_ms == expected.segment.start_ms
                && row.end_ms == expected.segment.end_ms && row.text == expected.segment.text
        }))
        .then_some(publication.effective_id))
}

pub(crate) fn effective_rows_on(
    conn: &Connection,
    meeting_id: &str,
    version_id: &str,
) -> DataResult<Option<Vec<TranscriptVersionSegment>>> {
    if effective_id_on(conn, meeting_id)?.as_deref() != Some(version_id) {
        return Ok(None);
    }
    let input: NewTranscriptVersion = setting(conn, &format!("{}{version_id}", data_prefix(meeting_id)))?
        .ok_or(DataError::Invalid("missing published transcript"))?;
    let mut inputs = input.segments;
    inputs.sort_by_key(|row| (row.segment.start_ms, row.segment.end_ms));
    Ok(Some(crate::segments::segments_on(conn, meeting_id)?.into_iter().zip(inputs).map(|(segment, input)| {
        TranscriptVersionSegment { segment, playback: input.playback, provenance: input.provenance }
    }).collect()))
}

/// Save the immutable version first. Promotion is allowed once, before any
/// manual text or speaker changes, and shares its caller's transaction.
pub(crate) fn promote_combined_on(
    tx: &Transaction<'_>,
    meeting_id: &str,
    input: &NewTranscriptVersion,
    original_info: Option<TranscriptVersion>,
    protect_speakers: bool,
) -> DataResult<bool> {
    if input.source != TranscriptVersionSource::Combined || input.partial
        || setting::<Publication>(tx, &publication_key(meeting_id))?.is_some()
    {
        return Ok(false);
    }
    let rows = crate::segments::segments_on(tx, meeting_id)?;
    if protect_speakers || rows.iter().any(|row| row.clean_text_manual) {
        return Ok(false);
    }
    let original: Vec<_> = rows.iter().cloned().map(Into::into).collect();
    if let Some(info) = original_info {
        tx.execute("INSERT OR IGNORE INTO settings(key,value) VALUES(?1,?2)",
            params![format!("{}{}", info_prefix(meeting_id), INITIAL_TRANSCRIPT_VERSION_ID), serde_json::to_string(&info)?])?;
    }
    // Insert first so IDs still referenced by the archived transcript are
    // never immediately recycled for different words.
    let old_last = rows.iter().map(|row| row.id).max();
    let mut inputs: Vec<_> = input.segments.iter().collect();
    inputs.sort_by_key(|row| (row.segment.start_ms, row.segment.end_ms));
    let mut ids = Vec::with_capacity(inputs.len());
    {
        let mut insert = tx.prepare("INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text) VALUES(?1,?2,?3,?4,?5)")?;
        for row in inputs {
            let row = &row.segment;
            insert.execute(params![meeting_id,row.start_ms,row.end_ms,row.speaker,row.text])?;
            ids.push(tx.last_insert_rowid());
        }
    }
    if let Some(last) = old_last {
        tx.execute("DELETE FROM segments WHERE meeting_id=?1 AND id<=?2", params![meeting_id,last])?;
    }
    tx.execute("DELETE FROM meeting_speakers WHERE meeting_id=?1", [meeting_id])?;
    tx.execute("INSERT INTO meeting_speakers(meeting_id,label) SELECT DISTINCT meeting_id,speaker FROM segments WHERE meeting_id=?1 AND speaker IS NOT NULL", [meeting_id])?;
    tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2)", params![publication_key(meeting_id),serde_json::to_string(&Publication {
        effective_id: input.id.clone(), original, canonical_ids: ids, job_id: None, source_id: None,
    })?])?;
    crate::settings::clear_meeting_voice_queries_on(tx, meeting_id)?;
    crate::ai::rebuild_chunks_on(tx, meeting_id)?;
    Ok(true)
}

fn setting<T: serde::de::DeserializeOwned>(conn: &Connection, key: &str) -> DataResult<Option<T>> {
    let value: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(value.as_deref().map(serde_json::from_str).transpose()?)
}

pub(crate) fn initial_info_on(
    conn: &Connection,
    meeting_id: &str,
) -> DataResult<Option<TranscriptVersion>> {
    setting(
        conn,
        &format!("{}{INITIAL_TRANSCRIPT_VERSION_ID}", info_prefix(meeting_id)),
    )
}

pub(crate) fn listing_on(
    conn: &Connection,
    meeting_id: &str,
    original: Option<TranscriptVersion>,
) -> DataResult<TranscriptVersions> {
    let prefix = info_prefix(meeting_id);
    let original_key = format!("{prefix}{INITIAL_TRANSCRIPT_VERSION_ID}");
    let mut statement = conn.prepare(
        "SELECT value FROM settings WHERE substr(key,1,length(?1))=?1 AND key<>?2 LIMIT ?3",
    )?;
    let encoded = statement
        .query_map(
            params![prefix, original_key, MAX_MANUAL_VERSIONS + 1],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if encoded.len() > MAX_MANUAL_VERSIONS as usize {
        return Err(DataError::Invalid("transcript version count"));
    }
    let mut versions = encoded
        .iter()
        .map(|value| serde_json::from_str::<TranscriptVersion>(value))
        .collect::<serde_json::Result<Vec<_>>>()?;
    versions.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let initial_id = original.as_ref().map(|version| version.id.clone());
    if let Some(original) = original {
        versions.insert(0, original);
    }
    Ok(TranscriptVersions {
        effective_id: effective_id_on(conn, meeting_id)?.or_else(|| initial_id.clone()),
        initial_id,
        versions,
        participant_source: None,
    })
}

pub(crate) fn manual_rows_on(
    conn: &Connection,
    meeting_id: &str,
    version_id: &str,
) -> DataResult<Option<Vec<TranscriptVersionSegment>>> {
    if !manual_transcript_version_id_valid(version_id) {
        return Err(DataError::Invalid("transcript version ID"));
    }
    let Some(version) =
        setting::<NewTranscriptVersion>(conn, &format!("{}{version_id}", data_prefix(meeting_id)))?
    else {
        return Ok(None);
    };
    if version.id != version_id || !version.valid() {
        return Err(DataError::Invalid("stored transcript version"));
    }
    let mut segments: Vec<_> = version
        .segments
        .into_iter()
        .enumerate()
        .map(|(index, row)| TranscriptVersionSegment {
            segment: Segment {
                // These view-only rows must never target a canonical correction by ID.
                id: -(index as i64 + 1),
                meeting_id: meeting_id.into(),
                start_ms: row.segment.start_ms,
                end_ms: row.segment.end_ms,
                speaker: row.segment.speaker,
                text: row.segment.text,
                clean_text: None,
                clean_text_manual: false,
            },
            playback: row.playback,
            provenance: row.provenance,
        })
        .collect();
    segments.sort_by_key(|row| {
        let start = row.playback.map_or(row.segment.start_ms, |timing| timing.start_ms);
        (start, -row.segment.id)
    });
    Ok(Some(segments))
}

pub(crate) fn save_manual_on(
    tx: &Transaction<'_>,
    meeting_id: &str,
    input: &NewTranscriptVersion,
) -> DataResult<TranscriptVersion> {
    if !input.valid() {
        return Err(DataError::Invalid("transcript version"));
    }
    let body = serde_json::to_string(input)?;
    if body.len() as u64 > MAX_LOCAL_ASR_BODY_BYTES {
        return Err(DataError::Invalid("transcript version byte budget"));
    }
    let prefix = data_prefix(meeting_id);
    let data_key = format!("{prefix}{}", input.id);
    let info_key = format!("{}{}", info_prefix(meeting_id), input.id);
    if let Some(existing) = setting::<NewTranscriptVersion>(tx, &data_key)? {
        if &existing != input {
            return Err(DataError::IdempotencyConflict);
        }
        return setting(tx, &info_key)?
            .ok_or(DataError::Invalid("missing transcript version metadata"));
    }
    let count: u32 = tx.query_row(
        "SELECT count(*) FROM settings WHERE substr(key,1,length(?1))=?1",
        [&prefix],
        |row| row.get(0),
    )?;
    if count >= MAX_MANUAL_VERSIONS {
        return Err(DataError::Invalid("transcript version limit"));
    }
    let info = TranscriptVersion {
        id: input.id.clone(),
        source: input.source,
        model: Some(input.model.clone()),
        language: input.language.clone(),
        created_at: Utc::now(),
        partial: input.partial,
    };
    tx.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2)",
        params![data_key, body],
    )?;
    tx.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2)",
        params![info_key, serde_json::to_string(&info)?],
    )?;
    Ok(info)
}

pub(crate) fn delete_versions_on(tx: &Transaction<'_>, meeting_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM settings WHERE substr(key,1,length(?1))=?1 OR substr(key,1,length(?2))=?2 OR key=?3",
        params![info_prefix(meeting_id), data_prefix(meeting_id), publication_key(meeting_id)],
    )?;
    Ok(())
}

fn has_original(conn: &Connection, meeting_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM segments WHERE meeting_id=?1)",
        [meeting_id],
        |row| row.get(0),
    )
}

#[derive(Deserialize)]
struct SttMetadata {
    model: Option<String>,
    language: Option<String>,
    provider: Option<String>,
}

fn original_info(
    conn: &Connection,
    meeting_id: &str,
    created_at: DateTime<Utc>,
) -> DataResult<Option<TranscriptVersion>> {
    if let Some(archived) = archived_original_on(conn, meeting_id)? {
        return if archived.is_empty() { Ok(None) } else { initial_info_on(conn, meeting_id) };
    }
    if !has_original(conn, meeting_id)? {
        return Ok(None);
    }
    if let Some(info) = initial_info_on(conn, meeting_id)? {
        return Ok(Some(info));
    }
    // Malformed optional historical provenance must not hide the original text.
    let provenance = match setting::<SttMetadata>(conn, &format!("stt:{meeting_id}")) {
        Ok(provenance) => provenance,
        Err(DataError::Json(_)) => None,
        Err(error) => return Err(error),
    };
    let cloud_live = provenance
        .as_ref()
        .and_then(|value| value.provider.as_deref())
        == Some("soniox");
    Ok(Some(TranscriptVersion {
        id: INITIAL_TRANSCRIPT_VERSION_ID.into(),
        source: if cloud_live {
            TranscriptVersionSource::Live
        } else {
            TranscriptVersionSource::Existing
        },
        model: provenance.as_ref().and_then(|value| value.model.clone()),
        language: provenance.and_then(|value| value.language),
        created_at,
        partial: false,
    }))
}

fn legacy_error(error: DataError) -> StoreError {
    match error {
        DataError::Sqlite(error) => StoreError::Sqlite(error),
        DataError::Json(error) => StoreError::Json(error),
        error => StoreError::Other(error.to_string()),
    }
}

impl Store {
    /// Enrich the published live rows without changing their identity, text or timing.
    pub fn apply_initial_live_speakers(
        &self,
        meeting_id: &str,
        labels: &[(i64, Option<String>)],
        speakers: &[MeetingSpeaker],
        query_key: &str,
        queries: &BTreeMap<String, Vec<f32>>,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let original = crate::segments::segments_on(&tx, meeting_id)?;
        if !initial_info_on(&tx, meeting_id).map_err(legacy_error)?.is_some_and(|info| info.source == TranscriptVersionSource::Live)
            || original.len() != labels.len()
            || original.iter().zip(labels).any(|(row, (id, _))| row.id != *id || row.speaker.is_some())
        {
            return Err(legacy_error(DataError::Busy));
        }
        {
            let mut update = tx.prepare("UPDATE segments SET speaker=?3 WHERE meeting_id=?1 AND id=?2")?;
            for (id, label) in labels {
                update.execute(params![meeting_id, id, label])?;
            }
        }
        crate::speakers::replace_meeting_speakers_on(&tx, meeting_id, speakers)?;
        crate::settings::clear_meeting_voice_queries_on(&tx, meeting_id)?;
        tx.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![query_key, serde_json::to_string(queries)?],
        )?;
        crate::ai::rebuild_chunks_on(&tx, meeting_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn has_published_initial_live(&self, meeting_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        Ok(initial_info_on(&conn, meeting_id)
            .map_err(legacy_error)?
            .is_some_and(|info| info.source == TranscriptVersionSource::Live)
            && has_original(&conn, meeting_id)?)
    }

    pub fn transcript_versions(&self, meeting_id: &str) -> Result<TranscriptVersions> {
        let conn = self.conn.lock();
        let created_at = conn
            .query_row(
                "SELECT created_at FROM meetings WHERE id=?1",
                [meeting_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| legacy_error(DataError::NotFound))?;
        let original = original_info(&conn, meeting_id, created_at).map_err(legacy_error)?;
        listing_on(&conn, meeting_id, original).map_err(legacy_error)
    }

    pub fn transcript_version(
        &self,
        meeting_id: &str,
        version_id: &str,
    ) -> Result<Option<Vec<TranscriptVersionSegment>>> {
        let conn = self.conn.lock();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?1)",
            [meeting_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        if version_id == INITIAL_TRANSCRIPT_VERSION_ID {
            if let Some(archived) = archived_original_on(&conn, meeting_id).map_err(legacy_error)? {
                return Ok((!archived.is_empty()).then_some(archived));
            }
            let rows = crate::segments::segments_on(&conn, meeting_id)?;
            return Ok((!rows.is_empty()).then(|| rows.into_iter().map(Into::into).collect()));
        }
        if let Some(rows) = effective_rows_on(&conn, meeting_id, version_id).map_err(legacy_error)? {
            return Ok(Some(rows));
        }
        manual_rows_on(&conn, meeting_id, version_id).map_err(legacy_error)
    }

    pub fn save_transcript_version(
        &self,
        meeting_id: &str,
        input: &NewTranscriptVersion,
    ) -> Result<TranscriptVersion> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (status, created_at): (String, DateTime<Utc>) = tx
            .query_row(
                "SELECT status,created_at FROM meetings WHERE id=?1",
                [meeting_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| legacy_error(DataError::NotFound))?;
        let has_original = has_original(&tx, meeting_id)?;
        if !matches!(status.as_str(), "ready" | "failed")
            && !(status == "processing" && (has_original || input.source == TranscriptVersionSource::Combined))
        {
            return Err(legacy_error(DataError::Busy));
        }
        let version = save_manual_on(&tx, meeting_id, input).map_err(legacy_error)?;
        let original = original_info(&tx, meeting_id, created_at).map_err(legacy_error)?;
        let protect_speakers: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM meeting_speakers WHERE meeting_id=?1 AND speaker_id IS NOT NULL)", [meeting_id], |row| row.get(0))?;
        promote_combined_on(&tx, meeting_id, input, original, protect_speakers).map_err(legacy_error)?;
        tx.commit()?;
        Ok(version)
    }

    /// Publish the initial local live transcript once, before background stages.
    /// A retry never changes segment identities, raw text or corrections.
    pub fn publish_initial_live_transcript(
        &self,
        meeting_id: &str,
        model: &str,
        language: Option<&str>,
        segments: &[NewSegment],
    ) -> Result<()> {
        if segments.is_empty() {
            return Ok(());
        }
        if !model_id_valid(model) || !segments.iter().all(valid_transcript_timing_text) {
            return Err(legacy_error(DataError::Invalid("initial live transcript")));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let status: String = tx
            .query_row(
                "SELECT status FROM meetings WHERE id=?1",
                [meeting_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| legacy_error(DataError::NotFound))?;
        if has_original(&tx, meeting_id)?
            || initial_info_on(&tx, meeting_id)
                .map_err(legacy_error)?
                .is_some()
        {
            return Ok(());
        }
        if !matches!(status.as_str(), "recording" | "processing") {
            return Err(legacy_error(DataError::Busy));
        }
        {
            let mut insert = tx.prepare("INSERT INTO segments(meeting_id,start_ms,end_ms,speaker,text) VALUES(?1,?2,?3,?4,?5)")?;
            for segment in segments {
                insert.execute(params![
                    meeting_id,
                    segment.start_ms,
                    segment.end_ms,
                    segment.speaker,
                    segment.text
                ])?;
            }
        }
        let info = TranscriptVersion {
            id: INITIAL_TRANSCRIPT_VERSION_ID.into(),
            source: TranscriptVersionSource::Live,
            model: Some(model.into()),
            language: language.map(str::to_owned),
            created_at: Utc::now(),
            partial: false,
        };
        let key = format!("{}{}", info_prefix(meeting_id), info.id);
        tx.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)",
            params![key, serde_json::to_string(&info)?],
        )?;
        crate::settings::clear_meeting_voice_queries_on(&tx, meeting_id)?;
        crate::ai::rebuild_chunks_on(&tx, meeting_id)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Meeting, MeetingStatus};

    fn segment(start_ms: i64, text: &str, speaker: &str) -> NewSegment {
        NewSegment {
            start_ms,
            end_ms: start_ms + 100,
            text: text.into(),
            speaker: Some(speaker.into()),
        }
    }

    fn combined(text: &str, partial: bool) -> NewTranscriptVersion {
        use bazza_domain::transcript_versions::{TranscriptSegmentProvenance, TranscriptSegmentSource};
        let mut row: bazza_domain::transcript_versions::NewTranscriptVersionSegment = segment(500, text, "System").into();
        row.provenance = Some(TranscriptSegmentProvenance {
            source: TranscriptSegmentSource::System, provider: None, participant_id: None,
            source_start_ms: 500, source_end_ms: 600, uncertain: partial,
        });
        NewTranscriptVersion { id: uuid::Uuid::new_v4().to_string(), source: TranscriptVersionSource::Combined,
            model: "whisper-small".into(), language: None, partial, segments: vec![row] }
    }

    #[test]
    fn combined_publication_archives_live_and_updates_search_once_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("combined.sqlite");
        let store = Store::open_legacy(&path).unwrap();
        let mut meeting = Meeting::new_recording("Combined fixture", None);
        store.create_meeting(&meeting).unwrap();
        store.publish_initial_live_transcript(&meeting.id, "whisper-small", None, &[segment(10, "original live", "Alex")]).unwrap();
        let original = store.transcript_version(&meeting.id, "original").unwrap().unwrap();
        meeting.status = MeetingStatus::Processing;
        store.update_processing_state(&meeting).unwrap();
        let input = combined("presentationunique", false);
        let saved = store.save_transcript_version(&meeting.id, &input).unwrap();
        let canonical = store.segments(&meeting.id).unwrap();
        assert_eq!(canonical[0].text, "presentationunique");
        assert!(store.replace_segments(&meeting.id,&[segment(0,"destructive replacement","Wrong")]).unwrap_err().to_string().contains("new transcript version"));
        assert_eq!(store.segments(&meeting.id).unwrap(),canonical);
        assert!(store.search("destructive",10).unwrap().is_empty());
        assert!(canonical[0].id > original[0].segment.id);
        assert_eq!(store.transcript_version(&meeting.id, "original").unwrap(), Some(original.clone()));
        assert_eq!(store.transcript_versions(&meeting.id).unwrap().effective_id.as_deref(), Some(input.id.as_str()));
        assert_eq!(store.search("presentationunique", 10).unwrap().len(), 1);
        assert_eq!(store.search_chunks("presentationunique", Some(&meeting.id), 10).unwrap().len(), 1);
        drop(store);
        let store = Store::open_legacy(&path).unwrap();
        assert_eq!(store.save_transcript_version(&meeting.id, &input).unwrap(), saved);
        assert_eq!(store.segments(&meeting.id).unwrap(), canonical);
        assert_eq!(store.transcript_version(&meeting.id, "original").unwrap(), Some(original));
        let effective = store.transcript_version(&meeting.id, &input.id).unwrap().unwrap();
        assert_eq!(effective[0].segment.id, canonical[0].id);
        assert_eq!(effective[0].provenance, input.segments[0].provenance);
        let replacement = combined("differentsecondresult", false);
        store.save_transcript_version(&meeting.id, &replacement).unwrap();
        assert_eq!(store.segments(&meeting.id).unwrap(), canonical);
        assert_eq!(store.transcript_versions(&meeting.id).unwrap().effective_id.as_deref(), Some(input.id.as_str()));
    }

    #[test]
    fn partial_combined_and_user_corrections_never_replace_canonical_text() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("Protected fixture", None);
        store.create_meeting(&meeting).unwrap();
        store.publish_initial_live_transcript(&meeting.id, "whisper-small", None, &[segment(10, "live", "Alex")]).unwrap();
        meeting.status = MeetingStatus::Ready;
        store.update_processing_state(&meeting).unwrap();
        let partial = combined("partial candidate", true);
        assert!(store.save_transcript_version(&meeting.id, &partial).unwrap().partial);
        assert_eq!(store.transcript_versions(&meeting.id).unwrap().effective_id.as_deref(), Some("original"));
        let original = store.segments(&meeting.id).unwrap();
        store.set_transcript_correction(&meeting.id, original[0].id, Some("user correction")).unwrap();
        let corrected = store.segments(&meeting.id).unwrap();
        store.save_transcript_version(&meeting.id, &combined("new candidate", false)).unwrap();
        assert_eq!(store.segments(&meeting.id).unwrap(), corrected);
        assert_eq!(store.transcript_versions(&meeting.id).unwrap().effective_id.as_deref(), Some("original"));
    }

    #[test]
    fn failed_combined_promotion_rolls_back_archive_version_and_projection() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("Rollback fixture", None);
        store.create_meeting(&meeting).unwrap();
        store.publish_initial_live_transcript(&meeting.id, "whisper-small", None, &[segment(10, "live", "Alex")]).unwrap();
        meeting.status = MeetingStatus::Processing;
        store.update_processing_state(&meeting).unwrap();
        let before = store.segments(&meeting.id).unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER fail_combined BEFORE INSERT ON settings WHEN new.key LIKE 'transcript-version-publication:%' BEGIN SELECT RAISE(ABORT,'fixture fault'); END;").unwrap();
        let input = combined("candidate", false);
        assert!(store.save_transcript_version(&meeting.id, &input).is_err());
        assert_eq!(store.segments(&meeting.id).unwrap(), before);
        assert!(store.transcript_version(&meeting.id, &input.id).unwrap().is_none());
        assert_eq!(store.transcript_versions(&meeting.id).unwrap().versions.len(), 1);
    }

    #[test]
    fn combined_can_publish_after_empty_live_recording_while_processing() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("Empty live", None);
        meeting.status = MeetingStatus::Processing;
        store.create_meeting(&meeting).unwrap();
        let input = combined("unique recovered speech", false);
        store.save_transcript_version(&meeting.id, &input).unwrap();
        let list = store.transcript_versions(&meeting.id).unwrap();
        assert!(list.initial_id.is_none());
        assert_eq!(list.effective_id.as_deref(), Some(input.id.as_str()));
        assert!(store.transcript_version(&meeting.id, "original").unwrap().is_none());
    }

    #[test]
    fn diarization_enriches_initial_live_rows_and_survives_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bazza.db");
        let meeting = Meeting::new_recording("Synthetic live captions", None);
        let store = Store::open_legacy(&path).unwrap();
        store.create_meeting(&meeting).unwrap();
        let mut row = segment(300, "Original words", "");
        row.speaker = None;
        store.publish_initial_live_transcript(&meeting.id, "whisper-small", None, &[row]).unwrap();
        let original = store.segments(&meeting.id).unwrap().remove(0);
        let label = "Speaker 1".to_string();
        let rows = [MeetingSpeaker {
            meeting_id: meeting.id.clone(), label: label.clone(), speaker_id: None,
            suggested_id: None, suggested_name: None, suggested_score: None,
        }];
        let updates = [(original.id, Some(label.clone()))];
        let query_key = format!("speaker_embeddings:fixture:{}", meeting.id);
        let queries = BTreeMap::from([(label.clone(), vec![1.0])]);
        store.apply_initial_live_speakers(&meeting.id, &updates, &rows, &query_key, &queries).unwrap();
        drop(store);

        let store = Store::open_legacy(&path).unwrap();
        let mut updated = store.segments(&meeting.id).unwrap().remove(0);
        assert_eq!(updated.speaker.take(), Some(label));
        assert_eq!((updated.id, updated.start_ms, updated.end_ms, updated.text),
            (original.id, original.start_ms, original.end_ms, original.text));
        assert_eq!(store.get_setting::<BTreeMap<String, Vec<f32>>>(&query_key).unwrap(), Some(queries));
        assert!(store.apply_initial_live_speakers(&meeting.id, &updates, &[], &query_key, &BTreeMap::new()).is_err());
        assert_eq!(store.meeting_speakers(&meeting.id).unwrap().len(), 1);
    }

    #[test]
    fn manual_only_meeting_does_not_claim_to_have_an_initial_version() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("manual only", None);
        meeting.status = MeetingStatus::Ready;
        store.create_meeting(&meeting).unwrap();
        let version = store
            .save_transcript_version(
                &meeting.id,
                &NewTranscriptVersion {
                    partial: false,
                    id: uuid::Uuid::new_v4().to_string(),
                    source: TranscriptVersionSource::Recording,
                    model: "whisper-small".into(),
                    language: None,
                    segments: vec![segment(20, "manual text", "Speaker 1").into()],
                },
            )
            .unwrap();
        let list = store.transcript_versions(&meeting.id).unwrap();
        assert!(list.initial_id.is_none());
        assert_eq!(list.versions, vec![version]);
    }

    #[test]
    fn legacy_v9_keeps_original_corrections_and_manual_alternatives_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("version-test.sqlite");
        let store = Store::open_legacy(&path).unwrap();
        let mut meeting = Meeting::new_recording("versions", None);
        store.create_meeting(&meeting).unwrap();
        let long_speaker_name = "Марина".repeat(40);
        let original = segment(50, "earliest original", &long_speaker_name);
        store
            .publish_initial_live_transcript(
                &meeting.id,
                "whisper-small",
                Some("ru"),
                std::slice::from_ref(&original),
            )
            .unwrap();
        assert!(store.has_published_initial_live(&meeting.id).unwrap());
        let original_id = store.segments(&meeting.id).unwrap()[0].id;
        meeting.status = MeetingStatus::Ready;
        store.update_processing_state(&meeting).unwrap();
        store
            .set_transcript_correction(&meeting.id, original_id, Some("manual correction"))
            .unwrap();
        store
            .publish_initial_live_transcript(
                &meeting.id,
                "whisper-small",
                Some("ru"),
                &[segment(25, "different", "Wrong")],
            )
            .unwrap();

        let mut input = NewTranscriptVersion {
            partial: false,
            id: uuid::Uuid::new_v4().to_string(),
            source: TranscriptVersionSource::Discord,
            model: "whisper-large-v3-turbo".into(),
            language: Some("ru".into()),
            segments: vec![
                segment(60, "second alternate", "Борис").into(),
                segment(45, "first alternate", "Марина").into(),
            ],
        };
        let playback = bazza_domain::transcript_versions::TranscriptPlaybackTiming {
            start_ms: 55,
            end_ms: 155,
        };
        input.segments[1].playback = Some(playback);
        let saved = store.save_transcript_version(&meeting.id, &input).unwrap();
        assert_eq!(
            store.save_transcript_version(&meeting.id, &input).unwrap(),
            saved
        );
        input.segments[0].segment.text = "changed".into();
        assert!(store.save_transcript_version(&meeting.id, &input).is_err());
        drop(store);

        assert_eq!(
            crate::schema_version(&path).unwrap(),
            crate::migrations::LEGACY_SCHEMA
        );
        let store = Store::open_legacy(&path).unwrap();
        let listing = store.transcript_versions(&meeting.id).unwrap();
        assert_eq!(
            listing.initial_id.as_deref(),
            Some(INITIAL_TRANSCRIPT_VERSION_ID)
        );
        assert_eq!(listing.versions[0].source, TranscriptVersionSource::Live);
        assert_eq!(listing.versions[1], saved);
        let canonical = store
            .transcript_version(&meeting.id, INITIAL_TRANSCRIPT_VERSION_ID)
            .unwrap()
            .unwrap();
        let canonical = &canonical[0].segment;
        assert_eq!(canonical.id, original_id);
        assert_eq!(canonical.text, original.text);
        assert_eq!(
            canonical.speaker.as_deref(),
            Some(long_speaker_name.as_str())
        );
        assert_eq!(
            canonical.clean_text.as_deref(),
            Some("manual correction")
        );
        let alternative = store
            .transcript_version(&meeting.id, &input.id)
            .unwrap()
            .unwrap();
        assert_eq!(alternative[0].segment.text, "first alternate");
        assert_eq!(alternative[0].segment.start_ms, 45);
        assert_eq!(alternative[0].playback, Some(playback));
        assert_eq!(alternative[1].segment.speaker.as_deref(), Some("Борис"));
        assert_eq!(alternative[1].playback, None);
        assert!(alternative
            .iter()
            .all(|row| row.segment.id < 0 && row.segment.clean_text.is_none()));
        assert!(store
            .set_transcript_correction(&meeting.id, alternative[0].segment.id, Some("wrong view"))
            .is_err());

        store.delete_meeting(&meeting.id).unwrap();
        assert!(store
            .get_setting::<TranscriptVersion>(&format!("{}{}", info_prefix(&meeting.id), input.id))
            .unwrap()
            .is_none());
        assert!(store
            .get_setting::<NewTranscriptVersion>(&format!(
                "{}{}",
                data_prefix(&meeting.id),
                input.id
            ))
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_initial_publication_rolls_back_rows_and_metadata() {
        let store = Store::open_in_memory().unwrap();
        let meeting = Meeting::new_recording("versions", None);
        store.create_meeting(&meeting).unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER fail_initial BEFORE INSERT ON settings WHEN new.key LIKE 'transcript-version-info:%' BEGIN SELECT RAISE(ABORT,'fixture fault'); END;").unwrap();
        assert!(store
            .publish_initial_live_transcript(
                &meeting.id,
                "whisper-small",
                None,
                &[segment(25, "initial", "Alex")]
            )
            .is_err());
        assert!(store.segments(&meeting.id).unwrap().is_empty());
        assert!(!store.has_published_initial_live(&meeting.id).unwrap());
    }

    #[test]
    fn legacy_import_without_version_metadata_is_the_original() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open_legacy(&directory.path().join("imported.sqlite")).unwrap();
        let staged = tempfile::tempdir_in(directory.path()).unwrap();
        let mut meeting = Meeting::new_recording("older import", None);
        meeting.status = MeetingStatus::Ready;
        let source = Segment {
            id: 20,
            meeting_id: meeting.id.clone(),
            start_ms: 10,
            end_ms: 30,
            speaker: Some("Анна".into()),
            text: "Imported raw".into(),
            clean_text: Some("Imported correction".into()),
            clean_text_manual: true,
        };
        let imported = crate::MeetingImport {
            meeting: &meeting,
            segments: std::slice::from_ref(&source),
            speakers: &[],
            summary: None,
            highlights: &[],
            tags: &[],
            transcript: None,
        };
        assert!(store.import_meeting(&imported, staged.path()).unwrap());
        // Historical optional provenance may be absent or malformed independently.
        store
            .set_setting(&format!("stt:{}", meeting.id), &17)
            .unwrap();
        let list = store.transcript_versions(&meeting.id).unwrap();
        assert_eq!(
            list.initial_id.as_deref(),
            Some(INITIAL_TRANSCRIPT_VERSION_ID)
        );
        assert_eq!(list.versions[0].source, TranscriptVersionSource::Existing);
        let read = store
            .transcript_version(&meeting.id, INITIAL_TRANSCRIPT_VERSION_ID)
            .unwrap()
            .unwrap();
        assert_eq!(read[0].segment.text, source.text);
        assert_eq!(read[0].segment.clean_text, source.clean_text);
        assert!(read[0].segment.clean_text_manual);
    }
}

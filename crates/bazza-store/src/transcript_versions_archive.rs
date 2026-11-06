//! Typed portable history; importing it grants no captured-worker authority.
use super::*;
use bazza_domain::transcript_versions::{
    NewTranscriptVersionSegment, TranscriptVersionArchive, TranscriptVersionSnapshot,
};

impl Store {
    pub fn transcript_version_archive(&self, meeting_id: &str) -> Result<Option<TranscriptVersionArchive>> {
        let listing = self.transcript_versions(meeting_id)?;
        if listing.versions.is_empty() { return Ok(None); }
        if listing.versions.iter().all(|row| row.id == INITIAL_TRANSCRIPT_VERSION_ID) {
            let conn = self.conn.lock();
            // A historical provider setting may make the synthetic initial
            // descriptor look Live. It is not independently saved history and
            // must keep the old v1 canonical-segment defaults on roundtrip.
            if initial_info_on(&conn,meeting_id).map_err(legacy_error)?.is_none()
                && !has_combined_publication_on(&conn,meeting_id)?
            { return Ok(None); }
        }
        let mut original = None;
        let mut alternatives = Vec::new();
        let mut bytes = 0_u64;
        for metadata in listing.versions {
            let segments = self.transcript_version(meeting_id, &metadata.id)?
                .ok_or_else(|| StoreError::Other("transcript version changed during export".into()))?;
            let snapshot = TranscriptVersionSnapshot { metadata, segments };
            bytes = bytes.saturating_add(serde_json::to_vec(&snapshot)?.len() as u64);
            if bytes > MAX_LOCAL_ASR_BODY_BYTES {
                return Err(StoreError::Other("transcript version history exceeds the portable byte budget".into()));
            }
            if snapshot.metadata.id == INITIAL_TRANSCRIPT_VERSION_ID { original = Some(snapshot); }
            else { alternatives.push(snapshot); }
        }
        let archive = TranscriptVersionArchive { schema:1, original, alternatives, effective_id:listing.effective_id };
        if !archive.valid(meeting_id, &self.segments(meeting_id)?) {
            return Err(StoreError::Other("transcript version archive is invalid or changed during export".into()));
        }
        Ok(Some(archive))
    }
}

pub(crate) fn import_archive_on(
    tx: &Transaction<'_>, meeting_id: &str, archive: &TranscriptVersionArchive,
) -> DataResult<()> {
    let current = crate::segments::segments_on(tx, meeting_id)?;
    if !archive.valid(meeting_id, &current) {
        return Err(DataError::Invalid("imported transcript version archive"));
    }
    if let Some(original) = &archive.original {
        tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2)",params![
            format!("{}{}",info_prefix(meeting_id),INITIAL_TRANSCRIPT_VERSION_ID),serde_json::to_string(&original.metadata)?])?;
    }
    for snapshot in &archive.alternatives {
        let input = NewTranscriptVersion {
            id:snapshot.metadata.id.clone(),source:snapshot.metadata.source,
            model:snapshot.metadata.model.clone().ok_or(DataError::Invalid("imported transcript model"))?,
            language:snapshot.metadata.language.clone(),partial:snapshot.metadata.partial,
            segments:snapshot.segments.iter().map(|row| NewTranscriptVersionSegment {
                segment:NewSegment { start_ms:row.segment.start_ms,end_ms:row.segment.end_ms,
                    speaker:row.segment.speaker.clone(),text:row.segment.text.clone() },
                playback:row.playback,provenance:row.provenance.clone(),
            }).collect(),
        };
        save_manual_on(tx,meeting_id,&input)?;
        // Creation time is historical metadata, not the import time.
        tx.execute("UPDATE settings SET value=?2 WHERE key=?1",params![
            format!("{}{}",info_prefix(meeting_id),snapshot.metadata.id),serde_json::to_string(&snapshot.metadata)?])?;
    }
    if let Some(effective) = archive.effective_id.as_deref().filter(|id| *id != INITIAL_TRANSCRIPT_VERSION_ID) {
        let original = archive.original.as_ref().map(|snapshot| snapshot.segments.iter().cloned().enumerate()
            .map(|(index,mut row)| {
                // These IDs can never accidentally target an imported current
                // segment. Archive editability also follows effective_id.
                row.segment.id = -(index as i64 + 1);
                row
            }).collect()).unwrap_or_default();
        tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2)",params![publication_key(meeting_id),serde_json::to_string(&Publication {
            effective_id:effective.into(),original,canonical_ids:current.iter().map(|row| row.id).collect(),job_id:None,source_id:None,
        })?])?;
        if effective_id_on(tx,meeting_id)?.as_deref() != Some(effective) {
            return Err(DataError::Invalid("imported effective transcript projection"));
        }
    }
    Ok(())
}

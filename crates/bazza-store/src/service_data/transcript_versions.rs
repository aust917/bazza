//! Fenced version writes and atomic publication of complete combined results.
use super::{
    check_revision, documents::transcript_state_on, safe_id, visible_meeting, DataError,
    DataResult, DocumentState,
};
use crate::transcript_versions::{archived_original_on, bind_combined_job_on, effective_rows_on, initial_info_on, listing_on, manual_rows_on, promote_combined_on, save_manual_on};
use crate::Store;
use bazza_domain::transcript_versions::{
    NewTranscriptVersion, TranscriptVersion, TranscriptVersionSegment, TranscriptVersionSource,
    TranscriptVersions, INITIAL_TRANSCRIPT_VERSION_ID,
};
use rusqlite::{params, Connection, OptionalExtension};

fn original_on(
    conn: &Connection,
    meeting_id: &str,
    source_id: &str,
) -> DataResult<TranscriptVersion> {
    let (origin, label, language, created_at, model): (String, Option<String>, Option<String>, _, Option<String>) = conn.query_row(
        "SELECT src.origin,src.source_label,src.language,src.created_at,
                COALESCE(local.model,(SELECT json_extract(c.provider_json,'$.model')
                    FROM service_post_process_anchors a JOIN service_capture_leases c ON c.job_id=a.job_id
                    WHERE a.meeting_id=src.meeting_id AND a.root_source_id=src.id LIMIT 1))
         FROM service_transcript_sources src
         LEFT JOIN service_post_process_local_sources local ON local.source_id=src.id
         WHERE src.meeting_id=?1 AND src.id=?2",
        params![meeting_id, source_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).optional()?.ok_or(DataError::NotFound)?;
    if origin == "legacy" {
        if let Some(info) = initial_info_on(conn, meeting_id)? {
            return Ok(info);
        }
    }
    let source = match (origin.as_str(), label.as_deref()) {
        ("captured", Some("local_live" | "soniox")) => TranscriptVersionSource::Live,
        ("local_asr", _) => TranscriptVersionSource::Recording,
        _ => TranscriptVersionSource::Existing,
    };
    Ok(TranscriptVersion {
        id: INITIAL_TRANSCRIPT_VERSION_ID.into(),
        source,
        model,
        language,
        created_at,
        partial: false,
    })
}

impl Store {
    pub fn service_transcript_versions(&self, meeting_id: &str) -> DataResult<TranscriptVersions> {
        safe_id(meeting_id)?;
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        let state = transcript_state_on(&conn, meeting_id)?;
        if state.document_state == DocumentState::Deleted {
            return Ok(TranscriptVersions::default());
        }
        let original = if let Some(archived) = archived_original_on(&conn, meeting_id)? {
            if archived.is_empty() { None } else { initial_info_on(&conn, meeting_id)? }
        } else { state
            .current_source_id
            .as_deref()
            .map(|source| original_on(&conn, meeting_id, source))
            .transpose()? };
        listing_on(&conn, meeting_id, original)
    }

    pub fn service_transcript_version(
        &self,
        meeting_id: &str,
        version_id: &str,
    ) -> DataResult<Option<Vec<TranscriptVersionSegment>>> {
        safe_id(meeting_id)?;
        let conn = self.conn.lock();
        visible_meeting(&conn, meeting_id)?;
        let state = transcript_state_on(&conn, meeting_id)?;
        if state.document_state == DocumentState::Deleted {
            return Ok(None);
        }
        if version_id == INITIAL_TRANSCRIPT_VERSION_ID {
            if let Some(archived) = archived_original_on(&conn, meeting_id)? {
                return Ok((!archived.is_empty()).then_some(archived));
            }
            return if state.document_state == DocumentState::Present {
                let rows = crate::segments::segments_on(&conn, meeting_id)?;
                Ok(Some(rows.into_iter().map(Into::into).collect()))
            } else {
                Ok(None)
            };
        }
        if let Some(rows) = effective_rows_on(&conn, meeting_id, version_id)? {
            return Ok(Some(rows));
        }
        manual_rows_on(&conn, meeting_id, version_id)
    }

    pub fn service_save_transcript_version(
        &self,
        meeting_id: &str,
        expected_lifecycle_revision: &str,
        expected_transcript_revision: &str,
        input: &NewTranscriptVersion,
    ) -> DataResult<TranscriptVersion> {
        safe_id(meeting_id)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let (status, lifecycle): (String, String) = tx.query_row(
            "SELECT m.status,s.lifecycle_revision FROM meetings m JOIN service_meeting_state s ON s.meeting_id=m.id
             WHERE m.id=?1 AND s.content_kind='recorded' AND s.tombstoned=0",
            [meeting_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.ok_or(DataError::NotFound)?;
        check_revision(&lifecycle, expected_lifecycle_revision)?;
        let state = transcript_state_on(&tx, meeting_id)?;
        check_revision(&state.revision, expected_transcript_revision)?;
        if state.document_state == DocumentState::Deleted {
            return Err(DataError::Forbidden);
        }
        if !matches!(status.as_str(), "ready" | "failed")
            && !(status == "processing" && (state.document_state == DocumentState::Present || input.source == TranscriptVersionSource::Combined))
        {
            return Err(DataError::Busy);
        }
        let version = save_manual_on(&tx, meeting_id, input)?;
        let original = state.current_source_id.as_deref()
            .map(|source| original_on(&tx, meeting_id, source)).transpose()?;
        let protected: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM meeting_speakers WHERE meeting_id=?1 AND speaker_id IS NOT NULL)
             OR EXISTS(SELECT 1 FROM service_transcript_sources WHERE id=?2 AND origin IN ('imported','external_agent'))
             OR EXISTS(SELECT 1 FROM service_transcript_overlays WHERE source_id=?2 AND manual=1)",
            params![meeting_id,state.current_source_id], |row| row.get(0))?;
        // Only an uncompleted Ready checkpoint can continue automatic work.
        // Terminal jobs and their historical completion receipts stay terminal.
        let ready_job: Option<String> = if input.source == TranscriptVersionSource::Combined && !input.partial && !protected { tx.query_row(
            "SELECT p.job_id FROM service_post_process_checkpoints p
             JOIN service_post_process_anchors a ON a.job_id=p.job_id
             JOIN service_jobs j ON j.id=p.job_id
             WHERE a.meeting_id=?1 AND p.lifecycle_revision=?2 AND p.transcript_revision=?3
               AND p.source_id IS ?4 AND p.phase='ready' AND p.state='active'
               AND j.state IN ('running','waiting_for_worker','interrupted') AND j.cancel_requested=0
               AND NOT EXISTS(SELECT 1 FROM service_post_process_generated_summaries WHERE job_id=p.job_id)
             ORDER BY p.job_id LIMIT 1",
            params![meeting_id,lifecycle,state.revision,state.current_source_id], |row| row.get(0)).optional()? } else { None };
        if let Some(job) = &ready_job {
            if !matches!(super::postprocess::candidate_on(&tx, job)?.stage, super::PostProcessStage::Ready { cleanup: false, .. } | super::PostProcessStage::SonioxReady { cleanup: false, .. }) {
                return Err(DataError::Forbidden);
            }
        }
        if promote_combined_on(&tx, meeting_id, input, original, protected)? {
            // Retain the captured source and its overlays as immutable history.
            // The new source is also consumed by service readers, not only the
            // legacy segment/search projection.
            let rows = crate::segments::segments_on(&tx, meeting_id)?;
            let source_id = super::revision();
            let revision = super::revision();
            tx.execute("INSERT INTO service_transcript_sources(id,meeting_id,origin,source_label,language,raw_json,created_at) VALUES(?1,?2,'combined',?3,?4,?5,?6)",
                params![source_id,meeting_id,input.id,input.language,serde_json::to_string(&rows)?,chrono::Utc::now()])?;
            tx.execute("UPDATE service_transcript_state SET revision=?2,current_source_id=?3,document_state='present' WHERE meeting_id=?1",params![meeting_id,revision,source_id])?;
            super::postprocess_checkpoint::invalidate_on(&tx, meeting_id)?;
            if let Some(job) = ready_job {
                bind_combined_job_on(&tx, meeting_id, &job, &source_id)?;
                tx.execute("UPDATE service_post_process_checkpoints SET revision=?2,transcript_revision=?3,source_id=?4,document_state='present',phase='ready',state='active' WHERE job_id=?1",
                    params![job,super::revision(),revision,source_id])?;
                let job_revision = super::revision();
                tx.execute("UPDATE service_jobs SET state='waiting_for_worker',revision=?2,claim_token=NULL,lease_until=NULL,error_code=NULL,updated_at=?3 WHERE id=?1",
                    params![job,job_revision,chrono::Utc::now()])?;
                super::consistency::change(&tx,"job",&job,&job_revision,"desktop_combined_transcript")?;
            }
            super::search::rebuild_meeting_on(&tx, meeting_id)?;
            super::consistency::change(&tx, "transcript", meeting_id, &revision, "desktop_combined_transcript")?;
        }
        tx.commit()?;
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Meeting, MeetingStatus, NewSegment};
    use bazza_domain::transcript_versions::manual_transcript_version_id_valid;

    fn managed_fixture() -> (Store, Meeting) {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("Managed combined fixture", None);
        meeting.status = MeetingStatus::Processing;
        store.create_meeting(&meeting).unwrap();
        let original = NewSegment { start_ms: 10, end_ms: 50, speaker: Some("Alex".into()), text: "initial live".into() };
        store.replace_segments(&meeting.id, &[original]).unwrap();
        let raw = store.segments(&meeting.id).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO service_meeting_state VALUES(?1,'recorded','meta','life',0)", [&meeting.id]).unwrap();
            conn.execute("INSERT INTO service_transcript_sources VALUES('source',?1,'captured','local_live','ru',?2,?3)", params![meeting.id,serde_json::to_string(&raw).unwrap(),meeting.created_at]).unwrap();
            conn.execute("INSERT INTO service_transcript_state VALUES(?1,'transcript','source','present',0)", [&meeting.id]).unwrap();
            conn.execute("INSERT INTO service_summary_state(meeting_id,revision,document_state,origin,auto_replace) VALUES(?1,'summary','absent','legacy',0)", [&meeting.id]).unwrap();
        }
        store.service_rebuild_search().unwrap();
        (store, meeting)
    }

    fn combined_fixture(partial: bool) -> NewTranscriptVersion {
        use bazza_domain::transcript_versions::{NewTranscriptVersionSegment, TranscriptSegmentProvenance, TranscriptSegmentSource};
        let mut row: NewTranscriptVersionSegment = NewSegment { start_ms: 100, end_ms: 200, speaker: Some("System".into()), text: "additionalpresentation".into() }.into();
        row.provenance = Some(TranscriptSegmentProvenance { source: TranscriptSegmentSource::System, provider: None, participant_id: None,
            source_start_ms: 100, source_end_ms: 200, uncertain: partial });
        NewTranscriptVersion { id: uuid::Uuid::new_v4().to_string(), source: TranscriptVersionSource::Combined,
            model: "whisper-small".into(), language: Some("ru".into()), partial, segments: vec![row] }
    }

    #[test]
    fn managed_combined_is_fenced_and_publishes_source_projection_and_search_atomically() {
        let (store, meeting) = managed_fixture();
        let original = store.service_transcript_version(&meeting.id, "original").unwrap();
        let input = combined_fixture(false);
        assert!(matches!(store.service_save_transcript_version(&meeting.id,"stale","transcript",&input), Err(DataError::Conflict { .. })));
        let result = store.service_save_transcript_version(&meeting.id,"life","transcript",&input).unwrap();
        assert!(!result.partial);
        let state = store.service_transcript_state(&meeting.id).unwrap();
        assert_ne!(state.revision, "transcript");
        assert_ne!(state.current_source_id.as_deref(), Some("source"));
        let current = store.service_read_transcript(&meeting.id, &super::super::TranscriptRead::default()).unwrap();
        assert_eq!(current.origin.as_deref(), Some("combined"));
        assert_eq!(current.segments[0].text, "additionalpresentation");
        assert_eq!(store.service_transcript_version(&meeting.id,"original").unwrap(), original);
        assert_eq!(store.service_transcript_versions(&meeting.id).unwrap().effective_id.as_deref(),Some(input.id.as_str()));
        let search = store.service_search(&super::super::SearchRequest { query: "additionalpresentation".into(), fields: vec!["transcript".into()], limit: 10, ..Default::default() }).unwrap();
        assert!(!search.hits.is_empty());
        let rows = store.segments(&meeting.id).unwrap();
        // Make ordinary document writes admissible: this must exercise the
        // published-version guard, not the generic Processing reservation.
        store.conn.lock().execute("UPDATE meetings SET status='ready' WHERE id=?1",[&meeting.id]).unwrap();
        let replacement = super::super::TranscriptWrite {
            expected_revision:state.revision.clone(),
            segments:vec![NewSegment { start_ms:0,end_ms:10,speaker:None,text:"destructivereplacement".into() }],
            source_label:Some("explicit replacement".into()),language:Some("ru".into()),
            origin:super::super::TranscriptImportOrigin::Imported,
        };
        let context = super::super::MutationContext { principal:"fixture".into(),idempotency_key:"replace-published-combined".into() };
        assert!(matches!(store.service_replace_transcript(&context,&meeting.id,&replacement),Err(DataError::Busy)));
        assert_eq!(store.segments(&meeting.id).unwrap(),rows);
        assert_eq!(store.service_transcript_version(&meeting.id,"original").unwrap(),original);
        assert_eq!(store.service_transcript_state(&meeting.id).unwrap().revision,state.revision);
        let absent = store.service_search(&super::super::SearchRequest { query:"destructivereplacement".into(),fields:vec!["transcript".into()],limit:10,..Default::default() }).unwrap();
        assert!(absent.hits.is_empty());
        assert_eq!(store.service_transcript_versions(&meeting.id).unwrap().effective_id.as_deref(),Some(input.id.as_str()));
        assert_eq!(store.service_save_transcript_version(&meeting.id,"life",&state.revision,&input).unwrap(), result);
        assert_eq!(store.service_transcript_state(&meeting.id).unwrap().revision,state.revision);
        assert_eq!(store.segments(&meeting.id).unwrap(),rows);
    }

    #[test]
    fn managed_partial_or_manual_overlay_never_promotes_combined() {
        let (store, meeting) = managed_fixture();
        let original = store.segments(&meeting.id).unwrap();
        store.service_save_transcript_version(&meeting.id,"life","transcript",&combined_fixture(true)).unwrap();
        assert_eq!(store.segments(&meeting.id).unwrap(),original);
        {
            let conn = store.conn.lock();
            conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES('source',?1,'user words',1)",[original[0].id.to_string()]).unwrap();
            conn.execute("UPDATE segments SET clean_text='user words',clean_text_manual=1 WHERE meeting_id=?1",[&meeting.id]).unwrap();
        }
        let corrected = store.segments(&meeting.id).unwrap();
        store.service_save_transcript_version(&meeting.id,"life","transcript",&combined_fixture(false)).unwrap();
        assert_eq!(store.segments(&meeting.id).unwrap(),corrected);
        assert_eq!(store.service_transcript_state(&meeting.id).unwrap().revision,"transcript");
    }

    #[test]
    fn managed_combined_failure_rolls_back_version_archive_source_and_rows() {
        let (store, meeting) = managed_fixture();
        let original = store.segments(&meeting.id).unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER reject_combined BEFORE INSERT ON service_transcript_sources WHEN new.origin='combined' BEGIN SELECT RAISE(ABORT,'fixture fault'); END;").unwrap();
        let input = combined_fixture(false);
        assert!(store.service_save_transcript_version(&meeting.id,"life","transcript",&input).is_err());
        assert_eq!(store.segments(&meeting.id).unwrap(),original);
        assert_eq!(store.service_transcript_state(&meeting.id).unwrap().revision,"transcript");
        assert!(store.service_transcript_version(&meeting.id,&input.id).unwrap().is_none());
        assert_eq!(store.service_transcript_versions(&meeting.id).unwrap().effective_id.as_deref(),Some("original"));
    }

    #[test]
    fn managed_manual_write_is_fenced_and_does_not_replace_or_keep_deleted_text() {
        let store = Store::open_in_memory().unwrap();
        let mut meeting = Meeting::new_recording("managed versions", None);
        meeting.status = MeetingStatus::Ready;
        store.create_meeting(&meeting).unwrap();
        store
            .replace_segments(
                &meeting.id,
                &[NewSegment {
                    start_ms: 10,
                    end_ms: 50,
                    speaker: Some("Анна".into()),
                    text: "saved original".into(),
                }],
            )
            .unwrap();
        let raw = store.segments(&meeting.id).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute(
                "INSERT INTO service_meeting_state VALUES(?1,'recorded','meta','life',0)",
                [&meeting.id],
            )
            .unwrap();
            conn.execute("INSERT INTO service_transcript_sources VALUES('source',?1,'captured','local_live','ru',?2,?3)",
                params![meeting.id, serde_json::to_string(&raw).unwrap(), meeting.created_at]).unwrap();
            conn.execute(
                "INSERT INTO service_transcript_state VALUES(?1,'transcript','source','present',0)",
                [&meeting.id],
            )
            .unwrap();
            conn.execute("INSERT INTO service_summary_state(meeting_id,revision,document_state,origin,auto_replace) VALUES(?1,'summary','absent','legacy',0)", [&meeting.id]).unwrap();
        }
        let mut input = NewTranscriptVersion {
            partial: false,
            id: uuid::Uuid::new_v4().to_string(),
            source: TranscriptVersionSource::Meet,
            model: "whisper-small".into(),
            language: Some("ru".into()),
            segments: vec![NewSegment {
                start_ms: 25,
                end_ms: 40,
                speaker: Some("Борис".into()),
                text: "manual alternate".into(),
            }
            .into()],
        };
        assert!(matches!(
            store.service_save_transcript_version(&meeting.id, "stale", "transcript", &input),
            Err(DataError::Conflict { .. })
        ));
        let result = store
            .service_save_transcript_version(&meeting.id, "life", "transcript", &input)
            .unwrap();
        assert!(manual_transcript_version_id_valid(&result.id));
        assert_eq!(
            store
                .service_save_transcript_version(&meeting.id, "life", "transcript", &input)
                .unwrap(),
            result
        );
        input.segments[0].segment.text = "changed".into();
        assert!(matches!(
            store.service_save_transcript_version(&meeting.id, "life", "transcript", &input),
            Err(DataError::IdempotencyConflict)
        ));

        let listing = store.service_transcript_versions(&meeting.id).unwrap();
        assert_eq!(
            listing.initial_id.as_deref(),
            Some(INITIAL_TRANSCRIPT_VERSION_ID)
        );
        assert_eq!(listing.versions[0].source, TranscriptVersionSource::Live);
        assert_eq!(listing.versions[1], result);
        assert_eq!(
            store
                .service_transcript_version(&meeting.id, INITIAL_TRANSCRIPT_VERSION_ID)
                .unwrap(),
            Some(raw.into_iter().map(Into::into).collect())
        );
        let rows = store
            .service_transcript_version(&meeting.id, &input.id)
            .unwrap()
            .unwrap();
        assert_eq!(rows[0].segment.text, "manual alternate");
        assert!(rows[0].segment.id < 0);
        assert_eq!(rows[0].playback, None);
        let state = store.service_transcript_state(&meeting.id).unwrap();
        assert_eq!(state.revision, "transcript");
        assert_eq!(state.current_source_id.as_deref(), Some("source"));

        let context = crate::service_data::MutationContext {
            principal: "fixture".into(),
            idempotency_key: "erase-test-transcript".into(),
        };
        store
            .service_delete_transcript(&context, &meeting.id, "transcript")
            .unwrap();
        assert!(store
            .service_transcript_versions(&meeting.id)
            .unwrap()
            .versions
            .is_empty());
        assert!(store
            .service_transcript_version(&meeting.id, &input.id)
            .unwrap()
            .is_none());
        let surviving: i64 = store
            .conn
            .lock()
            .query_row(
                "SELECT count(*) FROM settings WHERE key LIKE 'transcript-version-%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(surviving, 0);
    }
}

//! Fixture-only terminal transition, exercised through registered Store calls.
use super::*;

fn publish(
    f: &Fixture,
    body: &str,
    call: impl Fn(&PostProcessCleanupRequest) -> Option<PostProcessJobLease>,
) -> PostProcessJobLease {
    let upload_id = Uuid::new_v4();
    let digest = sha(body.as_bytes());
    for operation in [
        PostProcessCleanupOperation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: digest.clone(),
        },
        PostProcessCleanupOperation::Append {
            upload_id,
            sequence: 1,
            byte_offset: 0,
            text: body.into(),
            sha256: digest,
        },
    ] {
        assert!(call(&request(f, operation)).is_none());
    }
    call(&request(
        f,
        PostProcessCleanupOperation::Commit { upload_id },
    ))
    .unwrap()
}

fn ready(f: &mut Fixture, empty: bool) {
    let captions = if empty {
        serde_json::json!([])
    } else {
        serde_json::json!([{"start_ms":0,"end_ms":100,"text":"local fixture text"}])
    };
    let source = serde_json::json!({
        "model":"fixture",
        "language":null,
        "outcome":"complete",
        "captions":captions,
    });
    f.lease = publish(f, &source.to_string(), |request| {
        match f
            .store
            .service_post_process_source(PID, f.epoch, request)
            .unwrap()
        {
            PostProcessSourceReply::Upload(_) => None,
            PostProcessSourceReply::Committed(receipt) => Some(receipt.lease),
        }
    });
    let reason = if empty {
        "empty_transcript"
    } else {
        "disabled"
    };
    let speaker = serde_json::json!({"kind":"skipped","reason":reason});
    f.lease = publish(f, &speaker.to_string(), |request| {
        match f
            .store
            .service_post_process_speakers(PID, f.epoch, request)
            .unwrap()
        {
            PostProcessSpeakersReply::Upload(_) => None,
            PostProcessSpeakersReply::Committed(receipt) => Some(receipt.lease),
        }
    });
    let request = PostProcessReadyRequest {
        proof: f.proof.clone(),
        lease: f.lease.clone(),
        fence: f.fence.clone(),
        operation_id: Uuid::new_v4(),
    };
    f.lease = f
        .store
        .service_mark_post_process_ready(PID, f.epoch, &request)
        .unwrap()
        .lease;
}

fn completion(f: &Fixture) -> CompleteFixturePostProcessRequest {
    CompleteFixturePostProcessRequest {
        proof: f.proof.clone(),
        lease: f.lease.clone(),
        fence: f.fence.clone(),
        operation_id: Uuid::new_v4(),
    }
}

fn finish(
    f: &Fixture,
    request: &CompleteFixturePostProcessRequest,
) -> DataResult<CompleteFixturePostProcessReceipt> {
    f.store
        .service_complete_fixture_post_process(PID, f.epoch, request)
}

#[test]
fn complete_combined_rebinds_ready_job_and_fences_old_claim_until_new_cleanup() {
    use bazza_domain::transcript_versions::{NewTranscriptVersion, NewTranscriptVersionSegment, TranscriptSegmentProvenance, TranscriptSegmentSource, TranscriptVersionSource};
    for provider in [CaptureProvider::Local, CaptureProvider::Soniox] {
        let mut f = if provider == CaptureProvider::Local {
            fixture_with_captured_rows(Store::open_in_memory().unwrap(),provider,true,true)
        } else { fixture(provider) };
        f.lease = publish(&f,r#"{"kind":"skipped","reason":"disabled"}"#,|request| {
            match f.store.service_post_process_speakers(PID,f.epoch,request).unwrap() {
                PostProcessSpeakersReply::Upload(_) => None,
                PostProcessSpeakersReply::Committed(receipt) => Some(receipt.lease),
            }
        });
        f.lease = f.store.service_mark_post_process_ready(PID,f.epoch,&PostProcessReadyRequest {
            proof:f.proof.clone(),lease:f.lease.clone(),fence:f.fence.clone(),operation_id:Uuid::new_v4(),
        }).unwrap().lease;
        let meeting = f.lease.meeting_id.to_string();
        let original = f.store.service_transcript_version(&meeting,"original").unwrap();
        let mut rows: Vec<NewTranscriptVersionSegment> = f.store.segments(&meeting).unwrap().into_iter().map(|row| {
            let provenance = TranscriptSegmentProvenance { source:TranscriptSegmentSource::System,provider:None,participant_id:None,
                source_start_ms:row.start_ms,source_end_ms:row.end_ms,uncertain:false };
            NewTranscriptVersionSegment { segment:crate::NewSegment { start_ms:row.start_ms,end_ms:row.end_ms,speaker:row.speaker,text:row.text },
                playback:None,provenance:Some(provenance) }
        }).collect();
        rows.push(NewTranscriptVersionSegment { segment:crate::NewSegment { start_ms:900,end_ms:1000,speaker:Some("System".into()),text:"unique presentation".into() },
            playback:None,provenance:Some(TranscriptSegmentProvenance { source:TranscriptSegmentSource::System,provider:None,participant_id:None,
                source_start_ms:900,source_end_ms:1000,uncertain:false }) });
        let version = NewTranscriptVersion { id:Uuid::new_v4().to_string(),source:TranscriptVersionSource::Combined,
            model:"whisper-small".into(),language:Some("ru".into()),partial:false,segments:rows };
        f.store.service_save_transcript_version(&meeting,&f.lease.input.lifecycle_revision,&f.lease.input.transcript_revision,&version).unwrap();
        // The old claim cannot publish cleanup or complete after rebinding.
        assert!(finish(&f,&completion(&f)).is_err());
        assert!(call(&f,&request(&f,PostProcessCleanupOperation::Begin {
            upload_id:Uuid::new_v4(),total_bytes:2,sha256:sha(b"[]"),
        })).is_err());
        let mut jobs = f.store.service_post_process_jobs(PID,f.epoch,&PostProcessJobsRequest::new(f.proof.clone())).unwrap().items;
        assert_eq!(jobs.len(),1);
        let job = jobs.remove(0);
        assert!(matches!(job.stage,PostProcessStage::Ready { cleanup:false,.. }));
        assert_ne!(job.input.source_id,f.lease.input.source_id);
        let fence = WorkerSecret::random();
        f.lease = f.store.service_claim_post_process_job(PID,f.epoch,&ClaimPostProcessJobRequest {
            proof:f.proof.clone(),job,claim_id:Uuid::new_v4(),operation_id:Uuid::new_v4(),fence:fence.clone(),ttl_seconds:60,
        }).unwrap();
        f.fence = fence;
        assert!(finish(&f,&completion(&f)).is_err());
        let (upload_id,_,_) = stage(&f,"[]");
        f.lease = committed(call(&f,&request(&f,PostProcessCleanupOperation::Commit { upload_id })).unwrap()).lease;
        assert!(matches!(f.store.conn.lock().query_row("SELECT phase FROM service_post_process_checkpoints WHERE job_id=?1",[&f.lease.job_id],|row| row.get::<_,String>(0)).unwrap().as_str(),"ready_cleanup"));
        assert_eq!(finish(&f,&completion(&f)).unwrap().cleanup,FixtureCleanupOutcome::Accepted);
        assert!(f.store.service_post_process_jobs(PID,f.epoch,&PostProcessJobsRequest::new(f.proof.clone())).unwrap().items.is_empty());
        assert_eq!(f.store.service_transcript_version(&meeting,"original").unwrap(),original);
        assert_eq!(f.store.service_transcript_versions(&meeting).unwrap().effective_id.as_deref(),Some(version.id.as_str()));
    }
}

#[test]
fn soniox_skipped_speakers_preserve_captured_labels_and_allow_ready() {
    let mut f = fixture(CaptureProvider::Soniox);
    let meeting = f.lease.meeting_id.to_string();
    let before = f.store.segments(&meeting).unwrap();
    assert_eq!(before.len(), 3);
    assert!(before
        .iter()
        .all(|segment| segment.speaker.as_deref() == Some("Анна")));
    let transcript_revision = f.lease.input.transcript_revision.clone();
    let source = f.lease.input.source_id.clone();
    f.lease = publish(
        &f,
        r#"{"kind":"skipped","reason":"model_unavailable"}"#,
        |request| match f
            .store
            .service_post_process_speakers(PID, f.epoch, request)
            .unwrap()
        {
            PostProcessSpeakersReply::Upload(_) => None,
            PostProcessSpeakersReply::Committed(receipt) => {
                assert_eq!(receipt.changed_segments, 0);
                Some(receipt.lease)
            }
        },
    );
    assert_eq!(f.lease.input.transcript_revision, transcript_revision);
    assert_eq!(f.lease.input.source_id, source);
    let ready = f
        .store
        .service_mark_post_process_ready(
            PID,
            f.epoch,
            &PostProcessReadyRequest {
                proof: f.proof.clone(),
                lease: f.lease.clone(),
                fence: f.fence.clone(),
                operation_id: Uuid::new_v4(),
            },
        )
        .unwrap();
    assert_eq!(ready.lease.input.transcript_revision, transcript_revision);
    assert_eq!(ready.lease.input.source_id, source);
    assert_eq!(f.store.segments(&meeting).unwrap(), before);
    f.store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [&ready.lease.job_id],
        )
        .unwrap();
    let resumed = f
        .store
        .service_post_process_jobs(PID, f.epoch, &PostProcessJobsRequest::new(f.proof.clone()))
        .unwrap()
        .items;
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].input, ready.lease.input);
    assert!(matches!(
        resumed[0].stage,
        PostProcessStage::SonioxReady { cleanup: false, .. }
    ));
}

#[test]
fn completion_requires_ready_cleanup_and_pins_historical_summary_evidence() {
    let mut f = fixture(CaptureProvider::Local);
    assert!(matches!(
        finish(&f, &completion(&f)),
        Err(DataError::Forbidden)
    ));
    ready(&mut f, false);
    assert!(matches!(
        finish(&f, &completion(&f)),
        Err(DataError::Forbidden)
    ));

    // Even an accepted no-op is durable cleanup work for this checkpoint.
    let (upload_id, _, _) = stage(&f, "[]");
    f.lease = committed(
        call(
            &f,
            &request(&f, PostProcessCleanupOperation::Commit { upload_id }),
        )
        .unwrap(),
    )
    .lease;
    let mut forged_expiry = completion(&f);
    forged_expiry.lease.expires_at += Duration::milliseconds(1);
    assert!(matches!(
        finish(&f, &forged_expiry),
        Err(DataError::Forbidden)
    ));
    let complete = completion(&f);
    let expected_summary = "historical-summary-revision";
    let meeting = f.lease.meeting_id.to_string();
    let (payload, before) = {
        let conn = f.store.conn.lock();
        let before = postprocess::candidate_on(&conn, &f.lease.job_id).unwrap();
        // Exercise evidence pinning independently of the summary publisher.
        conn.execute(
            "INSERT INTO service_post_process_generated_summaries
             VALUES(?1,?2,'other-revision',?3,'template-id','template-rev','template-gen')",
            params![f.lease.job_id, f.lease.input.source_id, expected_summary],
        )
        .unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM service_jobs WHERE id=?1",
                [&f.lease.job_id],
                |row| row.get(0),
            )
            .unwrap();
        (payload, before)
    };
    assert!(matches!(finish(&f, &complete), Err(DataError::Forbidden)));
    {
        let conn = f.store.conn.lock();
        conn.execute(
            "DELETE FROM service_post_process_generated_summaries WHERE job_id=?1",
            [&f.lease.job_id],
        )
        .unwrap();
        assert_eq!(
            postprocess::candidate_on(&conn, &f.lease.job_id).unwrap(),
            before
        );
        conn.execute(
            "INSERT INTO service_post_process_generated_summaries
             VALUES(?1,?2,?3,?4,'template-id','template-rev','template-gen')",
            params![
                f.lease.job_id,
                f.lease.input.source_id,
                f.lease.input.transcript_revision,
                expected_summary
            ],
        )
        .unwrap();
    }
    let before_meeting = serde_json::to_value(f.store.service_meeting(&meeting).unwrap()).unwrap();
    let receipt = finish(&f, &complete).unwrap();
    assert_eq!(receipt.cleanup, FixtureCleanupOutcome::Accepted);
    assert_eq!(
        receipt.summary,
        FixtureSummaryOutcome::Committed {
            revision: expected_summary.into()
        }
    );
    assert_eq!(receipt.input, f.lease.input);
    assert_eq!(finish(&f, &complete).unwrap(), receipt);
    assert_eq!(
        serde_json::to_value(f.store.service_meeting(&meeting).unwrap()).unwrap(),
        before_meeting
    );
    assert!(matches!(
        finish(&f, &completion(&f)),
        Err(DataError::Forbidden)
    ));
    assert!(!f
        .store
        .service_has_live_post_process_claims(f.epoch)
        .unwrap());
    assert!(f
        .store
        .service_post_process_jobs(PID, f.epoch, &PostProcessJobsRequest::new(f.proof.clone()))
        .unwrap()
        .items
        .is_empty());

    let mut changed = complete.clone();
    changed.fence = WorkerSecret::random();
    assert!(matches!(
        finish(&f, &changed),
        Err(DataError::IdempotencyConflict)
    ));
    assert!(matches!(
        f.store
            .service_complete_fixture_post_process(PID + 1, f.epoch, &complete),
        Err(DataError::Forbidden)
    ));
    let conn = f.store.conn.lock();
    let terminal: (String, String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT state,payload_json,claim_token,lease_until FROM service_jobs WHERE id=?1",
            [&f.lease.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(terminal, ("succeeded".into(), payload, None, None));
    let checkpoint: (String, String) = conn
        .query_row(
            "SELECT state,revision FROM service_post_process_checkpoints WHERE job_id=?1",
            [&f.lease.job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(checkpoint.0, "completed");
    assert_ne!(checkpoint.1, f.lease.input.checkpoint_revision);
    let stored: String = conn
        .query_row(
            "SELECT outcome_json FROM service_post_process_completions WHERE job_id=?1",
            [&f.lease.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
        serde_json::json!({
            "cleanup":"accepted",
            "embeddings":"outside_fixture_scope",
            "summary":{"kind":"committed","revision":expected_summary},
            "webhook":"outside_fixture_scope"
        })
    );
    assert!(conn
        .execute(
            "UPDATE service_post_process_completions SET outcome_json='{}'",
            []
        )
        .is_err());
}

#[test]
fn empty_completion_checks_both_projections_and_rolls_back_late_failure() {
    let mut f = fixture(CaptureProvider::Local);
    ready(&mut f, true);
    let complete = completion(&f);
    let meeting = f.lease.meeting_id.to_string();
    {
        let conn = f.store.conn.lock();
        conn.execute(
            "INSERT INTO segments(meeting_id,start_ms,end_ms,text) VALUES(?1,0,100,'stale native row')",
            [&meeting],
        ).unwrap();
    }
    assert!(matches!(finish(&f, &complete), Err(DataError::Forbidden)));
    {
        let conn = f.store.conn.lock();
        conn.execute("DELETE FROM segments WHERE meeting_id=?1", [&meeting])
            .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_completion_receipt BEFORE INSERT ON service_idempotency
             WHEN NEW.tool='complete_fixture_post_process'
             BEGIN SELECT RAISE(ABORT,'fixture write fault'); END;",
        )
        .unwrap();
    }
    let (upload_id, _, _) = stage(&f, "[]");
    assert!(matches!(finish(&f, &complete), Err(DataError::Sqlite(_))));
    let upload_state = || {
        let conn = f.store.conn.lock();
        conn.query_row(
            "SELECT u.state,c.text FROM service_post_process_cleanup_uploads u
             JOIN service_post_process_cleanup_chunks c ON c.upload_id=u.id WHERE u.id=?1",
            [upload_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .unwrap()
    };
    assert_eq!(upload_state(), ("open".into(), Some("[]".into())));
    {
        let conn = f.store.conn.lock();
        postprocess::check_binding(&conn, &f.proof, &f.lease, &f.fence).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM service_post_process_completions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        conn.execute_batch("DROP TRIGGER reject_completion_receipt")
            .unwrap();
    }
    let receipt = finish(&f, &complete).unwrap();
    assert_eq!(
        receipt.cleanup,
        FixtureCleanupOutcome::SkippedEmptyTranscript
    );
    assert_eq!(
        receipt.summary,
        FixtureSummaryOutcome::OutsideFixtureScope {}
    );
    assert_eq!(upload_state(), ("invalidated".into(), None));
    assert!(matches!(
        call(
            &f,
            &request(&f, PostProcessCleanupOperation::Commit { upload_id })
        ),
        Err(DataError::Forbidden)
    ));
}

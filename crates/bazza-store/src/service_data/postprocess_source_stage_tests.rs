use super::*;

#[test]
fn retained_live_rows_are_already_the_source_and_stay_exact_through_ready() {
    let f = fixture_with_captured_rows(
        Store::open_in_memory().unwrap(),
        CaptureProvider::Local,
        true,
        true,
    );
    let id = f.lease.meeting_id.to_string();
    let original = f.store.segments(&id).unwrap();
    assert_eq!(original[0].speaker.as_deref(), Some("Анна"));
    assert_eq!(original[1].speaker, None);
    assert_eq!(
        (&*original[1].text, original[1].start_ms, original[1].end_ms),
        ("raw source 1", 300, 600)
    );
    assert_eq!(
        postprocess::candidate_on(&f.store.conn.lock(), &f.lease.job_id)
            .unwrap()
            .stage,
        PostProcessStage::CapturedLive
    );

    let body = r#"{"kind":"skipped","reason":"disabled"}"#;
    let upload_id = Uuid::new_v4();
    let digest = sha(body.as_bytes());
    let begin = request(
        &f,
        PostProcessCleanupOperation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: digest.clone(),
        },
    );
    assert!(matches!(
        f.store.service_post_process_source(PID, f.epoch, &begin),
        Err(DataError::Forbidden)
    ));
    f.store
        .service_post_process_speakers(PID, f.epoch, &begin)
        .unwrap();
    let append = request(
        &f,
        PostProcessCleanupOperation::Append {
            upload_id,
            sequence: 1,
            byte_offset: 0,
            text: body.into(),
            sha256: digest,
        },
    );
    f.store
        .service_post_process_speakers(PID, f.epoch, &append)
        .unwrap();
    let commit = request(&f, PostProcessCleanupOperation::Commit { upload_id });
    let PostProcessSpeakersReply::Committed(receipt) = f
        .store
        .service_post_process_speakers(PID, f.epoch, &commit)
        .unwrap()
    else {
        panic!("expected speakers receipt");
    };
    assert_eq!(receipt.changed_segments, 0);
    let ready = f
        .store
        .service_mark_post_process_ready(
            PID,
            f.epoch,
            &PostProcessReadyRequest {
                proof: f.proof.clone(),
                lease: receipt.lease,
                fence: f.fence.clone(),
                operation_id: Uuid::new_v4(),
            },
        )
        .unwrap();
    assert_eq!(ready.lease.input.source_id, f.lease.input.source_id);
    assert_eq!(
        ready.lease.input.transcript_revision,
        f.lease.input.transcript_revision
    );
    let rows = |items: &[bazza_domain::Segment]| {
        items
            .iter()
            .map(|row| {
                (
                    row.id,
                    row.start_ms,
                    row.end_ms,
                    row.speaker.clone(),
                    row.text.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&f.store.segments(&id).unwrap()), rows(&original));
    assert!(matches!(
        postprocess::candidate_on(&f.store.conn.lock(), &f.lease.job_id)
            .unwrap()
            .stage,
        PostProcessStage::Ready { .. }
    ));
}

#[test]
fn captured_live_diarization_changes_only_unnamed_rows() {
    for named in [false, true] {
        let f = fixture_with_captured_rows(Store::open_in_memory().unwrap(), CaptureProvider::Local, true, named);
        let id = f.lease.meeting_id.to_string();
        let original = f.store.segments(&id).unwrap();
        let assignments: Vec<_> = original.iter().map(|row| {
            serde_json::json!({"segment_id": row.id, "cluster": 0})
        }).collect();
        let body = serde_json::json!({"kind": "applied", "assignments": assignments}).to_string();
        let digest = sha(body.as_bytes());
        let upload_id = Uuid::new_v4();
        for operation in [
            PostProcessCleanupOperation::Begin { upload_id, total_bytes: body.len() as u64, sha256: digest.clone() },
            PostProcessCleanupOperation::Append { upload_id, sequence: 1, byte_offset: 0, text: body, sha256: digest },
        ] {
            f.store.service_post_process_speakers(PID, f.epoch, &request(&f, operation)).unwrap();
        }
        let result = f.store.service_post_process_speakers(PID, f.epoch, &request(&f, PostProcessCleanupOperation::Commit { upload_id }));
        if named {
            assert!(matches!(result, Err(DataError::Forbidden)));
        } else {
            let PostProcessSpeakersReply::Committed(receipt) = result.unwrap() else { panic!("expected speakers receipt") };
            assert_eq!(receipt.changed_segments as usize, original.len());
            assert_eq!(receipt.lease.input.source_id, f.lease.input.source_id);
        }
        for (before, after) in original.iter().zip(f.store.segments(&id).unwrap()) {
            assert_eq!((after.id, after.start_ms, after.end_ms, &after.text), (before.id, before.start_ms, before.end_ms, &before.text));
            assert_eq!(after.speaker.as_deref(), if named { before.speaker.as_deref() } else { Some("Speaker 1") });
        }
    }
}

#[test]
fn local_source_and_cleanup_share_quota_but_cannot_adopt_uploads() {
    let f = fixture(CaptureProvider::Local);
    let source = |request: &PostProcessCleanupRequest| {
        f.store.service_post_process_source(PID, f.epoch, request)
    };
    let body = r#"{"model":"fixture-actual","language":"en","outcome":"complete","captions":[]}"#;
    let upload_id = Uuid::new_v4();
    let begin = request(
        &f,
        PostProcessCleanupOperation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: sha(body.as_bytes()),
        },
    );
    source(&begin).unwrap();
    assert!(matches!(call(&f, &begin), Err(DataError::Forbidden)));

    let append = request(
        &f,
        PostProcessCleanupOperation::Append {
            upload_id,
            sequence: 1,
            byte_offset: 0,
            text: body.into(),
            sha256: sha(body.as_bytes()),
        },
    );
    source(&append).unwrap();
    assert!(matches!(call(&f, &append), Err(DataError::Forbidden)));

    let cleanup = super::body(&[(f.ids[0], "stale overlay")]);
    let (sibling, _, _) = stage(&f, &cleanup);
    let siblings = usize::from(MAX_POST_PROCESS_CLEANUP_OPEN_UPLOADS) - 2;
    for _ in 0..siblings {
        stage(&f, "[]");
    }
    let mut exhausted = begin.clone();
    exhausted.operation_id = Uuid::new_v4();
    exhausted.operation = PostProcessCleanupOperation::Begin {
        upload_id: Uuid::new_v4(),
        total_bytes: body.len() as u64,
        sha256: sha(body.as_bytes()),
    };
    assert!(matches!(source(&exhausted), Err(DataError::Busy)));

    let commit = request(&f, PostProcessCleanupOperation::Commit { upload_id });
    let receipt = source(&commit).unwrap();
    assert!(matches!(receipt, PostProcessSourceReply::Committed(_)));
    assert_eq!(source(&commit).unwrap(), receipt);
    let conn = f.store.conn.lock();
    assert_eq!(
        postprocess::candidate_on(&conn, &f.lease.job_id)
            .unwrap()
            .stage,
        PostProcessStage::LocalAsr {
            outcome: bazza_domain::local_asr::LocalAsrOutcome::Complete
        }
    );
    let state: (String, Option<String>) = conn
        .query_row(
            "SELECT u.state,c.text FROM service_post_process_cleanup_uploads u
         JOIN service_post_process_cleanup_chunks c ON c.upload_id=u.id WHERE u.id=?1",
            [sibling.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, ("invalidated".into(), None));
    let live_bodies: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM service_post_process_cleanup_chunks WHERE text IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live_bodies, 0);
}

#[test]
fn ready_rolls_back_lifecycle_and_claim_if_receipt_write_fails() {
    let f = fixture(CaptureProvider::Local);
    let publish = |lease: &PostProcessJobLease, body: &str, speakers: bool| {
        let upload_id = Uuid::new_v4();
        let digest = sha(body.as_bytes());
        for op in [
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
            let mut req = request(&f, op);
            req.lease = lease.clone();
            if speakers {
                f.store
                    .service_post_process_speakers(PID, f.epoch, &req)
                    .unwrap();
            } else {
                f.store
                    .service_post_process_source(PID, f.epoch, &req)
                    .unwrap();
            }
        }
        let mut req = request(&f, PostProcessCleanupOperation::Commit { upload_id });
        req.lease = lease.clone();
        if speakers {
            let PostProcessSpeakersReply::Committed(receipt) = f
                .store
                .service_post_process_speakers(PID, f.epoch, &req)
                .unwrap()
            else {
                panic!("expected speakers commit")
            };
            receipt.lease
        } else {
            let PostProcessSourceReply::Committed(receipt) = f
                .store
                .service_post_process_source(PID, f.epoch, &req)
                .unwrap()
            else {
                panic!("expected source commit")
            };
            receipt.lease
        }
    };
    let source = publish(
        &f.lease,
        r#"{"model":"fixture","language":null,"outcome":"complete","captions":[]}"#,
        false,
    );
    let speakers = publish(
        &source,
        r#"{"kind":"skipped","reason":"empty_transcript"}"#,
        true,
    );
    let request = PostProcessReadyRequest {
        proof: f.proof.clone(),
        lease: speakers.clone(),
        fence: f.fence.clone(),
        operation_id: Uuid::new_v4(),
    };
    let before = {
        let conn = f.store.conn.lock();
        conn.execute_batch(
            "CREATE TRIGGER reject_ready_receipt BEFORE INSERT ON service_idempotency
             WHEN NEW.tool='post_process_ready'
             BEGIN SELECT RAISE(ABORT,'fixture write fault'); END;",
        )
        .unwrap();
        postprocess::candidate_on(&conn, &speakers.job_id).unwrap()
    };
    assert!(matches!(
        f.store
            .service_mark_post_process_ready(PID, f.epoch, &request),
        Err(DataError::Sqlite(_))
    ));
    {
        let conn = f.store.conn.lock();
        assert_eq!(
            postprocess::candidate_on(&conn, &speakers.job_id).unwrap(),
            before
        );
        postprocess::check_binding(&conn, &f.proof, &speakers, &f.fence).unwrap();
        conn.execute_batch("DROP TRIGGER reject_ready_receipt;")
            .unwrap();
    }
    let ready = f
        .store
        .service_mark_post_process_ready(PID, f.epoch, &request)
        .unwrap();
    assert_ne!(
        ready.lease.input.lifecycle_revision,
        speakers.input.lifecycle_revision
    );
    assert_eq!(
        f.store
            .service_mark_post_process_ready(PID, f.epoch, &request)
            .unwrap(),
        ready
    );
}

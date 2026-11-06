//! Synthetic Store-only whole-result checkpoints. No inference or media access.
use super::*;
use rusqlite::types::Value;

#[path = "postprocess_complete_tests.rs"]
mod complete;

#[path = "postprocess_source_stage_tests.rs"]
mod source_stage;

const PID: u32 = 42;
struct Fixture {
    store: Store,
    epoch: Uuid,
    proof: WorkerProof,
    lease: PostProcessJobLease,
    fence: WorkerSecret,
    ids: Vec<i64>,
}
fn fixture_on(store: Store, provider: CaptureProvider) -> Fixture {
    fixture_with_captured_rows(store, provider, false, true)
}

fn fixture_with_captured_rows(
    store: Store,
    provider: CaptureProvider,
    retain_local_live: bool,
    named: bool,
) -> Fixture {
    let epoch = Uuid::new_v4();
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities: vec![WorkerCapability::Capture, WorkerCapability::PostProcess],
    };
    store
        .service_register_worker(PID, epoch, &registration, 300)
        .unwrap();
    let proof = WorkerProof {
        session_id: registration.session_id,
        secret: registration.secret,
        library_epoch: epoch,
    };
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Cleanup fixture".into(),
        app: None,
        started_at: Utc::now() - Duration::seconds(2),
        participants: vec![],
        provider: CaptureSelection {
            provider,
            model: Some("fixture-model".into()),
            language: Some("ru".into()),
        },
    };
    let capture = store
        .service_prepare_capture(PID, epoch, &proof, &start)
        .unwrap();
    store
        .service_begin_capture(PID, epoch, &proof, &capture, &start.fence)
        .unwrap();
    store
        .service_finalize_capture(
            PID,
            epoch,
            &proof,
            &capture,
            &start.fence,
            &CaptureTerminal {
                raw_screen: None,
                soniox_outcome: None,
                retain_local_live,
                status: CaptureTerminalStatus::Completed,
                provider: start.provider,
                duration_ms: 1000,
                ended_at: start.started_at + Duration::seconds(1),
                has_video: false,
                last_confirmed_sequence: 3,
                captions: (0..3)
                    .map(|index| ConfirmedCaption {
                        sequence: index + 1,
                        start_ms: index as i64 * 300,
                        end_ms: index as i64 * 300 + 300,
                        speaker: (named && (!retain_local_live || index != 1)).then(|| "Анна".into()),
                        text: format!("raw source {index}"),
                    })
                    .collect(),
            },
        )
        .unwrap();
    let job = store
        .service_post_process_jobs(PID, epoch, &PostProcessJobsRequest::new(proof.clone()))
        .unwrap()
        .items
        .remove(0);
    let fence = WorkerSecret::random();
    let lease = store
        .service_claim_post_process_job(
            PID,
            epoch,
            &ClaimPostProcessJobRequest {
                proof: proof.clone(),
                job,
                claim_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                fence: fence.clone(),
                ttl_seconds: 120,
            },
        )
        .unwrap();
    let ids = store
        .segments(&lease.meeting_id.to_string())
        .unwrap()
        .into_iter()
        .map(|segment| segment.id)
        .collect();
    Fixture {
        store,
        epoch,
        proof,
        lease,
        fence,
        ids,
    }
}
fn fixture(provider: CaptureProvider) -> Fixture {
    fixture_on(Store::open_in_memory().unwrap(), provider)
}
fn request(f: &Fixture, operation: PostProcessCleanupOperation) -> PostProcessCleanupRequest {
    PostProcessCleanupRequest {
        proof: f.proof.clone(),
        lease: f.lease.clone(),
        fence: f.fence.clone(),
        operation_id: Uuid::new_v4(),
        operation,
    }
}
fn call(f: &Fixture, request: &PostProcessCleanupRequest) -> DataResult<PostProcessCleanupReply> {
    f.store.service_post_process_cleanup(PID, f.epoch, request)
}
fn body(updates: &[(i64, &str)]) -> String {
    serde_json::to_string(
        &updates
            .iter()
            .map(|(segment, text)| PostProcessCleanupUpdate {
                segment_id: *segment,
                text: (*text).into(),
            })
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn stage(
    f: &Fixture,
    text: &str,
) -> (
    Uuid,
    PostProcessCleanupRequest,
    Vec<PostProcessCleanupRequest>,
) {
    let id = Uuid::new_v4();
    let begin = request(
        f,
        PostProcessCleanupOperation::Begin {
            upload_id: id,
            total_bytes: text.len() as u64,
            sha256: sha(text.as_bytes()),
        },
    );
    call(f, &begin).unwrap();
    let mut offset = 0;
    let mut appends = Vec::new();
    while offset < text.len() {
        let mut end = (offset + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let fragment = &text[offset..end];
        let append = request(
            f,
            PostProcessCleanupOperation::Append {
                upload_id: id,
                sequence: appends.len() as u64 + 1,
                byte_offset: offset as u64,
                text: fragment.into(),
                sha256: sha(fragment.as_bytes()),
            },
        );
        call(f, &append).unwrap();
        appends.push(append);
        offset = end;
    }
    (id, begin, appends)
}
fn committed(reply: PostProcessCleanupReply) -> PostProcessCleanupReceipt {
    match reply {
        PostProcessCleanupReply::Committed(receipt) => receipt,
        other => panic!("expected committed cleanup: {other:?}"),
    }
}
fn payload_count(store: &Store, id: Uuid) -> i64 {
    store.conn.lock().query_row("SELECT COUNT(*) FROM service_post_process_cleanup_chunks WHERE upload_id=?1 AND text IS NOT NULL",[id.to_string()],|row|row.get(0)).unwrap()
}
fn database(store: &Store) -> Vec<(String, Vec<Vec<Value>>)> {
    let conn = store.conn.lock();
    let tables = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|name| {
            let sql = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
            let columns = conn.prepare(&sql).unwrap().column_count();
            let order = (1..=columns)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let rows = conn
                .prepare(&format!("{sql} ORDER BY {order}"))
                .unwrap()
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<Vec<Value>>>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

#[test]
fn cleanup_is_atomic_preserves_soniox_raw_caches_profiles_and_manual_empty() {
    let f = fixture(CaptureProvider::Soniox);
    let id = f.lease.meeting_id.to_string();
    let source = f.lease.input.source_id.as_ref().unwrap();
    {
        // Pre-existing manual overlay fixture, including an intentionally empty
        // value. Production manual writes still retain their Processing guard.
        let conn = f.store.conn.lock();
        conn.execute(
            "UPDATE segments SET clean_text='',clean_text_manual=1 WHERE id=?1",
            [f.ids[1]],
        )
        .unwrap();
        conn.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,'',1)",params![source,f.ids[1].to_string()]).unwrap();
    }
    let keys = [
        format!("speaker_embeddings:{id}"),
        format!("speaker_embeddings:policy-v2:{id}"),
        format!("speaker_embeddings:other-policy:{id}"),
        "private:provider_key".into(),
    ];
    for key in &keys {
        f.store.set_setting(key, &"preserved cache").unwrap();
    }
    let speaker = crate::Speaker {
        id: Uuid::new_v4().to_string(),
        name: "Saved voice".into(),
        embedding: vec![1.0, 0.0],
    };
    f.store.upsert_speaker(&speaker).unwrap();
    f.store
        .add_voice_sample(
            &Uuid::new_v4().to_string(),
            &crate::NewVoiceSample {
                speaker_id: &speaker.id,
                embedding: &[0.0, 1.0],
                model_id: crate::CURRENT_VOICE_MODEL,
                source: "file",
                source_label: "Synthetic sample",
                meeting_id: None,
                start_ms: Some(0),
                end_ms: Some(1000),
                has_audio: false,
            },
        )
        .unwrap();
    let profiles = f.store.list_voice_profiles().unwrap();
    let samples = f.store.list_voice_samples(&speaker.id).unwrap();
    let stt = f
        .store
        .get_setting::<serde_json::Value>(&format!("stt:{id}"))
        .unwrap();
    assert_eq!(stt.as_ref().unwrap()["provider"], "soniox");
    let raw = f
        .store
        .segments(&id)
        .unwrap()
        .into_iter()
        .map(|segment| {
            (
                segment.id,
                segment.text,
                segment.start_ms,
                segment.end_ms,
                segment.speaker,
            )
        })
        .collect::<Vec<_>>();
    let capture_before:(String,String,String) = f.store.conn.lock().query_row("SELECT provider_json,outcome_json,finalize_hash FROM service_capture_leases WHERE id=?1",[f.lease.input.capture_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    let text = body(&[
        (f.ids[0], "discard duplicate"),
        (f.ids[1], "never overwrite manual"),
        (f.ids[0], ""),
        (f.ids[2], "  Орион\0🙂  "),
        (9_000_000_000_000_000, "foreign id ignored"),
    ]);
    let (upload, begin, appends) = stage(&f, &text);
    assert_eq!(
        f.store.segments(&id).unwrap()[0].clean_text,
        None,
        "staging is invisible"
    );
    let commit = request(
        &f,
        PostProcessCleanupOperation::Commit { upload_id: upload },
    );
    let result = committed(call(&f, &commit).unwrap());
    assert_eq!(result.changed_segments, 2);
    assert_eq!(result.outcome, PostProcessCleanupOutcome::Applied);
    assert_ne!(
        result.lease.input.checkpoint_revision,
        f.lease.input.checkpoint_revision
    );
    assert_ne!(result.lease.job_revision, f.lease.job_revision);
    assert_eq!(result.lease.expires_at, f.lease.expires_at);
    assert_eq!(result.lease.claim_id, f.lease.claim_id);
    assert_eq!(payload_count(&f.store, upload), 0);
    let segments = f.store.segments(&id).unwrap();
    assert_eq!(segments[0].clean_text.as_deref(), Some(""));
    assert!(!segments[0].clean_text_manual);
    assert_eq!(segments[1].clean_text.as_deref(), Some(""));
    assert!(segments[1].clean_text_manual);
    assert_eq!(segments[2].clean_text.as_deref(), Some("  Орион\0🙂  "));
    assert_eq!(
        segments
            .into_iter()
            .map(|segment| (
                segment.id,
                segment.text,
                segment.start_ms,
                segment.end_ms,
                segment.speaker
            ))
            .collect::<Vec<_>>(),
        raw
    );
    assert_eq!(
        f.store
            .get_setting::<serde_json::Value>(&format!("stt:{id}"))
            .unwrap(),
        stt
    );
    for key in &keys {
        assert_eq!(
            f.store.get_setting::<String>(key).unwrap().as_deref(),
            Some("preserved cache")
        );
    }
    assert_eq!(f.store.list_voice_profiles().unwrap(), profiles);
    assert_eq!(f.store.list_voice_samples(&speaker.id).unwrap(), samples);
    let capture_after:(String,String,String)=f.store.conn.lock().query_row("SELECT provider_json,outcome_json,finalize_hash FROM service_capture_leases WHERE id=?1",[f.lease.input.capture_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(capture_after, capture_before);
    let before = database(&f.store);
    assert_eq!(
        committed(call(&f, &commit).unwrap()),
        result,
        "lost ACK remains historical"
    );
    assert!(
        matches!(call(&f,&begin).unwrap(),PostProcessCleanupReply::Upload(status) if status.received_bytes==0 &&status.state==PostProcessCleanupUploadState::Open)
    );
    assert!(
        matches!(call(&f,&appends[0]).unwrap(),PostProcessCleanupReply::Upload(status) if status.state==PostProcessCleanupUploadState::Open)
    );
    assert_eq!(database(&f.store), before);
    let page = f
        .store
        .service_desktop_transcript(&DesktopTranscriptRequest {
            meeting_id: id,
            source_id: None,
            expected_revision: result.lease.input.transcript_revision,
            representation: DesktopTranscriptRepresentation::Effective,
            cursor: None,
            limit: 100,
            max_bytes: 65024,
        })
        .unwrap();
    assert_eq!(page.items[0].text, "");
    assert_eq!(
        page.items[0].effective_origin,
        DesktopEffectiveOrigin::Automatic
    );
    assert_eq!(page.items[1].text, "");
    assert_eq!(
        page.items[1].effective_origin,
        DesktopEffectiveOrigin::Manual
    );
}

#[test]
fn duplicates_noop_and_divergent_automatic_projection_have_truthful_effects() {
    for divergent in [false, true] {
        let f = fixture(CaptureProvider::Local);
        let id = f.lease.meeting_id.to_string();
        let source = f.lease.input.source_id.as_ref().unwrap();
        f.store
            .conn
            .lock()
            .execute(
                "UPDATE segments SET clean_text='desired' WHERE id=?1",
                [f.ids[0]],
            )
            .unwrap();
        f.store.conn.lock().execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,?3,0)",params![source,f.ids[0].to_string(),if divergent{"old canonical"}else{"desired"}]).unwrap();
        let (upload, _, _) = stage(
            &f,
            &body(&[(f.ids[0], "first ignored duplicate"), (f.ids[0], "desired")]),
        );
        let result = committed(
            call(
                &f,
                &request(
                    &f,
                    PostProcessCleanupOperation::Commit { upload_id: upload },
                ),
            )
            .unwrap(),
        );
        assert_eq!(result.changed_segments, u32::from(divergent));
        assert_eq!(
            result.outcome,
            if divergent {
                PostProcessCleanupOutcome::Applied
            } else {
                PostProcessCleanupOutcome::Noop
            }
        );
        assert_eq!(
            result.lease.input.transcript_revision == f.lease.input.transcript_revision,
            !divergent
        );
        assert_ne!(
            result.lease.input.checkpoint_revision,
            f.lease.input.checkpoint_revision
        );
        assert_eq!(f.store.conn.lock().query_row("SELECT clean_text FROM service_transcript_overlays WHERE source_id=?1 AND segment_key=?2",params![source,f.ids[0].to_string()],|row|row.get::<_,String>(0)).unwrap(),"desired");
        assert_eq!(f.store.service_job(&f.lease.job_id).unwrap().attempts, 1);
        let mut next = request(
            &f,
            PostProcessCleanupOperation::Begin {
                upload_id: Uuid::new_v4(),
                total_bytes: 2,
                sha256: sha(b"[]"),
            },
        );
        next.lease = result.lease;
        assert!(
            matches!(call(&f, &next), Err(DataError::Forbidden)),
            "one accepted cleanup per job"
        );
        assert!(f.store.get_meeting(&id).unwrap().is_some());
    }
}

#[test]
fn malformed_digest_and_late_commit_failure_roll_back_projection_checkpoint_and_receipt() {
    for fault in 0..4 {
        let f = fixture(CaptureProvider::Local);
        let text = match fault {
            0 => "[{\"segment_id\":1,\"text\":\"x\",\"unknown\":true}]".into(),
            1 => "not JSON".into(),
            _ => body(&[(f.ids[0], "changed")]),
        };
        let (upload, _, _) = stage(&f, &text);
        if fault == 2 {
            f.store
                .conn
                .lock()
                .execute(
                    "UPDATE service_post_process_cleanup_uploads SET sha256=?2 WHERE id=?1",
                    params![upload.to_string(), "0".repeat(64)],
                )
                .unwrap();
        }
        if fault == 3 {
            f.store.conn.lock().execute_batch("CREATE TRIGGER fail_cleanup_job BEFORE UPDATE OF claim_token ON service_jobs BEGIN SELECT RAISE(ABORT,'synthetic cleanup failure'); END;").unwrap();
        }
        let commit = request(
            &f,
            PostProcessCleanupOperation::Commit { upload_id: upload },
        );
        let before = database(&f.store);
        assert!(call(&f, &commit).is_err());
        assert_eq!(database(&f.store), before);
        assert!(payload_count(&f.store, upload) > 0);
    }
}

#[test]
fn permanent_erasure_scrubs_all_input_uploads_and_rolls_back_atomically_on_failure() {
    for meeting in [false, true] {
        let f = fixture(CaptureProvider::Local);
        let (first, _, _) = stage(&f, &body(&[(f.ids[0], "private candidate one")]));
        let (second, _, _) = stage(&f, &body(&[(f.ids[0], "private candidate two")]));
        f.store
            .service_fail_post_process_job(
                PID,
                f.epoch,
                &FailPostProcessJobRequest {
                    proof: f.proof.clone(),
                    lease: f.lease.clone(),
                    operation_id: Uuid::new_v4(),
                    fence: f.fence.clone(),
                    error_code: PostProcessFailureCode::PostProcessFailed,
                },
            )
            .unwrap();
        let id = f.lease.meeting_id.to_string();
        let live = f.store.service_meeting(&id).unwrap().unwrap();
        let context = MutationContext {
            principal: "fixture-user".into(),
            idempotency_key: "erase-input".into(),
        };
        let erase = || {
            if meeting {
                f.store
                    .service_delete_meeting(&context, &id, &live.lifecycle_revision)
            } else {
                f.store
                    .service_delete_transcript(&context, &id, &f.lease.input.transcript_revision)
            }
        };
        f.store.conn.lock().execute_batch("CREATE TRIGGER fail_cleanup_erasure BEFORE INSERT ON service_changes WHEN new.entity_kind IN ('transcript','meeting_lifecycle') BEGIN SELECT RAISE(ABORT,'synthetic erase failure'); END;").unwrap();
        let before = database(&f.store);
        assert!(erase().is_err());
        assert_eq!(database(&f.store), before);
        f.store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER fail_cleanup_erasure")
            .unwrap();
        erase().unwrap();
        for upload in [first, second] {
            assert_eq!(payload_count(&f.store, upload), 0);
            assert_eq!(
                upload_on(&f.store.conn.lock(), upload)
                    .unwrap()
                    .status
                    .state,
                PostProcessCleanupUploadState::Invalidated
            );
        }
        assert_eq!(
            f.store
                .conn
                .lock()
                .query_row(
                    "SELECT state FROM service_post_process_checkpoints WHERE job_id=?1",
                    [&f.lease.job_id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "invalidated"
        );
        assert!(matches!(
            call(
                &f,
                &request(&f, PostProcessCleanupOperation::Commit { upload_id: first })
            ),
            Err(DataError::Forbidden)
        ));
    }
}

#[test]
fn accepted_checkpoint_scrubs_other_stages_and_canonical_manual_empty_cannot_be_overwritten() {
    let f = fixture(CaptureProvider::Local);
    let source = f.lease.input.source_id.as_ref().unwrap();
    // Even a divergent legacy flag must never overwrite canonical manual text.
    f.store.conn.lock().execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,'',1)",params![source,f.ids[0].to_string()]).unwrap();
    let (stale, _, _) = stage(&f, &body(&[(f.ids[1], "obsolete candidate")]));
    let (accepted, _, _) = stage(&f, &body(&[(f.ids[0], "cannot overwrite manual")]));
    let receipt = committed(
        call(
            &f,
            &request(
                &f,
                PostProcessCleanupOperation::Commit {
                    upload_id: accepted,
                },
            ),
        )
        .unwrap(),
    );
    assert_eq!(receipt.outcome, PostProcessCleanupOutcome::Noop);
    assert_eq!(receipt.changed_segments, 0);
    assert_eq!(payload_count(&f.store, stale), 0);
    assert_eq!(
        upload_on(&f.store.conn.lock(), stale).unwrap().status.state,
        PostProcessCleanupUploadState::Invalidated
    );
    assert_eq!(f.store.conn.lock().query_row("SELECT clean_text FROM service_transcript_overlays WHERE source_id=?1 AND segment_key=?2",params![source,f.ids[0].to_string()],|row|row.get::<_,String>(0)).unwrap(),"");
    assert_eq!(
        f.store.segments(&f.lease.meeting_id.to_string()).unwrap()[0].clean_text,
        None
    );
}

#[test]
fn partial_staged_utf8_body_survives_reopen_without_visible_partial_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("bazza.db");
    let mut f = fixture_on(Store::open(&path).unwrap(), CaptureProvider::Local);
    let text = body(&[(f.ids[0], &"Орион\0🙂".repeat(1000))]);
    let upload = Uuid::new_v4();
    call(
        &f,
        &request(
            &f,
            PostProcessCleanupOperation::Begin {
                upload_id: upload,
                total_bytes: text.len() as u64,
                sha256: sha(text.as_bytes()),
            },
        ),
    )
    .unwrap();
    let split = text
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= 2048)
        .unwrap();
    let first = request(
        &f,
        PostProcessCleanupOperation::Append {
            upload_id: upload,
            sequence: 1,
            byte_offset: 0,
            text: text[..split].into(),
            sha256: sha(&text.as_bytes()[..split]),
        },
    );
    call(&f, &first).unwrap();
    drop(f.store);
    f.store = Store::open(&path).unwrap();
    assert_eq!(
        f.store.segments(&f.lease.meeting_id.to_string()).unwrap()[0].clean_text,
        None
    );
    call(&f, &first).unwrap();
    let rest = &text[split..];
    call(
        &f,
        &request(
            &f,
            PostProcessCleanupOperation::Append {
                upload_id: upload,
                sequence: 2,
                byte_offset: split as u64,
                text: rest.into(),
                sha256: sha(rest.as_bytes()),
            },
        ),
    )
    .unwrap();
    let result = committed(
        call(
            &f,
            &request(
                &f,
                PostProcessCleanupOperation::Commit { upload_id: upload },
            ),
        )
        .unwrap(),
    );
    assert_eq!(result.changed_segments, 1);
    assert_eq!(
        f.store.segments(&f.lease.meeting_id.to_string()).unwrap()[0]
            .clean_text
            .as_deref(),
        Some("Орион\0🙂".repeat(1000).as_str())
    );
}

#[test]
fn damaged_stored_counts_and_mid_utf8_chunk_refuse_commit_before_body_allocation() {
    for fault in 0..4 {
        let f = fixture(CaptureProvider::Local);
        let (upload, _, _) = stage(&f, &body(&[(f.ids[0], "candidate🙂")]));
        let conn = f.store.conn.lock();
        match fault {
            0 => {
                conn.pragma_update(None, "ignore_check_constraints", true)
                    .unwrap();
                conn.execute(
                    "UPDATE service_post_process_cleanup_uploads SET total_bytes=?2 WHERE id=?1",
                    params![
                        upload.to_string(),
                        (MAX_POST_PROCESS_CLEANUP_BODY_BYTES + 1) as i64
                    ],
                )
                .unwrap();
                conn.pragma_update(None, "ignore_check_constraints", false)
                    .unwrap();
            }
            1 => {
                conn.execute("UPDATE service_post_process_cleanup_uploads SET received_bytes=total_bytes+1 WHERE id=?1",[upload.to_string()]).unwrap();
            }
            2 => {
                conn.execute("UPDATE service_post_process_cleanup_uploads SET next_sequence=4098 WHERE id=?1",[upload.to_string()]).unwrap();
            }
            _ => {
                conn.execute("UPDATE service_post_process_cleanup_chunks SET text=X'F09F' WHERE upload_id=?1",[upload.to_string()]).unwrap();
            }
        }
        drop(conn);
        let before = database(&f.store);
        assert!(call(
            &f,
            &request(
                &f,
                PostProcessCleanupOperation::Commit { upload_id: upload }
            )
        )
        .is_err());
        assert_eq!(database(&f.store), before);
    }
}

#[test]
fn decoded_update_count_and_chunk_digest_fail_without_partial_visible_cleanup() {
    let f = fixture(CaptureProvider::Local);
    let upload = Uuid::new_v4();
    let begin = request(
        &f,
        PostProcessCleanupOperation::Begin {
            upload_id: upload,
            total_bytes: 2,
            sha256: sha(b"[]"),
        },
    );
    call(&f, &begin).unwrap();
    let before = database(&f.store);
    let bad = request(
        &f,
        PostProcessCleanupOperation::Append {
            upload_id: upload,
            sequence: 1,
            byte_offset: 0,
            text: "[]".into(),
            sha256: "0".repeat(64),
        },
    );
    assert!(matches!(
        call(&f, &bad),
        Err(DataError::Invalid("cleanup chunk digest"))
    ));
    assert_eq!(database(&f.store), before);
    let updates = (0..=MAX_POST_PROCESS_CLEANUP_UPDATES)
        .map(|_| PostProcessCleanupUpdate {
            segment_id: f.ids[0],
            text: "".into(),
        })
        .collect::<Vec<_>>();
    let text = serde_json::to_string(&updates).unwrap();
    let (upload, _, _) = stage(&f, &text);
    let before = database(&f.store);
    assert!(matches!(
        call(
            &f,
            &request(
                &f,
                PostProcessCleanupOperation::Commit { upload_id: upload }
            )
        ),
        Err(DataError::Invalid("cleanup body"))
    ));
    assert_eq!(database(&f.store), before);
}

#[path = "postprocess_cleanup_security_tests.rs"]
mod security;

//! Store state-machine checks. Kernel peer authentication, released capture
//! guards and artifact ownership are independently verified by Core tests.
//! Synthetic PIDs here never stand in for those OS checks.
use super::*;
use rusqlite::types::Value;
use std::sync::{Arc, Barrier};

const POST_PID: u32 = 42;
const CAPTURE_PID: u32 = 43;

struct Actor {
    pid: u32,
    proof: WorkerProof,
    registered: RegisteredWorker,
}

fn register(store: &Store, epoch: Uuid, pid: u32, capability: WorkerCapability) -> Actor {
    let registration = WorkerRegistration {
        session_id: Uuid::new_v4(),
        secret: WorkerSecret::random(),
        capabilities: vec![capability],
    };
    let registered = store
        .service_register_worker(pid, epoch, &registration, 60)
        .unwrap();
    Actor {
        pid,
        proof: WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: epoch,
        },
        registered,
    }
}

fn captured(
    store: &Store,
    epoch: Uuid,
    status: CaptureTerminalStatus,
) -> (CaptureLease, Option<String>) {
    let actor = register(store, epoch, CAPTURE_PID, WorkerCapability::Capture);
    let start = CaptureStart {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        title: "Synthetic post-process input — Орион".into(),
        app: Some("fixture".into()),
        started_at: Utc::now() - Duration::seconds(3),
        participants: vec!["Анна".into()],
        provider: CaptureSelection {
            provider: CaptureProvider::Soniox,
            model: Some("fixture-soniox".into()),
            language: Some("ru-RU".into()),
        },
    };
    let capture = store
        .service_prepare_capture(actor.pid, epoch, &actor.proof, &start)
        .unwrap();
    store
        .service_begin_capture(actor.pid, epoch, &actor.proof, &capture, &start.fence)
        .unwrap();
    let terminal = CaptureTerminal {
        raw_screen: None,
        soniox_outcome: None,
        retain_local_live: false,
        status,
        provider: start.provider,
        duration_ms: 2000,
        ended_at: start.started_at + Duration::seconds(2),
        has_video: true,
        last_confirmed_sequence: 1,
        captions: vec![ConfirmedCaption {
            sequence: 1,
            start_ms: 0,
            end_ms: 2000,
            speaker: Some("Speaker 1".into()),
            text: "um исходный текст; exact raw".into(),
        }],
    };
    let outcome = store
        .service_finalize_capture(
            actor.pid,
            epoch,
            &actor.proof,
            &capture,
            &start.fence,
            &terminal,
        )
        .unwrap();
    (capture, outcome.job_id)
}

fn fixture(count: usize) -> (Store, Uuid, Actor) {
    let store = Store::open_in_memory().unwrap();
    let epoch = Uuid::new_v4();
    let actor = register(&store, epoch, POST_PID, WorkerCapability::PostProcess);
    for _ in 0..count {
        assert!(captured(&store, epoch, CaptureTerminalStatus::Completed)
            .1
            .is_some());
    }
    (store, epoch, actor)
}

fn list(store: &Store, epoch: Uuid, actor: &Actor) -> PostProcessJobPage {
    store
        .service_post_process_jobs(
            actor.pid,
            epoch,
            &PostProcessJobsRequest::new(actor.proof.clone()),
        )
        .unwrap()
}

fn claim_request(actor: &Actor, job: PostProcessJob) -> ClaimPostProcessJobRequest {
    ClaimPostProcessJobRequest {
        proof: actor.proof.clone(),
        job,
        claim_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        fence: WorkerSecret::random(),
        ttl_seconds: 30,
    }
}

fn renew_request(
    actor: &Actor,
    lease: &PostProcessJobLease,
    fence: &WorkerSecret,
) -> RenewPostProcessJobRequest {
    RenewPostProcessJobRequest {
        proof: actor.proof.clone(),
        lease: lease.clone(),
        operation_id: Uuid::new_v4(),
        fence: fence.clone(),
        ttl_seconds: 30,
    }
}

fn fail_request(
    actor: &Actor,
    lease: &PostProcessJobLease,
    fence: &WorkerSecret,
) -> FailPostProcessJobRequest {
    FailPostProcessJobRequest {
        proof: actor.proof.clone(),
        lease: lease.clone(),
        operation_id: Uuid::new_v4(),
        fence: fence.clone(),
        error_code: PostProcessFailureCode::PostProcessFailed,
    }
}

type Database = Vec<(String, Vec<Vec<Value>>)>;

/// Small fixture databases only. Include receipt/binding rows, sequences and
/// search projections so rollback checks do not overlook a partial side effect.
fn database(store: &Store) -> Database {
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
            // FTS shadow config tables can be WITHOUT ROWID.
            let order = (1..=columns)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = conn.prepare(&format!("{sql} ORDER BY {order}")).unwrap();
            let rows = statement
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

fn count(store: &Store, table: &str) -> i64 {
    store
        .conn
        .lock()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn expire_job(store: &Store, job_id: &str) {
    // Controlled clock fault: no public write may bypass the active lease.
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1",
            [job_id],
        )
        .unwrap();
}

fn assert_authority_denied(
    store: &Store,
    epoch: Uuid,
    actor: &Actor,
    original: &ClaimPostProcessJobRequest,
    lease: &PostProcessJobLease,
) {
    let before = database(store);
    assert!(matches!(
        store.service_post_process_jobs(
            actor.pid,
            epoch,
            &PostProcessJobsRequest::new(actor.proof.clone()),
        ),
        Err(DataError::Forbidden)
    ));
    let mut claim = original.clone();
    claim.proof = actor.proof.clone();
    assert!(matches!(
        store.service_claim_post_process_job(actor.pid, epoch, &claim),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_renew_post_process_job(
            actor.pid,
            epoch,
            &renew_request(actor, lease, &original.fence),
        ),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_fail_post_process_job(
            actor.pid,
            epoch,
            &fail_request(actor, lease, &original.fence),
        ),
        Err(DataError::Forbidden)
    ));
    assert_eq!(database(store), before);
}

#[test]
fn only_completed_captured_jobs_are_candidates_and_all_operations_reauthenticate() {
    let (store, epoch, actor) = fixture(1);
    let (failed, failed_job) = captured(&store, epoch, CaptureTerminalStatus::Failed);
    let page = list(&store, epoch, &actor);
    assert_eq!(page.items.len(), 1);
    let job = page.items[0].clone();
    assert!(failed_job.is_none());
    assert_ne!(job.meeting_id, failed.meeting_id);
    assert_eq!(job.provider.provider, CaptureProvider::Soniox);
    assert_eq!(job.provider.model.as_deref(), Some("fixture-soniox"));
    assert!(job.input.source_id.is_some());
    {
        let mut conn = store.conn.lock();
        let tx = conn.transaction().unwrap();
        // Same meeting/kind without its finalized capture's exact job link is
        // not a registered post-process candidate.
        execution::insert_job(
            &tx,
            "processing",
            Some(&job.meeting_id.to_string()),
            "gui_worker",
            false,
            "fixture",
            "{}",
        )
        .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(list(&store, epoch, &actor).items, vec![job.clone()]);
    let claim = claim_request(&actor, job);
    let lease = store
        .service_claim_post_process_job(actor.pid, epoch, &claim)
        .unwrap();
    let mut wrong = Actor {
        pid: actor.pid + 100,
        proof: actor.proof.clone(),
        registered: actor.registered.clone(),
    };
    assert_authority_denied(&store, epoch, &wrong, &claim, &lease);
    wrong.pid = actor.pid;
    wrong.proof.secret = WorkerSecret::random();
    assert_authority_denied(&store, epoch, &wrong, &claim, &lease);
    wrong.proof = actor.proof.clone();
    wrong.proof.library_epoch = Uuid::new_v4();
    assert_authority_denied(&store, epoch, &wrong, &claim, &lease);
    assert_authority_denied(&store, Uuid::new_v4(), &actor, &claim, &lease);
    let capture_only = register(&store, epoch, 44, WorkerCapability::Capture);
    assert_authority_denied(&store, epoch, &capture_only, &claim, &lease);
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [actor.proof.session_id.to_string()],
        )
        .unwrap();
    assert_authority_denied(&store, epoch, &actor, &claim, &lease);
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_worker_sessions SET expires_at=?2 WHERE id=?1",
            params![
                actor.proof.session_id.to_string(),
                actor.registered.expires_at
            ],
        )
        .unwrap();
    store
        .service_close_worker(actor.pid, epoch, &actor.proof)
        .unwrap();
    assert_authority_denied(&store, epoch, &actor, &claim, &lease);
}

#[test]
fn descriptor_revisions_and_original_source_fences_cannot_be_refreshed_on_claim() {
    for (index, sql) in [
        "UPDATE service_jobs SET revision=?2 WHERE entity_id=?1",
        "UPDATE service_meeting_state SET lifecycle_revision=?2 WHERE meeting_id=?1",
        "UPDATE service_transcript_state SET revision=?2 WHERE meeting_id=?1",
        "UPDATE service_transcript_state SET current_source_id=?2 WHERE meeting_id=?1",
    ]
    .into_iter()
    .enumerate()
    {
        let (store, epoch, actor) = fixture(1);
        let job = list(&store, epoch, &actor).items.remove(0);
        // Processing intentionally blocks ordinary transcript/lifecycle writes.
        // Inject an external stale-input fault, rather than weakening that guard.
        store
            .conn
            .lock()
            .execute(
                sql,
                params![job.meeting_id.to_string(), Uuid::new_v4().to_string()],
            )
            .unwrap();
        let before = database(&store);
        let error = store
            .service_claim_post_process_job(actor.pid, epoch, &claim_request(&actor, job))
            .unwrap_err();
        if index == 3 {
            // A source not owned by this captured meeting is an authorization
            // failure; a changed original revision is a stale descriptor.
            assert!(matches!(error, DataError::Forbidden), "{sql}");
        } else {
            assert!(matches!(error, DataError::Conflict { .. }), "{sql}");
        }
        assert_eq!(database(&store), before, "{sql}");
    }
}

#[test]
fn competing_claims_have_one_winner_and_lost_ack_replays_do_not_rotate_a_nonce() {
    let (store, epoch, actor) = fixture(1);
    let other = register(&store, epoch, 45, WorkerCapability::PostProcess);
    let job = list(&store, epoch, &actor).items.remove(0);
    let requests = [
        claim_request(&actor, job.clone()),
        claim_request(&other, job),
    ];
    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = requests
        .iter()
        .cloned()
        .zip([actor.pid, other.pid])
        .map(|(request, pid)| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.service_claim_post_process_job(pid, epoch, &request)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(DataError::Conflict { .. })))
            .count(),
        1
    );
    let winner = results.iter().position(|result| result.is_ok()).unwrap();
    let lease = results[winner].as_ref().unwrap();
    let request = &requests[winner];
    let pid = [actor.pid, other.pid][winner];
    assert_eq!(count(&store, "service_worker_job_claims"), 1);
    assert_eq!(count(&store, "service_worker_job_receipts"), 1);
    let persisted = format!("{:?}", database(&store));
    assert!(!persisted.contains(request.fence.expose()));
    assert!(!persisted.contains(request.proof.secret.expose()));
    assert_eq!(store.service_job(&request.job.job_id).unwrap().attempts, 1);
    let before = database(&store);
    assert_eq!(
        store
            .service_claim_post_process_job(pid, epoch, request)
            .unwrap(),
        *lease
    );
    assert_eq!(database(&store), before);
    let mut conflict = request.clone();
    conflict.fence = WorkerSecret::random();
    assert!(matches!(
        store.service_claim_post_process_job(pid, epoch, &conflict),
        Err(DataError::IdempotencyConflict)
    ));
    conflict = request.clone();
    conflict.claim_id = Uuid::new_v4();
    assert!(matches!(
        store.service_claim_post_process_job(pid, epoch, &conflict),
        Err(DataError::IdempotencyConflict)
    ));
    assert_eq!(database(&store), before);
}

#[test]
fn renew_is_bounded_by_worker_ttl_and_its_receipt_never_refreshes_on_replay() {
    let (store, epoch, actor) = fixture(1);
    let job = list(&store, epoch, &actor).items.remove(0);
    let mut claim = claim_request(&actor, job);
    claim.ttl_seconds = 5;
    let lease = store
        .service_claim_post_process_job(actor.pid, epoch, &claim)
        .unwrap();
    let mut request = renew_request(&actor, &lease, &claim.fence);
    request.ttl_seconds = 300;
    let renewed = store
        .service_renew_post_process_job(actor.pid, epoch, &request)
        .unwrap();
    assert!(renewed.expires_at > lease.expires_at);
    assert_eq!(renewed.expires_at, actor.registered.expires_at);
    assert_ne!(renewed.job_revision, lease.job_revision);
    let before = database(&store);
    assert_eq!(
        store
            .service_renew_post_process_job(actor.pid, epoch, &request)
            .unwrap(),
        renewed
    );
    assert_eq!(database(&store), before);
    request.ttl_seconds = 299;
    assert!(matches!(
        store.service_renew_post_process_job(actor.pid, epoch, &request),
        Err(DataError::IdempotencyConflict)
    ));
    assert_eq!(database(&store), before);
    expire_job(&store, &renewed.job_id);
    let expired = database(&store);
    request.ttl_seconds = 300;
    assert_eq!(
        store
            .service_renew_post_process_job(actor.pid, epoch, &request)
            .unwrap(),
        renewed
    );
    assert_eq!(database(&store), expired);
}

#[test]
fn expired_claim_reclaim_keeps_historical_receipt_but_rejects_old_lease_writes() {
    let (store, epoch, actor) = fixture(1);
    let other = register(&store, epoch, 45, WorkerCapability::PostProcess);
    let job = list(&store, epoch, &actor).items.remove(0);
    let claim = claim_request(&actor, job);
    let old = store
        .service_claim_post_process_job(actor.pid, epoch, &claim)
        .unwrap();
    expire_job(&store, &old.job_id);
    let request = claim_request(&other, list(&store, epoch, &other).items.remove(0));
    let current = store
        .service_claim_post_process_job(other.pid, epoch, &request)
        .unwrap();
    assert_ne!(old.claim_id, current.claim_id);
    assert_ne!(old.job_revision, current.job_revision);
    assert_eq!(store.service_job(&old.job_id).unwrap().attempts, 2);
    let before = database(&store);
    assert_eq!(
        store
            .service_claim_post_process_job(actor.pid, epoch, &claim)
            .unwrap(),
        old
    );
    assert!(matches!(
        store.service_renew_post_process_job(
            actor.pid,
            epoch,
            &renew_request(&actor, &old, &claim.fence)
        ),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_fail_post_process_job(
            actor.pid,
            epoch,
            &fail_request(&actor, &old, &claim.fence)
        ),
        Err(DataError::Forbidden)
    ));
    let wrong_fence = renew_request(&other, &current, &claim.fence);
    assert!(matches!(
        store.service_renew_post_process_job(other.pid, epoch, &wrong_fence),
        Err(DataError::Forbidden)
    ));
    assert_eq!(database(&store), before);
}

#[test]
fn current_epoch_reclaims_old_epoch_binding_without_waiting_for_its_live_deadline() {
    let (store, old_epoch, old_actor) = fixture(1);
    let claim = claim_request(
        &old_actor,
        list(&store, old_epoch, &old_actor).items.remove(0),
    );
    let old = store
        .service_claim_post_process_job(old_actor.pid, old_epoch, &claim)
        .unwrap();
    assert!(old.expires_at > Utc::now());
    let epoch = Uuid::new_v4();
    let actor = register(&store, epoch, 45, WorkerCapability::PostProcess);
    let request = claim_request(&actor, list(&store, epoch, &actor).items.remove(0));
    let current = store
        .service_claim_post_process_job(actor.pid, epoch, &request)
        .unwrap();
    assert_eq!(current.job_id, old.job_id);
    assert_eq!(current.library_epoch, epoch);
    assert_ne!(current.claim_id, old.claim_id);
    assert_authority_denied(&store, epoch, &old_actor, &claim, &old);
    let before = database(&store);
    // Even a trusted caller accidentally supplying the historical epoch cannot
    // use the old binding once a current owner has reclaimed the same job.
    assert!(matches!(
        store.service_fail_post_process_job(
            old_actor.pid,
            old_epoch,
            &fail_request(&old_actor, &old, &claim.fence)
        ),
        Err(DataError::Forbidden)
    ));
    assert_eq!(database(&store), before);
}

#[test]
fn legacy_reclaim_also_retires_the_registered_binding() {
    let (store, epoch, actor) = fixture(1);
    let claim = claim_request(&actor, list(&store, epoch, &actor).items.remove(0));
    let old = store
        .service_claim_post_process_job(actor.pid, epoch, &claim)
        .unwrap();
    expire_job(&store, &old.job_id);
    let job = store.service_job(&old.job_id).unwrap();
    let current = store
        .service_claim_job(&job.id, JobExecutor::GuiWorker, &job.revision, 30)
        .unwrap();
    assert_ne!(current.job.revision, old.job_revision);
    let before = database(&store);
    assert!(matches!(
        store.service_renew_post_process_job(
            actor.pid,
            epoch,
            &renew_request(&actor, &old, &claim.fence)
        ),
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        store.service_fail_post_process_job(
            actor.pid,
            epoch,
            &fail_request(&actor, &old, &claim.fence)
        ),
        Err(DataError::Forbidden)
    ));
    assert_eq!(database(&store), before);
}

#[test]
fn failure_rolls_back_through_receipt_insert_then_preserves_raw_and_media_projection() {
    let (store, epoch, actor) = fixture(1);
    let claim = claim_request(&actor, list(&store, epoch, &actor).items.remove(0));
    let lease = store
        .service_claim_post_process_job(actor.pid, epoch, &claim)
        .unwrap();
    let request = fail_request(&actor, &lease, &claim.fence);
    let id = lease.meeting_id.to_string();
    let raw = store.segments(&id).unwrap();
    let source = store
        .service_read_transcript(&id, &TranscriptRead::default())
        .unwrap();
    let provider = store
        .get_setting::<serde_json::Value>(&format!("stt:{id}"))
        .unwrap();
    let meeting = store.service_meeting(&id).unwrap().unwrap();
    store.conn.lock().execute_batch("CREATE TRIGGER postprocess_failure_fault BEFORE INSERT ON service_worker_job_receipts WHEN NEW.operation='fail' BEGIN SELECT RAISE(ABORT,'injected late receipt failure'); END;").unwrap();
    let before = database(&store);
    assert!(matches!(
        store.service_fail_post_process_job(actor.pid, epoch, &request),
        Err(DataError::Sqlite(_))
    ));
    assert_eq!(database(&store), before);
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER postprocess_failure_fault")
        .unwrap();
    let receipt = store
        .service_fail_post_process_job(actor.pid, epoch, &request)
        .unwrap();
    assert_ne!(receipt.lifecycle_revision, meeting.lifecycle_revision);
    assert_eq!(store.service_job(&lease.job_id).unwrap().state, "failed");
    let after = store.service_meeting(&id).unwrap().unwrap();
    assert_eq!(after.meeting.status, crate::MeetingStatus::Failed);
    assert_eq!(after.media_state, meeting.media_state);
    assert_eq!(after.meeting.has_video, meeting.meeting.has_video);
    assert_eq!(after.meeting.duration_ms, meeting.meeting.duration_ms);
    assert_eq!(after.meeting.started_at, meeting.meeting.started_at);
    assert_eq!(after.meeting.ended_at, meeting.meeting.ended_at);
    assert_eq!(store.segments(&id).unwrap(), raw);
    assert_eq!(
        serde_json::to_value(
            store
                .service_read_transcript(&id, &TranscriptRead::default())
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(source).unwrap()
    );
    assert_eq!(
        store
            .get_setting::<serde_json::Value>(&format!("stt:{id}"))
            .unwrap(),
        provider
    );
    assert!(list(&store, epoch, &actor).items.is_empty());
    let committed = database(&store);
    assert_eq!(
        store
            .service_fail_post_process_job(actor.pid, epoch, &request)
            .unwrap(),
        receipt
    );
    let mut conflict = request;
    conflict.error_code = PostProcessFailureCode::PostProcessInputUnavailable;
    assert!(matches!(
        store.service_fail_post_process_job(actor.pid, epoch, &conflict),
        Err(DataError::IdempotencyConflict)
    ));
    assert_eq!(database(&store), committed);
}

#[test]
fn bounded_pages_bind_scope_worker_epoch_and_complete_serialized_budget() {
    let (store, epoch, actor) = fixture(5);
    let mut request = PostProcessJobsRequest::new(actor.proof.clone());
    request.limit = 1;
    let before = database(&store);
    let first = store
        .service_post_process_jobs(actor.pid, epoch, &request)
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let cursor = first
        .next
        .clone()
        .expect("fixture has more than one candidate");
    for field in 0..4 {
        let mut invalid = cursor.clone();
        match field {
            0 => invalid.library_scope = "0".repeat(32),
            1 => invalid.worker_session_id = Uuid::new_v4(),
            2 => invalid.library_epoch = Uuid::new_v4(),
            _ => invalid.query_hash = "0".repeat(64),
        }
        request.cursor = Some(invalid);
        assert!(matches!(
            store.service_post_process_jobs(actor.pid, epoch, &request),
            Err(DataError::StaleCursor)
        ));
    }
    request.cursor = None;
    request.limit = 100;
    request.max_bytes = 1800;
    let mut seen = Vec::new();
    loop {
        let page = store
            .service_post_process_jobs(actor.pid, epoch, &request)
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= request.max_bytes as usize);
        assert!(!page.items.is_empty());
        seen.extend(page.items.into_iter().map(|job| job.job_id));
        request.cursor = page.next;
        if request.cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 5);
    assert!(seen.windows(2).all(|jobs| jobs[0] < jobs[1]));
    assert_eq!(database(&store), before);
}

#[test]
fn pagination_pins_expiry_cutoff_and_monotonic_highwater_survives_event_pruning() {
    let (store, epoch, actor) = fixture(3);
    let jobs = list(&store, epoch, &actor).items;
    let blocked = jobs.last().unwrap().clone();
    let claim = claim_request(&actor, blocked.clone());
    store
        .service_claim_post_process_job(actor.pid, epoch, &claim)
        .unwrap();
    let expires = Utc::now() + Duration::seconds(1);
    store
        .conn
        .lock()
        .execute(
            "UPDATE service_jobs SET lease_until=?2 WHERE id=?1",
            params![blocked.job_id, expires],
        )
        .unwrap();
    let mut request = PostProcessJobsRequest::new(actor.proof.clone());
    request.limit = 1;
    let first = store
        .service_post_process_jobs(actor.pid, epoch, &request)
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(first.eligible_at < expires);
    request.cursor = first.next.clone();
    assert!(request.cursor.is_some());
    let before = database(&store);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while Utc::now() <= expires {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture clock did not advance"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let next = store
        .service_post_process_jobs(actor.pid, epoch, &request)
        .unwrap();
    assert_eq!(next.eligible_at, first.eligible_at);
    assert_eq!(next.snapshot_sequence, first.snapshot_sequence);
    assert_eq!(next.items.len(), 1);
    assert_ne!(next.items[0].job_id, blocked.job_id);
    assert!(next.next.is_none());
    assert_eq!(
        database(&store),
        before,
        "lease expiry and page reads do not mutate SQL"
    );
    assert!(list(&store, epoch, &actor)
        .items
        .iter()
        .any(|job| job.job_id == blocked.job_id));
    store
        .conn
        .lock()
        .execute("DELETE FROM service_changes", [])
        .unwrap();
    let after_prune = store
        .service_post_process_jobs(actor.pid, epoch, &request)
        .unwrap();
    assert_eq!(after_prune.snapshot_sequence, first.snapshot_sequence);
    captured(&store, epoch, CaptureTerminalStatus::Completed);
    store
        .conn
        .lock()
        .execute(
            "DELETE FROM service_changes WHERE sequence>?1",
            [first.snapshot_sequence],
        )
        .unwrap();
    assert!(matches!(
        store.service_post_process_jobs(actor.pid, epoch, &request),
        Err(DataError::StaleCursor)
    ));
}

fn assert_liveness_is_read_only(store: &Store, epoch: Uuid, expected: bool) {
    let before = database(store);
    assert_eq!(
        store.service_has_live_post_process_claims(epoch).unwrap(),
        expected
    );
    assert_eq!(
        database(store),
        before,
        "idle retention must not mutate jobs or receipts"
    );
}

#[test]
fn idle_retention_excludes_waiting_jobs_and_unregistered_generic_claims() {
    let (store, epoch, actor) = fixture(1);
    assert_liveness_is_read_only(&store, epoch, false);
    let job = list(&store, epoch, &actor).items.remove(0);
    store
        .service_claim_job(&job.job_id, JobExecutor::GuiWorker, &job.revision, 30)
        .unwrap();
    assert_liveness_is_read_only(&store, epoch, false);
    let before = database(&store);
    assert!(matches!(
        store.service_has_live_post_process_claims(Uuid::nil()),
        Err(DataError::Invalid(_))
    ));
    assert_eq!(database(&store), before);
}

#[test]
fn idle_retention_is_current_epoch_only_and_stops_after_failure_or_revoke() {
    let (store, epoch, actor) = fixture(1);
    let request = claim_request(&actor, list(&store, epoch, &actor).items.remove(0));
    let lease = store
        .service_claim_post_process_job(actor.pid, epoch, &request)
        .unwrap();
    assert_liveness_is_read_only(&store, epoch, true);
    assert_liveness_is_read_only(&store, Uuid::new_v4(), false);
    store
        .service_fail_post_process_job(
            actor.pid,
            epoch,
            &fail_request(&actor, &lease, &request.fence),
        )
        .unwrap();
    assert_liveness_is_read_only(&store, epoch, false);

    let (store, epoch, actor) = fixture(1);
    let request = claim_request(&actor, list(&store, epoch, &actor).items.remove(0));
    store
        .service_claim_post_process_job(actor.pid, epoch, &request)
        .unwrap();
    assert_liveness_is_read_only(&store, epoch, true);
    store
        .service_close_worker(actor.pid, epoch, &actor.proof)
        .unwrap();
    assert_liveness_is_read_only(&store, epoch, false);
}

#[test]
fn idle_retention_checks_both_deadlines_binding_capability_and_running_state() {
    let (store, epoch, actor) = fixture(1);
    let request = claim_request(&actor, list(&store, epoch, &actor).items.remove(0));
    let lease = store
        .service_claim_post_process_job(actor.pid, epoch, &request)
        .unwrap();
    let session = actor.proof.session_id.to_string();
    let claim_id = lease.claim_id.to_string();
    assert_liveness_is_read_only(&store, epoch, true);
    for (sql, id) in [
        ("UPDATE service_jobs SET lease_until='2000-01-01T00:00:00Z' WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_worker_sessions SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1", session.as_str()),
        ("UPDATE service_worker_sessions SET revoked=1 WHERE id=?1", session.as_str()),
        ("UPDATE service_worker_sessions SET library_epoch='another-epoch' WHERE id=?1", session.as_str()),
        ("UPDATE service_worker_sessions SET capabilities_json='[\"capture\"]' WHERE id=?1", session.as_str()),
        ("UPDATE service_worker_sessions SET capabilities_json='[\"PostProcess\"]' WHERE id=?1", session.as_str()),
        ("UPDATE service_worker_sessions SET capabilities_json='{\"cap\":\"post_process\"}' WHERE id=?1", session.as_str()),
        ("UPDATE service_worker_job_claims SET library_epoch='another-epoch' WHERE claim_id=?1", claim_id.as_str()),
        ("UPDATE service_worker_job_claims SET state='retired' WHERE claim_id=?1", claim_id.as_str()),
        ("UPDATE service_jobs SET state='interrupted' WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_jobs SET kind='capture' WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_jobs SET executor='service' WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_jobs SET mandatory=1 WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_jobs SET cancel_requested=1 WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_jobs SET claim_token=NULL WHERE id=?1", lease.job_id.as_str()),
        ("UPDATE service_jobs SET claim_token='' WHERE id=?1", lease.job_id.as_str()),
    ] {
        // Drop the mutex while the savepoint is live so the actual public read
        // method observes each controlled fault on this synthetic connection.
        store.conn.lock().execute_batch("SAVEPOINT idle_retention_fault").unwrap();
        store.conn.lock().execute(sql, [id]).unwrap();
        assert_liveness_is_read_only(&store, epoch, false);
        store.conn.lock().execute_batch("ROLLBACK TO idle_retention_fault; RELEASE idle_retention_fault").unwrap();
        assert_liveness_is_read_only(&store, epoch, true);
    }
    // A legacy reclaim retires the registered binding even though the job
    // immediately becomes running again with a fresh internal claim token.
    expire_job(&store, &lease.job_id);
    let job = store.service_job(&lease.job_id).unwrap();
    store
        .service_claim_job(&job.id, JobExecutor::GuiWorker, &job.revision, 30)
        .unwrap();
    assert_liveness_is_read_only(&store, epoch, false);
}

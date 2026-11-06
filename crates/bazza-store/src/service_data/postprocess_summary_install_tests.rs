use super::*;
use crate::{
    service_data::{
        CaptureProvider, CaptureSelection, MutationContext, PostProcessSourceFence, SummaryAuthor,
        SummaryWrite, SummaryWriteMode,
    },
    Store,
};
use bazza_domain::speaker_result::SpeakerResultOutcome;
use serde_json::json;
use uuid::Uuid;

const TIME: &str = "2026-11-03T00:00:00Z";
const MARKDOWN: &str = "  Итоги: встреча 🦀\n\n  ";

fn fixture() -> (Store, PostProcessJob, WorkerGeneratedSummary) {
    let store = Store::open_in_memory().unwrap();
    let job = PostProcessJob {
        job_id: "summary-job".into(),
        meeting_id: Uuid::new_v4(),
        revision: "job-rev".into(),
        input: PostProcessSourceFence {
            capture_id: Uuid::new_v4(),
            capture_generation: Uuid::new_v4(),
            checkpoint_revision: "ready-cleanup-checkpoint".into(),
            lifecycle_revision: "life-rev".into(),
            transcript_revision: "source-rev".into(),
            source_id: Some("local-source".into()),
        },
        provider: CaptureSelection {
            provider: CaptureProvider::Local,
            model: None,
            language: None,
        },
        stage: PostProcessStage::Ready {
            local_asr: LocalAsrOutcome::Complete,
            speakers: SpeakerResultOutcome::Applied,
            cleanup: true,
            generated_summary_revision: None,
        },
    };
    let body = WorkerGeneratedSummary {
        expected_summary_revision: "summary-rev".into(),
        template_id: "Повестка встречи".into(),
        expected_template_revision: "template-rev".into(),
        expected_template_generation: "template-generation".into(),
        model: "fixture local model".into(),
        markdown: MARKDOWN.into(),
        structured_json: json!({"sections": ["  exact ", "\u{0000}"]}),
    };
    let id = job.meeting_id.to_string();
    {
        let conn = store.conn.lock();
        conn.execute(
            "INSERT INTO meetings(id,title,started_at,created_at,duration_ms,status)
             VALUES(?1,'Fixture summary',?2,?2,3000,'ready')",
            params![id, TIME],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_meeting_state VALUES(?1,'recorded','meta-rev','life-rev',0)",
            [&id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_jobs(id,kind,entity_id,executor,state,revision,actor,created_at,updated_at)
             VALUES(?1,'processing',?2,'gui_worker','running',?3,'fixture',?4,?4)",
            params![job.job_id, id, job.revision, TIME],
        ).unwrap();
        conn.execute(
            "INSERT INTO segments(id,meeting_id,start_ms,end_ms,text) VALUES(101,?1,0,1000,'Exact raw')",
            [&id],
        ).unwrap();
        conn.execute(
            "INSERT INTO service_transcript_sources VALUES('local-source',?1,'local_asr','local',NULL,?2,?3)",
            params![id, json!([{"id":101,"start_ms":0,"end_ms":1000,"speaker":null,"text":"Exact raw"}]).to_string(), TIME],
        ).unwrap();
        conn.execute(
            "INSERT INTO service_transcript_state VALUES(?1,'source-rev','local-source','present',0)",
            [&id],
        ).unwrap();
        conn.execute(
            "INSERT INTO service_post_process_local_sources VALUES('local-source',?1,'actual-model','complete',1)",
            [&job.job_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO service_summary_state(meeting_id,revision,document_state,origin,auto_replace)
             VALUES(?1,'summary-rev','absent','legacy',0)",
            [&id],
        ).unwrap();
        conn.execute(
            "INSERT INTO templates VALUES(?1,'Fixture template','','Prompt',0,?2)",
            params![body.template_id, TIME],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO service_template_state VALUES(?1,'template-rev','template-generation','present',0)",
            [&body.template_id],
        ).unwrap();
    }
    (store, job, body)
}

fn publish(
    store: &Store,
    job: &PostProcessJob,
    body: WorkerGeneratedSummary,
) -> DataResult<InstalledWorkerSummary> {
    let mut conn = store.conn.lock();
    let tx = conn.transaction()?;
    let installed = install_on(&tx, job, body, "fixture-worker")?;
    tx.commit()?;
    Ok(installed)
}

#[test]
fn worker_summary_is_exact_and_rebuilds_search_and_history() {
    let (store, job, mut body) = fixture();
    let id = job.meeting_id.to_string();
    let result = publish(&store, &job, body.clone()).unwrap();
    let current = store.service_read_summary(&id).unwrap();
    assert_eq!(current.state.revision, result.revision);
    assert_eq!(current.state.origin, "configured_generation");
    assert!(current.state.auto_replace);
    assert_eq!(current.state.source_revision.as_deref(), Some("source-rev"));
    let current = current.current.unwrap();
    assert_eq!(current.markdown, MARKDOWN);
    assert_eq!(current.structured, body.structured_json);
    assert_eq!(current.template_id, body.template_id);
    assert_eq!(current.model, body.model);

    body.expected_summary_revision = result.revision.clone();
    body.markdown = "Replacement".into();
    let replaced = publish(&store, &job, body).unwrap();
    assert!(replaced.change_sequence > result.change_sequence);
    let history = store.service_summary_history(&id, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].revision, result.revision);
    assert_eq!(history[0].summary.markdown, MARKDOWN);
    let conn = store.conn.lock();
    let indexed: (String, String) = conn.query_row(
        "SELECT revision,text FROM service_search_documents WHERE meeting_id=?1 AND source_kind='summary'",
        [&id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(indexed, (replaced.revision, "Replacement".into()));
}

#[test]
fn current_worker_inputs_and_protected_manual_target_are_checked_in_one_transaction() {
    let (store, job, mut body) = fixture();
    let id = job.meeting_id.to_string();
    let mut stale = job.clone();
    stale.input.transcript_revision = "stale-source-rev".into();
    assert!(matches!(
        publish(&store, &stale, body.clone()),
        Err(DataError::Conflict { .. })
    ));
    let mut wrong_template = body.clone();
    wrong_template.expected_template_generation = "old-generation".into();
    assert!(matches!(
        publish(&store, &job, wrong_template),
        Err(DataError::Conflict { .. })
    ));
    let mut stale_target = body.clone();
    stale_target.expected_summary_revision = "old-summary".into();
    assert!(matches!(
        publish(&store, &job, stale_target),
        Err(DataError::Conflict { .. })
    ));
    let mut not_cleaned = job.clone();
    if let PostProcessStage::Ready { cleanup, .. } = &mut not_cleaned.stage {
        *cleanup = false;
    }
    assert!(matches!(
        publish(&store, &not_cleaned, body.clone()),
        Err(DataError::Forbidden)
    ));
    assert!(store.service_read_summary(&id).unwrap().current.is_none());

    let manual = store
        .service_write_summary(
            &MutationContext {
                principal: "fixture-user".into(),
                idempotency_key: "manual-summary".into(),
            },
            &id,
            &SummaryWrite {
                expected_revision: body.expected_summary_revision.clone(),
                mode: SummaryWriteMode::Create,
                markdown: "Protected summary".into(),
                structured_json: json!({}),
                source_label: None,
                author: SummaryAuthor::ExternalAgent,
            },
        )
        .unwrap();
    body.expected_summary_revision = manual.revision;
    assert!(matches!(
        publish(&store, &job, body),
        Err(DataError::Forbidden)
    ));
    assert_eq!(
        store
            .service_read_summary(&id)
            .unwrap()
            .current
            .unwrap()
            .markdown,
        "Protected summary"
    );
}

#[test]
fn late_change_failure_rolls_back_summary_history_and_search() {
    let (store, job, mut body) = fixture();
    let id = job.meeting_id.to_string();
    let first = publish(&store, &job, body.clone()).unwrap();
    body.expected_summary_revision = first.revision.clone();
    body.markdown = "Must roll back".into();
    store.conn.lock().execute_batch(
        "CREATE TRIGGER fail_worker_summary_change BEFORE INSERT ON service_changes
         WHEN NEW.entity_kind='summary' BEGIN SELECT RAISE(ABORT,'fixture summary change failure'); END;",
    ).unwrap();
    let search_before = store.service_search_state().unwrap();
    assert!(matches!(
        publish(&store, &job, body),
        Err(DataError::Sqlite(_))
    ));
    let summary = store.service_read_summary(&id).unwrap();
    assert_eq!(summary.state.revision, first.revision);
    assert_eq!(summary.current.unwrap().markdown, MARKDOWN);
    assert!(store.service_summary_history(&id, 10).unwrap().is_empty());
    assert_eq!(store.service_search_state().unwrap(), search_before);
}

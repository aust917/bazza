//! Owned fixture service acceptance for generated summary and terminal completion.
use super::{source::Claim, *};
use bazza_service_api::{
    content::{DesktopSummaryField, DesktopSummaryRequest},
    fixture_read::{FixtureReadRequest, FixtureReadTool},
    postprocess_complete::{
        CompleteFixturePostProcessRequest, FixtureCleanupOutcome, FixtureExcludedStage,
        FixtureSummaryOutcome,
    },
    postprocess_source::{LocalAsrCaption, LocalAsrOutcome},
    postprocess_speakers::{PostProcessSpeakersReply, SpeakerResultBody, SpeakerSkipReason},
    postprocess_summary::{
        PostProcessSummaryOperation, PostProcessSummaryReply, PostProcessSummaryRequest,
        WorkerGeneratedSummary,
    },
    templates::{DesktopTemplatesReply, DesktopTemplatesRequest},
};
use serde_json::{json, Value};

async fn stage_summary(
    client: &mut ServiceClient,
    active: &Claim,
    lease: &PostProcessJobLease,
    body: &WorkerGeneratedSummary,
) -> PostProcessSummaryRequest {
    let upload_id = Uuid::new_v4();
    let body = serde_json::to_string(body).unwrap();
    let begin = active.source(
        lease,
        PostProcessSummaryOperation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: source::sha(&body),
        },
    );
    assert!(matches!(
        client
            .post_process_summary(&active.worker, begin)
            .await
            .unwrap()
            .data,
        PostProcessSummaryReply::Upload(_)
    ));
    let mut offset = 0;
    let mut sequence = 1;
    while offset < body.len() {
        let mut end = (offset + 8192).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let text = &body[offset..end];
        let append = active.source(
            lease,
            PostProcessSummaryOperation::Append {
                upload_id,
                sequence,
                byte_offset: offset as u64,
                text: text.into(),
                sha256: source::sha(text),
            },
        );
        assert!(matches!(
            client
                .post_process_summary(&active.worker, append)
                .await
                .unwrap()
                .data,
            PostProcessSummaryReply::Upload(_)
        ));
        offset = end;
        sequence += 1;
    }
    assert!(sequence > 2);
    active.source(lease, PostProcessSummaryOperation::Commit { upload_id })
}

#[tokio::test(flavor = "current_thread")]
async fn generated_summary_publishes_after_cleanup_and_completion_revokes_the_claim() {
    timeout(Duration::from_secs(45), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let active = Claim::new(&mut client, &captured).await;
        let selected = speakers::select_source(
            &mut client,
            &active,
            LocalAsrOutcome::Complete,
            vec![LocalAsrCaption {
                start_ms: 0,
                end_ms: 1_000,
                text: " Оригинал для итога ".into(),
            }],
        )
        .await;
        let segment = client
            .post_process_transcript(&active.worker, active.read(&selected))
            .await
            .unwrap()
            .data
            .items[0]
            .segment_id;
        let speakers = speakers::stage(
            &mut client,
            &active,
            &selected,
            &SpeakerResultBody::Skipped {
                reason: SpeakerSkipReason::Disabled,
            },
        )
        .await;
        let PostProcessSpeakersReply::Committed(speakers) = client
            .post_process_speakers(&active.worker, speakers)
            .await
            .unwrap()
            .data
        else {
            panic!("expected a committed speaker result");
        };
        let ready = client
            .mark_post_process_ready(
                &active.worker,
                speakers::ready_request(&active, &speakers.lease),
            )
            .await
            .unwrap()
            .data
            .lease;
        let begin_probe = |lease: &PostProcessJobLease| {
            active.source(
                lease,
                PostProcessSummaryOperation::Begin {
                    upload_id: Uuid::new_v4(),
                    total_bytes: 2,
                    sha256: source::sha("{}"),
                },
            )
        };
        remote(
            client
                .post_process_summary(&active.worker, begin_probe(&ready))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let lease = source::cleanup_after_source(&mut client, &active, &ready, segment).await;
        let meeting = header(&mut client, captured.meeting_id).await;
        assert_eq!(meeting.status, "ready");
        let DesktopTemplatesReply::Describe(template) = client
            .desktop_templates(DesktopTemplatesRequest::Describe {
                template_id: "fixture-template".into(),
            })
            .await
            .unwrap()
            .data
        else {
            panic!("expected the stored fixture template");
        };
        let body = WorkerGeneratedSummary {
            expected_summary_revision: meeting.summary_revision.clone(),
            template_id: template.id,
            expected_template_revision: template.revision,
            expected_template_generation: template.generation,
            model: "fixture-summary-model".into(),
            markdown: format!("  # Итог\n{}  \n", "Орион после очистки.\n".repeat(520)),
            structured_json: json!({"decisions": ["Орион", "exact\u{0} text"], "count": 2}),
        };
        let commit = stage_summary(&mut client, &active, &lease, &body).await;
        let published = client
            .post_process_summary(&active.worker, commit.clone())
            .await
            .unwrap()
            .data;
        let PostProcessSummaryReply::Committed(summary) = &published else {
            panic!("expected a committed summary");
        };
        assert_ne!(summary.summary_revision, meeting.summary_revision);
        assert_eq!(summary.job_revision, lease.job_revision);
        assert_eq!(summary.input, lease.input);
        assert_eq!(
            client
                .post_process_summary(&active.worker, commit)
                .await
                .unwrap()
                .data,
            published
        );

        let markdown_request = DesktopSummaryRequest {
            meeting_id: captured.meeting_id.to_string(),
            expected_revision: summary.summary_revision.clone(),
            field: DesktopSummaryField::Markdown,
            cursor: None,
            max_bytes: 32_768,
        };
        let markdown = client
            .desktop_summary(markdown_request.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(markdown.text, body.markdown);
        assert!(markdown.next.is_none());
        let structured = client
            .desktop_summary(DesktopSummaryRequest {
                field: DesktopSummaryField::StructuredJson,
                ..markdown_request
            })
            .await
            .unwrap()
            .data;
        assert!(structured.next.is_none());
        assert_eq!(
            serde_json::from_str::<Value>(&structured.text).unwrap(),
            body.structured_json
        );
        let public = client
            .fixture_read(FixtureReadRequest {
                tool: FixtureReadTool::Summary,
                arguments: json!({
                    "meeting_id": captured.meeting_id,
                    "format": "markdown",
                    "max_bytes": 32_768,
                }),
            })
            .await
            .unwrap()
            .output;
        assert_eq!(public["data"]["revision"], summary.summary_revision);
        assert_eq!(public["data"]["origin"], "configured_generation");
        assert_eq!(public["data"]["content"], body.markdown);

        let request = CompleteFixturePostProcessRequest {
            proof: active.proof.clone(),
            lease: lease.clone(),
            fence: active.fence.clone(),
            operation_id: Uuid::new_v4(),
        };
        let done = client
            .complete_fixture_post_process(&active.worker, request.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(done.cleanup, FixtureCleanupOutcome::Accepted);
        assert_eq!(
            done.summary,
            FixtureSummaryOutcome::Committed {
                revision: summary.summary_revision.clone(),
            }
        );
        assert_eq!(done.embeddings, FixtureExcludedStage::OutsideFixtureScope);
        assert_eq!(done.webhook, FixtureExcludedStage::OutsideFixtureScope);
        assert_eq!(done.input, lease.input);
        assert_ne!(done.job_revision, lease.job_revision);
        assert!(done.change_sequence > summary.change_sequence);
        assert!(serde_json::to_value(&done).unwrap().get("lease").is_none());
        assert_eq!(
            client
                .complete_fixture_post_process(&active.worker, request)
                .await
                .unwrap()
                .data,
            done
        );
        remote(
            client
                .post_process_summary(&active.worker, begin_probe(&lease))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        drop(client);
        h.stop().await;

        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let pending = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof))
            .await
            .unwrap()
            .data;
        assert!(pending.next.is_none());
        assert!(pending
            .items
            .iter()
            .all(|job| job.job_id != captured.job_id));
        let after = header(&mut client, captured.meeting_id).await;
        assert_eq!(after.status, "ready");
        assert_eq!(after.lifecycle_revision, meeting.lifecycle_revision);
        assert_eq!(after.transcript_revision, meeting.transcript_revision);
        assert_eq!(after.summary_revision, summary.summary_revision);
        drop(client);
        h.stop().await;
        let repository =
            Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        assert_eq!(
            repository.data().job(&captured.job_id).unwrap().state,
            "succeeded"
        );
    })
    .await
    .expect("owned fixture completion timed out");
}

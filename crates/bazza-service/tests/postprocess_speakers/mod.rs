//! Owned fixture service acceptance for speaker publication and Ready.
use super::{source::Claim, *};
use bazza_service_api::{
    content::DesktopTranscriptFragment,
    postprocess_ready::PostProcessReadyRequest,
    postprocess_source::{LocalAsrBody, LocalAsrCaption, LocalAsrOutcome, PostProcessSourceReply},
    postprocess_speakers::*,
};

pub(super) async fn select_source(
    client: &mut ServiceClient,
    active: &Claim,
    outcome: LocalAsrOutcome,
    captions: Vec<LocalAsrCaption>,
) -> PostProcessJobLease {
    let body = serde_json::to_string(&LocalAsrBody {
        model: "speaker-fixture-local".into(),
        language: Some("ru".into()),
        outcome,
        captions,
    })
    .unwrap();
    let upload_id = source::upload(client, active, &body).await;
    let commit = active.source(
        &active.lease,
        PostProcessSpeakersOperation::Commit { upload_id },
    );
    let reply = client
        .post_process_source(&active.worker, commit)
        .await
        .unwrap()
        .data;
    let PostProcessSourceReply::Committed(receipt) = reply else {
        panic!("expected a selected local ASR source")
    };
    receipt.lease
}

pub(super) async fn stage(
    client: &mut ServiceClient,
    active: &Claim,
    lease: &PostProcessJobLease,
    result: &SpeakerResultBody,
) -> PostProcessSpeakersRequest {
    let upload_id = Uuid::new_v4();
    let text = serde_json::to_string(result).unwrap();
    let sha256 = source::sha(&text);
    for operation in [
        PostProcessSpeakersOperation::Begin {
            upload_id,
            total_bytes: text.len() as u64,
            sha256: sha256.clone(),
        },
        PostProcessSpeakersOperation::Append {
            upload_id,
            sequence: 1,
            byte_offset: 0,
            text,
            sha256,
        },
    ] {
        let request = active.source(lease, operation);
        assert!(matches!(
            client
                .post_process_speakers(&active.worker, request)
                .await
                .unwrap()
                .data,
            PostProcessSpeakersReply::Upload(_)
        ));
    }
    active.source(lease, PostProcessSpeakersOperation::Commit { upload_id })
}

async fn transcript(
    client: &mut ServiceClient,
    active: &Claim,
    lease: &PostProcessJobLease,
    representation: DesktopTranscriptRepresentation,
) -> Vec<DesktopTranscriptFragment> {
    let mut request = active.read(lease);
    request.page.representation = representation;
    request.page.limit = 8;
    let page = client
        .post_process_transcript(&active.worker, request)
        .await
        .unwrap()
        .data;
    assert!(page.next.is_none());
    assert_eq!(page.source_id, lease.input.source_id);
    page.items
}

pub(super) fn ready_request(
    active: &Claim,
    lease: &PostProcessJobLease,
) -> PostProcessReadyRequest {
    PostProcessReadyRequest {
        proof: active.proof.clone(),
        lease: lease.clone(),
        fence: active.fence.clone(),
        operation_id: Uuid::new_v4(),
    }
}

fn assert_same_source_rotated(before: &PostProcessJobLease, after: &PostProcessJobLease) {
    assert_eq!(before.claim_id, after.claim_id);
    assert_eq!(before.expires_at, after.expires_at);
    assert_eq!(before.input.capture_id, after.input.capture_id);
    assert_eq!(
        before.input.capture_generation,
        after.input.capture_generation
    );
    assert_eq!(before.input.source_id, after.input.source_id);
    assert_ne!(
        before.input.checkpoint_revision,
        after.input.checkpoint_revision
    );
    assert_ne!(before.job_revision, after.job_revision);
}

#[tokio::test(flavor = "current_thread")]
async fn anonymous_labels_fence_source_order_and_ready_keeps_the_job_running() {
    timeout(Duration::from_secs(45), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let active = Claim::new(&mut client, &captured).await;
        let original = transcript(
            &mut client,
            &active,
            &active.lease,
            DesktopTranscriptRepresentation::Raw,
        )
        .await;
        assert_eq!(original[0].speaker.as_deref(), Some("Sam"));

        let too_early = SpeakerResultBody::Skipped {
            reason: SpeakerSkipReason::Disabled,
        };
        let body = serde_json::to_string(&too_early).unwrap();
        let premature_upload = active.source(
            &active.lease,
            PostProcessSpeakersOperation::Begin {
                upload_id: Uuid::new_v4(),
                total_bytes: body.len() as u64,
                sha256: source::sha(&body),
            },
        );
        remote(
            client
                .post_process_speakers(&active.worker, premature_upload)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );

        let captions: Vec<_> = [
            (300, 400, "  first ASR segment Орион  "),
            (0, 100, "earlier time, second ASR segment"),
            (200, 200, "zero duration"),
            (500, 600, "repeat first speaker"),
        ]
        .into_iter()
        .map(|(start_ms, end_ms, text)| LocalAsrCaption {
            start_ms,
            end_ms,
            text: text.into(),
        })
        .collect();
        let selected =
            select_source(&mut client, &active, LocalAsrOutcome::Complete, captions).await;
        remote(
            client
                .mark_post_process_ready(&active.worker, ready_request(&active, &selected))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let asr = transcript(
            &mut client,
            &active,
            &selected,
            DesktopTranscriptRepresentation::Raw,
        )
        .await;
        assert_eq!(asr.len(), 4);
        assert_eq!(asr[0].start_ms, 300);
        assert!(asr.iter().all(|item| item.speaker.is_none()));
        let assignments: Vec<_> = asr
            .iter()
            .zip([Some(900), Some(8), None, Some(900)])
            .map(|(item, cluster)| SpeakerAssignment {
                segment_id: item.segment_id,
                cluster,
            })
            .collect();

        let mut stale_cursor = active.read(&selected).page;
        stale_cursor.cursor = client
            .desktop_transcript(stale_cursor.clone())
            .await
            .unwrap()
            .data
            .next;
        assert!(stale_cursor.cursor.is_some());

        let mut out_of_order = assignments.clone();
        out_of_order.swap(0, 1);
        let mut zero_duration = assignments.clone();
        zero_duration[2].cluster = Some(8);
        for malformed in [out_of_order, zero_duration] {
            let commit = stage(
                &mut client,
                &active,
                &selected,
                &SpeakerResultBody::Applied {
                    assignments: malformed,
                },
            )
            .await;
            remote(
                client
                    .post_process_speakers(&active.worker, commit)
                    .await
                    .unwrap_err(),
                ErrorCode::InvalidArgument,
            );
        }
        assert_eq!(
            transcript(
                &mut client,
                &active,
                &selected,
                DesktopTranscriptRepresentation::Raw,
            )
            .await,
            asr
        );

        let commit = stage(
            &mut client,
            &active,
            &selected,
            &SpeakerResultBody::Applied { assignments },
        )
        .await;
        discard_committed_reply(
            &h.root,
            Request::PostProcessSpeakers(Box::new(commit.clone())),
        )
        .await;
        let reply = client
            .post_process_speakers(&active.worker, commit.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(
            reply,
            client
                .post_process_speakers(&active.worker, commit.clone())
                .await
                .unwrap()
                .data
        );
        let PostProcessSpeakersReply::Committed(receipt) = reply else {
            panic!("expected the original durable speaker receipt")
        };
        assert_eq!(receipt.outcome, SpeakerResultOutcome::Applied);
        assert_eq!(receipt.changed_segments, 3);
        let labeled = receipt.lease;
        assert_same_source_rotated(&selected, &labeled);
        assert_eq!(
            selected.input.lifecycle_revision,
            labeled.input.lifecycle_revision
        );
        assert_ne!(
            selected.input.transcript_revision,
            labeled.input.transcript_revision
        );
        assert_eq!(
            header(&mut client, captured.meeting_id).await.status,
            "processing"
        );
        remote(
            client
                .post_process_transcript(&active.worker, active.read(&selected))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        remote(
            client.desktop_transcript(stale_cursor).await.unwrap_err(),
            ErrorCode::ResyncRequired,
        );
        for representation in [
            DesktopTranscriptRepresentation::Raw,
            DesktopTranscriptRepresentation::Effective,
        ] {
            let mut expected = asr.clone();
            for (item, label) in expected.iter_mut().zip([
                Some("Speaker 1"),
                Some("Speaker 2"),
                None,
                Some("Speaker 1"),
            ]) {
                item.speaker = label.map(str::to_owned);
            }
            assert_eq!(
                transcript(&mut client, &active, &labeled, representation).await,
                expected
            );
        }
        let archived = client
            .desktop_transcript(DesktopTranscriptRequest {
                source_id: active.lease.input.source_id.clone(),
                ..active.read(&labeled).page
            })
            .await
            .unwrap()
            .data;
        assert_eq!(archived.items, original);

        let request = ready_request(&active, &labeled);
        let ready = client
            .mark_post_process_ready(&active.worker, request.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(
            ready.lease,
            client
                .mark_post_process_ready(&active.worker, request)
                .await
                .unwrap()
                .data
                .lease
        );
        assert_same_source_rotated(&labeled, &ready.lease);
        assert_ne!(
            labeled.input.lifecycle_revision,
            ready.lease.input.lifecycle_revision
        );
        assert_eq!(
            labeled.input.transcript_revision,
            ready.lease.input.transcript_revision
        );
        let meeting = header(&mut client, captured.meeting_id).await;
        assert_eq!(meeting.status, "ready");
        assert_eq!(
            meeting.lifecycle_revision,
            ready.lease.input.lifecycle_revision
        );
        remote(
            client
                .post_process_transcript(&active.worker, active.read(&labeled))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        remote(
            client
                .fail_post_process_job(
                    &active.worker,
                    FailPostProcessJobRequest {
                        proof: active.proof.clone(),
                        lease: ready.lease.clone(),
                        operation_id: Uuid::new_v4(),
                        fence: active.fence.clone(),
                        error_code: PostProcessFailureCode::PostProcessFailed,
                    },
                )
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let renewed = active.renew(&mut client, &ready.lease).await;
        let latest =
            source::cleanup_after_source(&mut client, &active, &renewed, asr[0].segment_id).await;
        assert_eq!(
            latest.input.lifecycle_revision,
            ready.lease.input.lifecycle_revision
        );
        raw_unchanged(&h.root, &captured);

        drop(client);
        h.stop().await;
        h.start().await;
        raw_expect_error(
            &h.root,
            Request::PostProcessSpeakers(Box::new(commit)),
            ErrorCode::PermissionDenied,
        )
        .await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let reclaimed = Claim::new(&mut client, &captured).await;
        assert_eq!(reclaimed.lease.input, latest.input);
        assert_eq!(
            reclaimed.job.stage,
            PostProcessStage::Ready {
                local_asr: LocalAsrOutcome::Complete,
                speakers: SpeakerResultOutcome::Applied,
                cleanup: true,
                generated_summary_revision: None,
            }
        );
        let current = transcript(
            &mut client,
            &reclaimed,
            &reclaimed.lease,
            DesktopTranscriptRepresentation::Effective,
        )
        .await;
        assert_eq!(current[0].speaker.as_deref(), Some("Speaker 1"));
        assert_ne!(current[0].text, asr[0].text);
        assert_eq!(
            header(&mut client, captured.meeting_id).await.status,
            "ready"
        );
        raw_unchanged(&h.root, &captured);
        drop(client);
        h.stop().await;
    })
    .await
    .expect("bounded speaker and Ready handoff over owned fixture service");
}

#[tokio::test(flavor = "current_thread")]
async fn skipped_and_applied_without_labels_keep_distinct_durable_stages() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let mut observed = Vec::new();
        for outcome in [LocalAsrOutcome::Complete, LocalAsrOutcome::NoModel] {
            let captured = capture(&mut client, &h.root, &capture_proof).await;
            let active = Claim::new(&mut client, &captured).await;
            let no_model = outcome == LocalAsrOutcome::NoModel;
            let captions = if no_model {
                vec![]
            } else {
                vec![LocalAsrCaption {
                    start_ms: 0,
                    end_ms: 200,
                    text: "speech with no assigned speaker".into(),
                }]
            };
            let selected = select_source(&mut client, &active, outcome, captions).await;
            let (result, expected) = if no_model {
                let reason = SpeakerSkipReason::EmptyTranscript;
                (
                    SpeakerResultBody::Skipped { reason },
                    SpeakerResultOutcome::Skipped { reason },
                )
            } else {
                let items = transcript(
                    &mut client,
                    &active,
                    &selected,
                    DesktopTranscriptRepresentation::Raw,
                )
                .await;
                (
                    SpeakerResultBody::Applied {
                        assignments: vec![SpeakerAssignment {
                            segment_id: items[0].segment_id,
                            cluster: None,
                        }],
                    },
                    SpeakerResultOutcome::Applied,
                )
            };
            let commit = stage(&mut client, &active, &selected, &result).await;
            let reply = client
                .post_process_speakers(&active.worker, commit)
                .await
                .unwrap()
                .data;
            let PostProcessSpeakersReply::Committed(receipt) = reply else {
                panic!("expected durable result even when labels do not change")
            };
            assert_eq!(receipt.outcome, expected);
            assert_eq!(receipt.changed_segments, 0);
            assert_same_source_rotated(&selected, &receipt.lease);
            assert_eq!(
                selected.input.transcript_revision,
                receipt.lease.input.transcript_revision
            );
            let (lease, stage) = if no_model {
                let ready = client
                    .mark_post_process_ready(&active.worker, ready_request(&active, &receipt.lease))
                    .await
                    .unwrap()
                    .data;
                (
                    ready.lease,
                    PostProcessStage::Ready {
                        local_asr: outcome,
                        speakers: expected,
                        cleanup: false,
                        generated_summary_revision: None,
                    },
                )
            } else {
                (
                    receipt.lease,
                    PostProcessStage::Speakers {
                        local_asr: outcome,
                        outcome: expected,
                    },
                )
            };
            observed.push((captured, lease.input, stage, no_model));
        }
        drop(client);
        h.stop().await;
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        for (captured, input, stage, ready) in observed {
            let current = Claim::new(&mut client, &captured).await;
            assert_eq!(current.lease.input, input);
            assert_eq!(current.job.stage, stage);
            assert_eq!(
                header(&mut client, captured.meeting_id).await.status,
                if ready { "ready" } else { "processing" }
            );
            raw_unchanged(&h.root, &captured);
        }
        drop(client);
        h.stop().await;
    })
    .await
    .expect("bounded skipped and no-label speaker outcomes over owned fixture service");
}

use super::*;
use bazza_service_api::postprocess_cleanup::*;
use sha2::{Digest, Sha256};

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn cleanup_request(
    proof: &WorkerProof,
    lease: &PostProcessJobLease,
    fence: &WorkerSecret,
    operation: PostProcessCleanupOperation,
) -> PostProcessCleanupRequest {
    PostProcessCleanupRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: fence.clone(),
        operation_id: Uuid::new_v4(),
        operation,
    }
}

async fn upload(
    client: &mut ServiceClient,
    registered: &RegisteredWorker,
    proof: &WorkerProof,
    lease: &PostProcessJobLease,
    fence: &WorkerSecret,
    body: &str,
) -> Uuid {
    let upload_id = Uuid::new_v4();
    let begin = cleanup_request(
        proof,
        lease,
        fence,
        PostProcessCleanupOperation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: sha(body),
        },
    );
    client
        .post_process_cleanup(registered, begin)
        .await
        .unwrap();
    let mut offset = 0;
    let mut sequence = 1;
    while offset < body.len() {
        let mut end = (offset + 8192).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let text = &body[offset..end];
        let request = cleanup_request(
            proof,
            lease,
            fence,
            PostProcessCleanupOperation::Append {
                upload_id,
                sequence,
                byte_offset: offset as u64,
                text: text.into(),
                sha256: sha(text),
            },
        );
        let reply = client
            .post_process_cleanup(registered, request.clone())
            .await
            .unwrap();
        let duplicate = client
            .post_process_cleanup(registered, request)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(reply.data).unwrap(),
            serde_json::to_value(duplicate.data).unwrap(),
            "append replay keeps historical progress"
        );
        offset = end;
        sequence += 1;
    }
    upload_id
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_large_result_lost_ack_rotates_checkpoint_and_survives_reopen() {
    timeout(Duration::from_secs(45), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let descriptor = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap()
            .data
            .items
            .remove(0);
        assert!(!descriptor.input.checkpoint_revision.is_empty());
        let request = claim(&proof, descriptor.clone());
        let fence = request.fence.clone();
        let original = client
            .claim_post_process_job(&registered, request)
            .await
            .unwrap()
            .data;
        let media = client
            .acquire_post_process_media(
                &registered,
                &h.root,
                WorkerMediaInputRequest {
                    proof: proof.clone(),
                    lease: original.clone(),
                    fence: fence.clone(),
                    expected_provider: descriptor.provider.clone(),
                },
            )
            .await
            .unwrap();
        let raw = client
            .post_process_transcript(&registered, transcript_input(&proof, &original, &fence))
            .await
            .unwrap()
            .data;
        let segment_id = raw.items[0].segment_id;
        let text = format!(
            "  {}\0 exact whitespace  ",
            "SapphireCleanup Орион🦀 ".repeat(3000)
        );
        let body = serde_json::to_string(&vec![
            PostProcessCleanupUpdate {
                segment_id,
                text: "intermediate".into(),
            },
            PostProcessCleanupUpdate {
                segment_id,
                text: text.clone(),
            },
            PostProcessCleanupUpdate {
                segment_id: 9_007_199_254_740_990,
                text: "scoped missing ID".into(),
            },
        ])
        .unwrap();
        assert!(body.len() > 65_024);
        let upload_id = upload(&mut client, &registered, &proof, &original, &fence, &body).await;
        let historical_status = cleanup_request(
            &proof,
            &original,
            &fence,
            PostProcessCleanupOperation::Status { upload_id },
        );
        let staged_status = client
            .post_process_cleanup(&registered, historical_status.clone())
            .await
            .unwrap()
            .data;
        let renewed = client
            .renew_post_process_job(
                &registered,
                RenewPostProcessJobRequest {
                    proof: proof.clone(),
                    lease: original.clone(),
                    fence: fence.clone(),
                    operation_id: Uuid::new_v4(),
                    ttl_seconds: 30,
                },
            )
            .await
            .unwrap()
            .data;
        assert_eq!(renewed.input, original.input);
        let commit = cleanup_request(
            &proof,
            &renewed,
            &fence,
            PostProcessCleanupOperation::Commit { upload_id },
        );
        discard_committed_reply(
            &h.root,
            Request::PostProcessCleanup(Box::new(commit.clone())),
        )
        .await;
        let result = client
            .post_process_cleanup(&registered, commit.clone())
            .await
            .unwrap();
        let PostProcessCleanupReply::Committed(receipt) = result.data else {
            panic!("commit receipt expected")
        };
        assert_eq!(receipt.changed_segments, 1);
        assert_eq!(receipt.outcome, PostProcessCleanupOutcome::Applied);
        assert_eq!(receipt.lease.expires_at, renewed.expires_at);
        assert_eq!(receipt.lease.claim_id, renewed.claim_id);
        assert_ne!(receipt.lease.job_revision, renewed.job_revision);
        assert_ne!(
            receipt.lease.input.checkpoint_revision,
            renewed.input.checkpoint_revision
        );
        assert_ne!(
            receipt.lease.input.transcript_revision,
            renewed.input.transcript_revision
        );
        assert_eq!(receipt.lease.input.capture_id, renewed.input.capture_id);
        assert_eq!(receipt.lease.input.source_id, renewed.input.source_id);
        assert_eq!(
            receipt.lease.input.lifecycle_revision,
            renewed.input.lifecycle_revision
        );
        let inert_status = client
            .post_process_cleanup(&registered, historical_status)
            .await
            .unwrap()
            .data;
        assert_eq!(
            inert_status, staged_status,
            "an exact historical status is metadata, not current authority"
        );
        remote(
            client
                .post_process_media(
                    &registered,
                    PostProcessMediaRequest {
                        proof: proof.clone(),
                        lease: receipt.lease.clone(),
                        fence: fence.clone(),
                        owner_nonce: media.grant().owner_nonce,
                        expected_provider: descriptor.provider.clone(),
                    },
                )
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let next_media = client
            .acquire_post_process_media(
                &registered,
                &h.root,
                WorkerMediaInputRequest {
                    proof: proof.clone(),
                    lease: receipt.lease.clone(),
                    fence: fence.clone(),
                    expected_provider: descriptor.provider.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            next_media.grant().input.checkpoint_revision,
            receipt.lease.input.checkpoint_revision
        );
        drop(next_media);
        drop(media);
        let after = header(&mut client, captured.meeting_id).await;
        assert_eq!(after.status, "processing");
        assert_eq!(
            after.transcript_revision,
            receipt.lease.input.transcript_revision
        );
        raw_unchanged(&h.root, &captured);
        let mut stale = cleanup_request(
            &proof,
            &renewed,
            &fence,
            PostProcessCleanupOperation::Status { upload_id },
        );
        remote(
            client
                .post_process_cleanup(&registered, stale.clone())
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        stale.operation_id = commit.operation_id;
        remote(
            client
                .post_process_cleanup(&registered, stale)
                .await
                .unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );
        remote(
            client
                .post_process_transcript(&registered, transcript_input(&proof, &renewed, &fence))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let mut read = transcript_input(&proof, &receipt.lease, &fence);
        read.page.representation = DesktopTranscriptRepresentation::Effective;
        let mut effective = String::new();
        loop {
            let page = client
                .post_process_transcript(&registered, read.clone())
                .await
                .unwrap()
                .data;
            for fragment in page.items {
                assert_eq!(
                    fragment.effective_origin,
                    bazza_service_api::content::DesktopEffectiveOrigin::Automatic
                );
                effective.push_str(&fragment.text);
            }
            if let Some(cursor) = page.next {
                read.page.cursor = Some(cursor);
            } else {
                break;
            }
        }
        assert_eq!(effective, text);
        let original_raw = client
            .post_process_transcript(
                &registered,
                transcript_input(&proof, &receipt.lease, &fence),
            )
            .await
            .unwrap()
            .data;
        assert_eq!(original_raw.items[0].text, raw.items[0].text);
        let replay = client
            .post_process_cleanup(&registered, commit.clone())
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(replay.data).unwrap(),
            serde_json::to_value(PostProcessCleanupReply::Committed(receipt.clone())).unwrap()
        );
        let failed = client
            .fail_post_process_job(
                &registered,
                FailPostProcessJobRequest {
                    proof: proof.clone(),
                    lease: receipt.lease.clone(),
                    operation_id: Uuid::new_v4(),
                    fence: fence.clone(),
                    error_code: PostProcessFailureCode::PostProcessFailed,
                },
            )
            .await
            .unwrap()
            .data;
        drop(client);
        h.stop().await;
        let repository =
            Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        let mut durable_read = transcript_input(&proof, &receipt.lease, &fence).page;
        let source = repository
            .desktop_transcript(std::process::id(), durable_read.clone())
            .unwrap()
            .data;
        assert_eq!(
            source.state.revision,
            receipt.lease.input.transcript_revision
        );
        assert_eq!(source.items[0].text, "Original raw caption Орион");
        durable_read.representation = DesktopTranscriptRepresentation::Effective;
        let mut durable_effective = String::new();
        loop {
            let page = repository
                .desktop_transcript(std::process::id(), durable_read.clone())
                .unwrap()
                .data;
            for fragment in page.items {
                assert!(fragment.has_clean_text && !fragment.clean_text_manual);
                durable_effective.push_str(&fragment.text);
            }
            if let Some(cursor) = page.next {
                durable_read.cursor = Some(cursor);
            } else {
                break;
            }
        }
        assert_eq!(durable_effective, text);
        assert_eq!(
            repository.data().job(&captured.job_id).unwrap().revision,
            failed.job_revision
        );
        raw_unchanged(&h.root, &captured);
        drop(repository);
        h.start().await;
        raw_expect_error(
            &h.root,
            Request::PostProcessCleanup(Box::new(commit)),
            ErrorCode::PermissionDenied,
        )
        .await;
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_noop_advances_checkpoint_without_transcript_revision_or_media_changes() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
        let job = client
            .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap()
            .data
            .items
            .remove(0);
        let claim = claim(&proof, job);
        let fence = claim.fence.clone();
        let lease = client
            .claim_post_process_job(&registered, claim)
            .await
            .unwrap()
            .data;
        let body = serde_json::to_string(&vec![PostProcessCleanupUpdate {
            segment_id: 9_007_199_254_740_990,
            text: String::new(),
        }])
        .unwrap();
        let upload_id = upload(&mut client, &registered, &proof, &lease, &fence, &body).await;
        let result = client
            .post_process_cleanup(
                &registered,
                cleanup_request(
                    &proof,
                    &lease,
                    &fence,
                    PostProcessCleanupOperation::Commit { upload_id },
                ),
            )
            .await
            .unwrap()
            .data;
        let PostProcessCleanupReply::Committed(receipt) = result else {
            panic!("commit receipt expected")
        };
        assert_eq!(receipt.outcome, PostProcessCleanupOutcome::Noop);
        assert_eq!(receipt.changed_segments, 0);
        assert_eq!(
            receipt.lease.input.transcript_revision,
            lease.input.transcript_revision
        );
        assert_ne!(
            receipt.lease.input.checkpoint_revision,
            lease.input.checkpoint_revision
        );
        assert_ne!(receipt.lease.job_revision, lease.job_revision);
        assert_eq!(receipt.lease.expires_at, lease.expires_at);
        client
            .post_process_transcript(
                &registered,
                transcript_input(&proof, &receipt.lease, &fence),
            )
            .await
            .unwrap();
        raw_unchanged(&h.root, &captured);
        drop(client);
        h.stop().await;
    })
    .await
    .unwrap();
}

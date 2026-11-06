use super::*;
use bazza_service_api::{
    content::DesktopEffectiveOrigin,
    fixture_read::{FixtureReadRequest, FixtureReadTool},
    postprocess_cleanup::{PostProcessCleanupReply, PostProcessCleanupUpdate},
    postprocess_source::*,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) struct Claim {
    pub(super) worker: RegisteredWorker,
    pub(super) proof: WorkerProof,
    pub(super) job: PostProcessJob,
    pub(super) lease: PostProcessJobLease,
    pub(super) fence: WorkerSecret,
}

impl Claim {
    pub(super) async fn new(client: &mut ServiceClient, captured: &Capture) -> Self {
        let (worker, proof) = worker(client, WorkerCapability::PostProcess).await;
        let job = client
            .post_process_jobs(&worker, PostProcessJobsRequest::new(proof.clone()))
            .await
            .unwrap()
            .data
            .items
            .into_iter()
            .find(|job| job.job_id == captured.job_id)
            .expect("captured meeting has an eligible job");
        let request = claim(&proof, job.clone());
        let fence = request.fence.clone();
        let lease = client
            .claim_post_process_job(&worker, request)
            .await
            .unwrap()
            .data;
        Self {
            worker,
            proof,
            job,
            lease,
            fence,
        }
    }

    pub(super) fn source(
        &self,
        lease: &PostProcessJobLease,
        operation: PostProcessSourceOperation,
    ) -> PostProcessSourceRequest {
        PostProcessSourceRequest {
            proof: self.proof.clone(),
            lease: lease.clone(),
            fence: self.fence.clone(),
            operation_id: Uuid::new_v4(),
            operation,
        }
    }

    fn begin(&self, upload_id: Uuid, body: &str) -> PostProcessSourceRequest {
        self.source(
            &self.lease,
            PostProcessSourceOperation::Begin {
                upload_id,
                total_bytes: body.len() as u64,
                sha256: sha(body),
            },
        )
    }

    pub(super) async fn renew(
        &self,
        client: &mut ServiceClient,
        lease: &PostProcessJobLease,
    ) -> PostProcessJobLease {
        client
            .renew_post_process_job(
                &self.worker,
                RenewPostProcessJobRequest {
                    proof: self.proof.clone(),
                    lease: lease.clone(),
                    fence: self.fence.clone(),
                    operation_id: Uuid::new_v4(),
                    ttl_seconds: 30,
                },
            )
            .await
            .unwrap()
            .data
    }

    pub(super) fn read(&self, lease: &PostProcessJobLease) -> PostProcessTranscriptRequest {
        transcript_input(&self.proof, lease, &self.fence)
    }
}

pub(super) fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub(super) async fn upload(client: &mut ServiceClient, claim: &Claim, body: &str) -> Uuid {
    let upload_id = Uuid::new_v4();
    let begin = client
        .post_process_source(&claim.worker, claim.begin(upload_id, body))
        .await
        .unwrap()
        .data;
    let PostProcessSourceReply::Upload(begin) = begin else {
        panic!("begin must return upload metadata")
    };
    assert_eq!(begin.received_bytes, 0);
    let mut offset = 0;
    let mut sequence = 1;
    while offset < body.len() {
        let mut end = (offset + 8192).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let text = &body[offset..end];
        let append = claim.source(
            &claim.lease,
            PostProcessSourceOperation::Append {
                upload_id,
                sequence,
                byte_offset: offset as u64,
                text: text.into(),
                sha256: sha(text),
            },
        );
        let reply = client
            .post_process_source(&claim.worker, append.clone())
            .await
            .unwrap()
            .data;
        let PostProcessSourceReply::Upload(status) = &reply else {
            panic!("append must return upload metadata")
        };
        assert_eq!(status.received_bytes, end as u64);
        if sequence == 1 {
            let duplicate = client
                .post_process_source(&claim.worker, append)
                .await
                .unwrap()
                .data;
            assert_eq!(duplicate, reply);
        }
        offset = end;
        sequence += 1;
    }
    upload_id
}

// Legacy setting is inspected only after stopping the owned service.
fn saved_stt(root: &Path, meeting_id: Uuid) -> Value {
    let conn = Connection::open_with_flags(root.join("bazza.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let value: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [format!("stt:{meeting_id}")],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&value).unwrap()
}

pub(super) async fn cleanup_after_source(
    client: &mut ServiceClient,
    active: &Claim,
    lease: &PostProcessJobLease,
    segment_id: i64,
) -> PostProcessJobLease {
    let clean = "  Орион после очистки  ";
    let body = serde_json::to_string(&[PostProcessCleanupUpdate {
        segment_id,
        text: clean.into(),
    }])
    .unwrap();
    let upload_id = Uuid::new_v4();
    let sha256 = sha(&body);
    for operation in [
        PostProcessSourceOperation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: sha256.clone(),
        },
        PostProcessSourceOperation::Append {
            upload_id,
            sequence: 1,
            byte_offset: 0,
            text: body,
            sha256,
        },
    ] {
        client
            .post_process_cleanup(&active.worker, active.source(lease, operation))
            .await
            .unwrap();
    }
    let commit = active.source(lease, PostProcessSourceOperation::Commit { upload_id });
    let reply = client
        .post_process_cleanup(&active.worker, commit)
        .await
        .unwrap()
        .data;
    let PostProcessCleanupReply::Committed(receipt) = reply else {
        panic!("expected cleanup commit against the new source")
    };
    assert_eq!(receipt.changed_segments, 1);
    assert_eq!(receipt.lease.input.source_id, lease.input.source_id);
    let mut read = active.read(&receipt.lease);
    read.page.representation = DesktopTranscriptRepresentation::Effective;
    let page = client
        .post_process_transcript(&active.worker, read)
        .await
        .unwrap()
        .data;
    assert_eq!(
        page.items[0].effective_origin,
        DesktopEffectiveOrigin::Automatic
    );
    assert_eq!(page.items[0].text, clean);
    receipt.lease
}

#[tokio::test(flavor = "current_thread")]
async fn local_source_spans_frames_and_rotates_lease_with_exact_retry_and_restart() {
    timeout(Duration::from_secs(45), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let captured = capture(&mut client, &h.root, &capture_proof).await;
        let active = Claim::new(&mut client, &captured).await;
        let before = header(&mut client, captured.meeting_id).await;
        let media_request = |lease| WorkerMediaInputRequest {
            proof: active.proof.clone(),
            lease,
            fence: active.fence.clone(),
            expected_provider: active.job.provider.clone(),
        };
        let old_media = client
            .acquire_post_process_media(
                &active.worker,
                &h.root,
                media_request(active.lease.clone()),
            )
            .await
            .unwrap();
        let captions: Vec<_> = (0..4)
            .map(|i| LocalAsrCaption {
                start_ms: 100 + 200 * i,
                end_ms: if i == 3 { 700 } else { 200 + 200 * i },
                text: format!(
                    "  [{i}] {}\0 exact suffix  ",
                    "SapphireSource Орион🦀 ".repeat(1500)
                ),
            })
            .collect();
        let result = LocalAsrBody {
            model: "fixture-local-actual-fallback".into(),
            language: Some("en".into()),
            outcome: LocalAsrOutcome::Complete,
            captions,
        };
        let body = serde_json::to_string(&result).unwrap();
        assert!(
            body.len() > 128 * 1024,
            "result exceeds the entire private frame budget"
        );
        let upload_id = upload(&mut client, &active, &body).await;
        let cross_kind = active.source(
            &active.lease,
            PostProcessSourceOperation::Status { upload_id },
        );
        remote(
            client
                .post_process_cleanup(&active.worker, cross_kind)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let renewed = active.renew(&mut client, &active.lease).await;
        assert_eq!(renewed.input, active.lease.input);
        let commit = active.source(&renewed, PostProcessSourceOperation::Commit { upload_id });
        discard_committed_reply(
            &h.root,
            Request::PostProcessSource(Box::new(commit.clone())),
        )
        .await;
        let reply = client
            .post_process_source(&active.worker, commit.clone())
            .await
            .unwrap()
            .data;
        let PostProcessSourceReply::Committed(receipt) = reply else {
            panic!("commit must return a source receipt")
        };
        assert_eq!(receipt.outcome, LocalAsrOutcome::Complete);
        assert_eq!(receipt.segment_count as usize, result.captions.len());
        assert_eq!(receipt.lease.expires_at, renewed.expires_at);
        assert_eq!(receipt.lease.claim_id, renewed.claim_id);
        assert_eq!(receipt.lease.input.capture_id, renewed.input.capture_id);
        assert_eq!(
            receipt.lease.input.capture_generation,
            renewed.input.capture_generation
        );
        assert_eq!(
            receipt.lease.input.lifecycle_revision,
            renewed.input.lifecycle_revision
        );
        assert_ne!(
            receipt.lease.input.checkpoint_revision,
            renewed.input.checkpoint_revision
        );
        assert_ne!(
            receipt.lease.input.transcript_revision,
            renewed.input.transcript_revision
        );
        assert_ne!(receipt.lease.input.source_id, renewed.input.source_id);
        assert_ne!(receipt.lease.job_revision, renewed.job_revision);
        let expected_receipt = PostProcessSourceReply::Committed(receipt.clone());
        let encoded = serde_json::to_string(&expected_receipt).unwrap();
        assert!(!encoded.contains("SapphireSource"));
        assert!(!encoded.contains(&result.model));
        let duplicate = client
            .post_process_source(&active.worker, commit.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(duplicate, expected_receipt);

        remote(
            client
                .post_process_transcript(&active.worker, active.read(&renewed))
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        for lease in [&renewed, &receipt.lease] {
            remote(
                client
                    .post_process_media(
                        &active.worker,
                        PostProcessMediaRequest {
                            proof: active.proof.clone(),
                            lease: lease.clone(),
                            fence: active.fence.clone(),
                            owner_nonce: old_media.grant().owner_nonce,
                            expected_provider: active.job.provider.clone(),
                        },
                    )
                    .await
                    .unwrap_err(),
                ErrorCode::PermissionDenied,
            );
        }
        let new_media = client
            .acquire_post_process_media(
                &active.worker,
                &h.root,
                media_request(receipt.lease.clone()),
            )
            .await
            .unwrap();
        assert_eq!(new_media.grant().provider, active.job.provider);
        assert_eq!(new_media.grant().input, receipt.lease.input);
        drop((old_media, new_media));
        let after = header(&mut client, captured.meeting_id).await;
        assert_eq!(after.status, "processing");
        assert_eq!(after.lifecycle_revision, before.lifecycle_revision);
        assert_eq!(
            after.transcript_revision,
            receipt.lease.input.transcript_revision
        );
        let live = active.renew(&mut client, &receipt.lease).await;
        assert_eq!(live.input, receipt.lease.input);
        let forbidden_second = active.source(
            &live,
            PostProcessSourceOperation::Begin {
                upload_id: Uuid::new_v4(),
                total_bytes: body.len() as u64,
                sha256: sha(&body),
            },
        );
        remote(
            client
                .post_process_source(&active.worker, forbidden_second)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );

        let mut request = active.read(&live);
        let mut found = vec![String::new(); result.captions.len()];
        let mut first_segment = None;
        loop {
            let page = client
                .post_process_transcript(&active.worker, request.clone())
                .await
                .unwrap()
                .data;
            assert_eq!(page.source_id, live.input.source_id);
            for fragment in page.items {
                first_segment.get_or_insert(fragment.segment_id);
                let i = fragment.segment_index as usize;
                let caption = &result.captions[i];
                assert_eq!(
                    (fragment.start_ms, fragment.end_ms),
                    (caption.start_ms, caption.end_ms)
                );
                assert_eq!(fragment.byte_offset as usize, found[i].len());
                assert!(!fragment.has_clean_text);
                found[i].push_str(&fragment.text);
            }
            let Some(cursor) = page.next else { break };
            request.page.cursor = Some(cursor);
        }
        assert_eq!(
            found,
            result
                .captions
                .iter()
                .map(|caption| caption.text.clone())
                .collect::<Vec<_>>()
        );
        let archived = client
            .desktop_transcript(DesktopTranscriptRequest {
                source_id: renewed.input.source_id.clone(),
                ..active.read(&live).page
            })
            .await
            .unwrap()
            .data;
        assert_eq!(archived.source_id, renewed.input.source_id);
        assert_eq!(archived.items[0].text, "Original raw caption Орион");
        let latest =
            cleanup_after_source(&mut client, &active, &live, first_segment.unwrap()).await;
        raw_unchanged(&h.root, &captured);

        drop(client);
        h.stop().await;
        let expected = json!({
            "provider": "local",
            "model": result.model,
            "language": result.language,
            "status": "complete",
            "error": null,
            "captions": result.captions.iter().map(|caption| json!({
                "start_ms": caption.start_ms,
                "end_ms": caption.end_ms,
                "text": caption.text,
                "is_final": true,
            })).collect::<Vec<_>>(),
        });
        assert_eq!(saved_stt(&h.root, captured.meeting_id), expected);
        h.start().await;
        raw_expect_error(
            &h.root,
            Request::PostProcessSource(Box::new(commit)),
            ErrorCode::PermissionDenied,
        )
        .await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let reclaimed = Claim::new(&mut client, &captured).await;
        assert_ne!(reclaimed.lease.claim_id, latest.claim_id);
        assert_eq!(reclaimed.lease.input, latest.input);
        assert_eq!(
            reclaimed.job.stage,
            PostProcessStage::AutomaticCleanup {
                local_asr: Some(LocalAsrOutcome::Complete)
            }
        );
        let read = client
            .post_process_transcript(&reclaimed.worker, reclaimed.read(&reclaimed.lease))
            .await
            .unwrap()
            .data;
        assert!(result.captions[0].text.starts_with(&read.items[0].text));
        assert_eq!(
            header(&mut client, captured.meeting_id).await.status,
            "processing"
        );
        drop(client);
        h.stop().await;
    })
    .await
    .expect("bounded local-ASR source handoff over owned fixture service");
}

#[tokio::test(flavor = "current_thread")]
async fn local_source_distinguishes_empty_outcomes_and_denies_soniox() {
    timeout(Duration::from_secs(30), async {
        let mut h = Harness::new();
        h.start().await;
        let mut client = ServiceClient::connect(&h.root).await.unwrap();
        let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
        let cloud = capture_with_provider(
            &mut client,
            &h.root,
            &capture_proof,
            CaptureProvider::Soniox,
        )
        .await;
        let active = Claim::new(&mut client, &cloud).await;
        let complete = LocalAsrBody {
            model: "fixture-local-selected".into(),
            language: None,
            outcome: LocalAsrOutcome::Complete,
            captions: Vec::new(),
        };
        let body = serde_json::to_string(&complete).unwrap();
        let denied = active.begin(Uuid::new_v4(), &body);
        remote(
            client
                .post_process_source(&active.worker, denied)
                .await
                .unwrap_err(),
            ErrorCode::PermissionDenied,
        );
        let original = client
            .post_process_transcript(&active.worker, active.read(&active.lease))
            .await
            .unwrap()
            .data;
        assert_eq!(original.source_id, active.lease.input.source_id);
        assert_eq!(original.items[0].text, "Original raw caption Орион");
        raw_unchanged(&h.root, &cloud);
        let mut observed = Vec::new();
        for outcome in [LocalAsrOutcome::Complete, LocalAsrOutcome::NoModel] {
            let captured = capture(&mut client, &h.root, &capture_proof).await;
            let active = Claim::new(&mut client, &captured).await;
            let result = LocalAsrBody {
                outcome,
                ..complete.clone()
            };
            let body = serde_json::to_string(&result).unwrap();
            let upload_id = upload(&mut client, &active, &body).await;
            let commit = active.source(
                &active.lease,
                PostProcessSourceOperation::Commit { upload_id },
            );
            let reply = client
                .post_process_source(&active.worker, commit)
                .await
                .unwrap()
                .data;
            let PostProcessSourceReply::Committed(receipt) = reply else {
                panic!("commit must return a source receipt")
            };
            assert_eq!(receipt.outcome, outcome);
            assert_eq!(receipt.segment_count, 0);
            assert!(receipt.lease.input.source_id.is_some());
            assert_ne!(receipt.lease.input.source_id, active.lease.input.source_id);
            let empty = client
                .post_process_transcript(&active.worker, active.read(&receipt.lease))
                .await
                .unwrap()
                .data;
            assert!(empty.items.is_empty());
            assert!(empty.next.is_none());
            let preview = client
                .fixture_read(FixtureReadRequest {
                    tool: FixtureReadTool::Transcript,
                    arguments: json!({"meeting_id": captured.meeting_id}),
                })
                .await
                .unwrap()
                .output;
            assert_eq!(preview["data"]["segments"], json!([]));
            let source = &preview["data"]["source"];
            assert_eq!(source["origin"], "captured");
            assert_eq!(source["provider_label"], "local");
            assert_eq!(
                source["model_label"],
                match outcome {
                    LocalAsrOutcome::Complete => json!(complete.model),
                    LocalAsrOutcome::NoModel => Value::Null,
                }
            );
            assert_eq!(
                header(&mut client, captured.meeting_id).await.status,
                "processing"
            );
            observed.push((captured.meeting_id, outcome));
        }
        drop(client);
        h.stop().await;
        let cloud_stt = saved_stt(&h.root, cloud.meeting_id);
        assert_eq!(cloud_stt["provider"], "soniox");
        assert_eq!(cloud_stt["model"], "fixture-soniox");
        for (meeting_id, outcome) in observed {
            let saved = saved_stt(&h.root, meeting_id);
            assert_eq!(saved["provider"], "local");
            assert_eq!(saved["model"], complete.model);
            assert_eq!(saved["captions"], json!([]));
            match outcome {
                LocalAsrOutcome::Complete => {
                    assert_eq!(saved["status"], "complete");
                    assert_eq!(saved["error"], Value::Null);
                }
                LocalAsrOutcome::NoModel => {
                    assert_eq!(saved["status"], "incomplete");
                    assert_eq!(saved["error"], "No local speech model downloaded");
                }
            }
        }
    })
    .await
    .expect("bounded provider and empty ASR result checks over owned fixture service");
}

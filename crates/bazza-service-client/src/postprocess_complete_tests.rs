use super::*;
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::{PostProcessJobLease, PostProcessSourceFence},
    postprocess_complete::*,
};
use std::time::SystemTime;

#[test]
fn completion_accepts_a_terminal_receipt_and_rejects_a_rebound_source() {
    run(async {
        let mut server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            for forged in [false, true] {
                let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let Request::CompleteFixturePostProcess(request) = &frame.request else {
                    panic!("expected the dedicated fixture completion operation");
                };
                assert_eq!(request.lease.input.source_id.as_deref(), Some("source"));
                let mut input = request.lease.input.clone();
                if forged {
                    input.source_id = Some("rebound-source".into());
                }
                let data = CompleteFixturePostProcessReceipt {
                    operation_id: request.operation_id,
                    claim_id: request.lease.claim_id,
                    worker_session_id: request.proof.session_id,
                    library_epoch: descriptor.library_epoch,
                    job_id: request.lease.job_id.clone(),
                    meeting_id: request.lease.meeting_id,
                    job_revision: "terminal-revision".into(),
                    input,
                    cleanup: FixtureCleanupOutcome::Accepted,
                    embeddings: FixtureExcludedStage::OutsideFixtureScope,
                    summary: FixtureSummaryOutcome::OutsideFixtureScope {},
                    webhook: FixtureExcludedStage::OutsideFixtureScope,
                    change_sequence: request.lease.change_sequence + 1,
                    dedup_until: request.lease.dedup_until,
                };
                let response = Reply::FixturePostProcessComplete(Box::new(DesktopEnvelope {
                    library_id: descriptor.library_id,
                    library_epoch: descriptor.library_epoch,
                    data,
                }));
                write_frame(&mut stream, &reply(&frame, response))
                    .await
                    .unwrap();
            }
            assert!(read_frame::<RequestEnvelope>(&mut stream).await.is_err());
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let session = Uuid::new_v4();
        let epoch = server.descriptor.library_epoch;
        let now = SystemTime::now();
        let worker = RegisteredWorker {
            session_id: session,
            library_epoch: epoch,
            peer_pid: std::process::id(),
            capabilities: vec![WorkerCapability::PostProcess],
            expires_at: (now + Duration::from_secs(120)).into(),
        };
        let mut request = CompleteFixturePostProcessRequest {
            proof: WorkerProof {
                session_id: session,
                secret: WorkerSecret::random(),
                library_epoch: epoch,
            },
            lease: PostProcessJobLease {
                operation_id: Uuid::new_v4(),
                claim_id: Uuid::new_v4(),
                worker_session_id: session,
                library_epoch: epoch,
                job_id: "fixture-post-process".into(),
                meeting_id: Uuid::new_v4(),
                job_revision: "running-revision".into(),
                input: PostProcessSourceFence {
                    capture_id: Uuid::new_v4(),
                    capture_generation: Uuid::new_v4(),
                    checkpoint_revision: "ready-cleanup-checkpoint".into(),
                    lifecycle_revision: "ready-life".into(),
                    transcript_revision: "cleaned-transcript".into(),
                    source_id: None,
                },
                expires_at: (now - Duration::from_secs(1)).into(),
                change_sequence: 10,
                dedup_until: (now + Duration::from_secs(60)).into(),
            },
            fence: WorkerSecret::random(),
            operation_id: Uuid::new_v4(),
        };
        assert!(matches!(
            client
                .complete_fixture_post_process(&worker, request.clone())
                .await,
            Err(ClientError::Remote(RpcError {
                code: ErrorCode::InvalidArgument,
                ..
            }))
        ));
        request.lease.input.source_id = Some("source".into());
        let data = client
            .complete_fixture_post_process(&worker, request.clone())
            .await
            .unwrap()
            .data;
        assert_eq!(data.input, request.lease.input);
        assert_eq!(data.job_revision, "terminal-revision");
        assert!(matches!(
            client.complete_fixture_post_process(&worker, request).await,
            Err(ClientError::Protocol)
        ));
        assert_no_reuse(&mut client).await;
        (&mut server.task).await.unwrap();
    });
}

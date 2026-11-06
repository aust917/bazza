use super::*;
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::{PostProcessJobLease, PostProcessSourceFence},
    postprocess_speakers::*,
};
use std::time::SystemTime;

#[test]
fn speaker_upload_pins_local_source_and_closes_on_a_forged_transcript_revision() {
    run(async {
        let mut server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessSpeakers(request) = &frame.request else {
                panic!("expected the dedicated speaker operation");
            };
            assert!(request.lease.input.source_id.is_some());
            let mut lease = request.lease.clone();
            lease.operation_id = request.operation_id;
            lease.job_revision = "speakers-job".into();
            lease.input.checkpoint_revision = "speakers-checkpoint".into();
            lease.change_sequence += 1;
            // The forged receipt claims a label change without a transcript revision.
            let response = Reply::PostProcessSpeakers(Box::new(DesktopEnvelope {
                library_id: descriptor.library_id,
                library_epoch: descriptor.library_epoch,
                data: PostProcessSpeakersReply::Committed(PostProcessSpeakersReceipt {
                    operation_id: request.operation_id,
                    upload_id: request.operation.upload_id(),
                    lease,
                    outcome: SpeakerResultOutcome::Applied,
                    changed_segments: 1,
                }),
            }));
            write_frame(&mut stream, &reply(&frame, response))
                .await
                .unwrap();
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
        let mut request = PostProcessSpeakersRequest {
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
                job_id: "local-speakers-job".into(),
                meeting_id: Uuid::new_v4(),
                job_revision: "local-asr-job".into(),
                input: PostProcessSourceFence {
                    capture_id: Uuid::new_v4(),
                    capture_generation: Uuid::new_v4(),
                    checkpoint_revision: "local-asr-checkpoint".into(),
                    lifecycle_revision: "processing-life".into(),
                    transcript_revision: "local-transcript".into(),
                    source_id: None,
                },
                expires_at: (now + Duration::from_secs(60)).into(),
                change_sequence: 5,
                dedup_until: (now + Duration::from_secs(7 * 24 * 60 * 60)).into(),
            },
            fence: WorkerSecret::random(),
            operation_id: Uuid::new_v4(),
            operation: PostProcessSpeakersOperation::Commit {
                upload_id: Uuid::new_v4(),
            },
        };
        assert!(matches!(
            client.post_process_speakers(&worker, request.clone()).await,
            Err(ClientError::Remote(RpcError {
                code: ErrorCode::InvalidArgument,
                ..
            }))
        ));
        request.lease.input.source_id = Some("local-source".into());
        assert!(matches!(
            client.post_process_speakers(&worker, request).await,
            Err(ClientError::Protocol)
        ));
        assert_no_reuse(&mut client).await;
        (&mut server.task).await.unwrap();
    });
}

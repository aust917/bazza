use super::*;
use bazza_service_api::{
    desktop::DesktopEnvelope,
    postprocess::{PostProcessJobLease, PostProcessSourceFence},
    postprocess_source::{
        LocalAsrOutcome, PostProcessSourceOperation, PostProcessSourceReceipt,
        PostProcessSourceReply, PostProcessSourceRequest,
    },
};
use std::time::SystemTime;

#[test]
fn postprocess_source_rejects_foreign_pid_and_poisons_on_a_forged_source_receipt() {
    run(async {
        let mut server = fake_service(|mut stream, descriptor| async move {
            accept_hello(&mut stream, &descriptor).await;
            let frame: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::PostProcessSource(request) = &frame.request else {
                panic!("expected the dedicated source operation");
            };
            let mut lease = request.lease.clone();
            lease.operation_id = request.operation_id;
            lease.job_revision = "source-selected-job".into();
            lease.input.checkpoint_revision = "source-selected".into();
            lease.input.transcript_revision = "new-transcript".into();
            lease.change_sequence += 1;
            // Shape-valid lease tries to reuse the capture root as the new source.
            let forged = Reply::PostProcessSource(Box::new(DesktopEnvelope {
                library_id: descriptor.library_id,
                library_epoch: descriptor.library_epoch,
                data: PostProcessSourceReply::Committed(PostProcessSourceReceipt {
                    operation_id: request.operation_id,
                    upload_id: request.operation.upload_id(),
                    lease,
                    outcome: LocalAsrOutcome::Complete,
                    segment_count: 1,
                }),
            }));
            write_frame(&mut stream, &reply(&frame, forged))
                .await
                .unwrap();
            assert!(read_frame::<RequestEnvelope>(&mut stream).await.is_err());
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let session = Uuid::new_v4();
        let epoch = server.descriptor.library_epoch;
        let now = SystemTime::now();
        let mut worker = RegisteredWorker {
            session_id: session,
            library_epoch: epoch,
            peer_pid: 0,
            capabilities: vec![WorkerCapability::PostProcess],
            expires_at: (now + Duration::from_secs(120)).into(),
        };
        let request = PostProcessSourceRequest {
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
                job_id: "local-source-job".into(),
                meeting_id: Uuid::new_v4(),
                job_revision: "captured-job".into(),
                input: PostProcessSourceFence {
                    capture_id: Uuid::new_v4(),
                    capture_generation: Uuid::new_v4(),
                    checkpoint_revision: "captured-checkpoint".into(),
                    lifecycle_revision: "processing-life".into(),
                    transcript_revision: "captured-transcript".into(),
                    source_id: Some("immutable-capture-root".into()),
                },
                expires_at: (now + Duration::from_secs(60)).into(),
                change_sequence: 5,
                dedup_until: (now + Duration::from_secs(7 * 24 * 60 * 60)).into(),
            },
            fence: WorkerSecret::random(),
            operation_id: Uuid::new_v4(),
            operation: PostProcessSourceOperation::Commit {
                upload_id: Uuid::new_v4(),
            },
        };
        assert!(matches!(
            client.post_process_source(&worker, request.clone()).await,
            Err(ClientError::Remote(RpcError {
                code: ErrorCode::InvalidArgument,
                ..
            }))
        ));
        worker.peer_pid = std::process::id();
        assert!(matches!(
            client.post_process_source(&worker, request).await,
            Err(ClientError::Protocol)
        ));
        assert!(matches!(client.status().await, Err(ClientError::Protocol)));
        (&mut server.task).await.unwrap();
    });
}

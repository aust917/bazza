use super::*;

fn lease(epoch: Uuid) -> CaptureLease {
    CaptureLease {
        capture_id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        worker_session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        library_epoch: epoch,
    }
}
fn proof(lease: &CaptureLease) -> WorkerProof {
    WorkerProof {
        session_id: lease.worker_session_id,
        secret: WorkerSecret::random(),
        library_epoch: lease.library_epoch,
    }
}
fn outcome(lease: &CaptureLease) -> WorkerMutationOutcome {
    WorkerMutationOutcome {
        entity_id: lease.meeting_id.to_string(),
        revision: "aborted-revision".into(),
        change_sequence: 9,
        job_id: None,
        dedup_until: "2099-01-01T00:00:00Z".parse().unwrap(),
    }
}

#[test]
fn begin_ack_is_exact_and_abort_receipt_accepts_original_epoch_without_retries() {
    run(async {
        let mut server = fake_service(|mut stream, d| async move {
            accept_hello(&mut stream, &d).await;
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::BeginCapture(ref input) = request.request else {
                panic!("expected typed begin")
            };
            write_frame(
                &mut stream,
                &reply(&request, Reply::CaptureBegun(input.lease.clone())),
            )
            .await
            .unwrap();
            let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
            let Request::AbortCapture(ref input) = request.request else {
                panic!("unexpected retry/refetch instead of typed abort")
            };
            assert_ne!(input.lease.library_epoch, d.library_epoch);
            write_frame(
                &mut stream,
                &reply(&request, Reply::CaptureAborted(outcome(&input.lease))),
            )
            .await
            .unwrap();
        });
        let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
        let active = lease(client.descriptor.library_epoch);
        assert_eq!(
            client
                .begin_capture(BeginCaptureRequest {
                    proof: proof(&active),
                    lease: active.clone(),
                    fence: WorkerSecret::random()
                })
                .await
                .unwrap(),
            active
        );
        let reserved = lease(Uuid::new_v4());
        assert_eq!(
            client
                .abort_capture(AbortCaptureRequest {
                    proof: proof(&reserved),
                    lease: reserved.clone(),
                    fence: WorkerSecret::random()
                })
                .await
                .unwrap(),
            outcome(&reserved)
        );
        (&mut server.task).await.unwrap();
    });
}

#[test]
fn forged_begin_or_abort_receipts_poison_the_stream() {
    run(async {
        for variant in 0..6 {
            let mut server = fake_service(move |mut stream, d| async move {
                accept_hello(&mut stream, &d).await;
                let request: RequestEnvelope = read_frame(&mut stream).await.unwrap();
                let response = match &request.request {
                    Request::BeginCapture(input) => {
                        let mut value = input.lease.clone();
                        if variant == 0 {
                            value.generation = Uuid::new_v4();
                            Reply::CaptureBegun(value)
                        } else {
                            Reply::CaptureLease(value)
                        }
                    }
                    Request::AbortCapture(input) => {
                        let mut value = outcome(&input.lease);
                        match variant {
                            2 => value.job_id = Some(Uuid::new_v4().to_string()),
                            3 => value.entity_id = Uuid::new_v4().to_string(),
                            4 => value.revision.clear(),
                            _ => value.change_sequence = 0,
                        }
                        Reply::CaptureAborted(value)
                    }
                    _ => panic!("unexpected worker call"),
                };
                write_frame(&mut stream, &reply(&request, response))
                    .await
                    .unwrap();
            });
            let mut client = ServiceClient::connect(server.root.path()).await.unwrap();
            let lease = lease(client.descriptor.library_epoch);
            let error = if variant < 2 {
                client
                    .begin_capture(BeginCaptureRequest {
                        proof: proof(&lease),
                        lease,
                        fence: WorkerSecret::random(),
                    })
                    .await
                    .map(|_| ())
                    .unwrap_err()
            } else {
                client
                    .abort_capture(AbortCaptureRequest {
                        proof: proof(&lease),
                        lease,
                        fence: WorkerSecret::random(),
                    })
                    .await
                    .map(|_| ())
                    .unwrap_err()
            };
            assert!(
                matches!(error, ClientError::Protocol),
                "variant {variant}: {error:?}"
            );
            assert_no_reuse(&mut client).await;
            (&mut server.task).await.unwrap();
        }
    });
}

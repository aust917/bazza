use super::*;

fn abort(proof: &WorkerProof, start: &CaptureStart, lease: &CaptureLease) -> AbortCaptureRequest {
    AbortCaptureRequest {
        proof: proof.clone(),
        lease: lease.clone(),
        fence: start.fence.clone(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_abort_requires_released_pristine_guard_and_replays_after_restart() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        error(
            client
                .begin_capture(BeginCaptureRequest {
                    proof: proof.clone(),
                    lease: lease.clone(),
                    fence: start.fence.clone(),
                })
                .await
                .unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        let guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        error(
            client
                .abort_capture(abort(&proof, &start, &lease))
                .await
                .unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        drop(guard);
        let request = abort(&proof, &start, &lease);
        let receipt = client.abort_capture(request.clone()).await.unwrap();
        assert!(receipt.job_id.is_none());
        assert_eq!(
            client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        let directory = fixture
            .root
            .join("recordings")
            .join(lease.meeting_id.to_string());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        let guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        error(
            client
                .begin_capture(BeginCaptureRequest {
                    proof: proof.clone(),
                    lease: lease.clone(),
                    fence: start.fence.clone(),
                })
                .await
                .unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        drop(guard);
        service.crash();
        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut fresh = restarted.ready(&fixture.root).await;
        assert_eq!(fresh.abort_capture(request).await.unwrap(), receipt);
        let (_, newproof) = worker(&mut fresh).await;
        let (_, newlease) = prepare(&mut fresh, &newproof).await;
        assert_ne!(newlease.capture_id, lease.capture_id);
        assert!(!directory.join("mix.wav").exists());
        drop(client);
        restarted.shutdown(fresh).await;
    })
    .await
    .expect("bounded retained abort receipt restart test");
}

#[tokio::test(flavor = "current_thread")]
async fn startup_aborts_only_old_epoch_unused_reservations_without_heartbeat_inference() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let (_, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        sleep(Duration::from_millis(650)).await;
        assert_eq!(
            client
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "recording"
        );
        service.crash();
        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut fresh = restarted.ready(&fixture.root).await;
        assert_eq!(
            fresh
                .meeting(lease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        let receipt = fresh
            .abort_capture(abort(&proof, &start, &lease))
            .await
            .unwrap();
        assert!(receipt.job_id.is_none());
        assert_eq!(
            fresh
                .abort_capture(abort(&proof, &start, &lease))
                .await
                .unwrap(),
            receipt
        );
        drop(client);
        restarted.shutdown(fresh).await;
    })
    .await
    .expect("bounded old-epoch reservation compensation test");
}

#[tokio::test(flavor = "current_thread")]
async fn committed_begin_without_raw_bytes_is_never_treated_as_an_unused_reservation() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_, proof) = worker(&mut client).await;
        let (start, lease) = prepare(&mut client, &proof).await;
        let guard = artifact_guard(&fixture.root, &identity(status.library_id, &lease));
        begin(&mut client, &proof, &start, &lease).await;
        drop(guard);
        service.crash();
        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut fresh = restarted.ready(&fixture.root).await;
        let interrupted = fresh.meeting(lease.meeting_id).await.unwrap().unwrap();
        assert_eq!(interrupted.status, "failed");
        assert_eq!(interrupted.duration_ms, 0);
        assert!(interrupted.ended_at.is_some());
        error(
            fresh
                .abort_capture(abort(&proof, &start, &lease))
                .await
                .unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        let finalize = FinalizeCaptureRequest {
            proof,
            lease: lease.clone(),
            fence: start.fence.clone(),
        };
        let receipt = fresh.finalize_capture(finalize.clone()).await.unwrap();
        assert!(receipt.job_id.is_none());
        assert_eq!(fresh.finalize_capture(finalize).await.unwrap(), receipt);
        let directory = fixture
            .root
            .join("recordings")
            .join(lease.meeting_id.to_string());
        assert!(!directory.join("mix.wav").exists());
        assert!(!directory
            .join(bazza_capture_handoff::TERMINAL_FILE)
            .exists());
        let (_, newproof) = worker(&mut fresh).await;
        let (_, next) = prepare(&mut fresh, &newproof).await;
        assert_ne!(next.capture_id, lease.capture_id);
        drop(client);
        restarted.shutdown(fresh).await;
    })
    .await
    .expect("bounded lost begin ACK safety test");
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_live_guard_can_reattach_after_restart_then_begin_and_finalize() {
    timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut service = OwnedService::spawn(&fixture.root);
        let mut client = service.ready(&fixture.root).await;
        let status = client.status().await.unwrap();
        let (_, proof) = worker(&mut client).await;
        let (start, oldlease) = prepare(&mut client, &proof).await;
        let guard = artifact_guard(&fixture.root, &identity(status.library_id, &oldlease));
        service.crash();
        let mut restarted = OwnedService::spawn(&fixture.root);
        let mut fresh = restarted.ready(&fixture.root).await;
        assert_eq!(
            fresh
                .meeting(oldlease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "recording"
        );
        drop(guard);
        sleep(Duration::from_millis(650)).await;
        assert_eq!(
            fresh
                .meeting(oldlease.meeting_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "recording",
            "periodic recovery must not revisit a reservation that was busy at startup"
        );
        let mut guard = artifact_guard(&fixture.root, &identity(status.library_id, &oldlease));
        let lease = fresh
            .reattach_capture(ReattachCaptureRequest {
                proof: proof.clone(),
                capture_id: oldlease.capture_id,
                fence: start.fence.clone(),
                ttl_seconds: 60,
            })
            .await
            .unwrap();
        let proof = WorkerProof {
            library_epoch: lease.library_epoch,
            ..proof
        };
        begin(&mut fresh, &proof, &start, &lease).await;
        append(&mut guard, 1, "first bytes only after begin ACK");
        seal(&mut guard);
        drop(guard);
        let receipt = fresh
            .finalize_capture(FinalizeCaptureRequest {
                proof,
                lease: lease.clone(),
                fence: start.fence,
            })
            .await
            .unwrap();
        drop(client);
        restarted.shutdown(fresh).await;
        one_job(
            &fixture.root,
            lease.meeting_id,
            receipt.job_id.as_deref(),
            &["first bytes only after begin ACK"],
        );
    })
    .await
    .expect("bounded pre-begin reattach test");
}

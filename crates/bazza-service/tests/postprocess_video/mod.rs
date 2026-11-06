use super::*;
use bazza_service_api::postprocess_video::{
    PostProcessVideoAction, PostProcessVideoOutcome, PostProcessVideoRequest,
};

fn staged_mp4() -> Vec<u8> {
    if let Some(path) = std::env::var_os("BAZZA_TEST_POSTPROCESS_MP4") {
        return fs::read(path).expect("read explicitly selected synthetic FFmpeg output");
    }
    // The default fixture exercises MP4 structure and wire behavior without
    // requiring FFmpeg. The worker itself verifies streams before staging.
    let mut bytes = Vec::new();
    for (kind, payload) in [
        (b"ftyp", b"isom\0\0\0\0".as_slice()),
        (b"moov", b"fixture metadata".as_slice()),
        (b"mdat", b"fixture stream".as_slice()),
    ] {
        bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
    }
    bytes
}

#[tokio::test]
async fn private_video_publication_roundtrip() {
    let mut harness = Harness::new();
    harness.start().await;
    let mut client = ServiceClient::connect(&harness.root).await.unwrap();
    let (_, capture_proof) = worker(&mut client, WorkerCapability::Capture).await;
    let raw = b"original screen capture";
    let captured = capture_terminal_with_screen(
        &mut client,
        &harness.root,
        &capture_proof,
        true,
        CaptureProvider::Local,
        Some(raw),
    )
    .await;
    let (registered, proof) = worker(&mut client, WorkerCapability::PostProcess).await;
    let job = client
        .post_process_jobs(&registered, PostProcessJobsRequest::new(proof.clone()))
        .await
        .unwrap()
        .data
        .items
        .remove(0);
    assert_eq!(job.meeting_id, captured.meeting_id);
    let claim = claim(&proof, job.clone());
    let lease = client
        .claim_post_process_job(&registered, claim.clone())
        .await
        .unwrap()
        .data;
    let media = client
        .acquire_post_process_media(
            &registered,
            &harness.root,
            WorkerMediaInputRequest {
                proof: proof.clone(),
                lease: lease.clone(),
                fence: claim.fence.clone(),
                expected_provider: job.provider.clone(),
            },
        )
        .await
        .unwrap();
    let staged = media.video_staging_path().unwrap();
    let output = staged_mp4();
    fs::write(&staged, &output).unwrap();
    assert!(!header(&mut client, captured.meeting_id).await.has_video);
    let receipt = client
        .publish_post_process_video(&registered, &media, &proof, &lease, &claim.fence)
        .await
        .unwrap()
        .data;
    assert_eq!(
        receipt.outcome,
        PostProcessVideoOutcome::Published {
            bytes: output.len() as u64
        }
    );
    assert!(header(&mut client, captured.meeting_id).await.has_video);
    let directory = staged.parent().unwrap();
    assert_eq!(fs::read(directory.join("screen.mp4")).unwrap(), output);
    assert_eq!(fs::read(directory.join("screen.raw.mp4")).unwrap(), raw);
    assert!(!staged.exists());
    let replay = client
        .publish_post_process_video(&registered, &media, &proof, &lease, &claim.fence)
        .await
        .unwrap()
        .data;
    assert_eq!(replay, receipt);
    let nonce = media.grant().owner_nonce;
    drop(media);
    raw_expect_error(
        &harness.root,
        Request::PostProcessVideo(Box::new(PostProcessVideoRequest {
            media: PostProcessMediaRequest {
                proof,
                lease,
                fence: claim.fence,
                owner_nonce: nonce,
                expected_provider: job.provider,
            },
            result: PostProcessVideoAction::Publish {},
        })),
        ErrorCode::PermissionDenied,
    )
    .await;
    harness.stop().await;
}

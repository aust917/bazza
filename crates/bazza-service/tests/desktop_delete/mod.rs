//! Private typed delete through the real fixture service.
use super::{remote, Harness};
use bazza_service_api::{desktop::DeleteDesktopMeetingRequest, rpc::ErrorCode};
use bazza_service_client::RepositoryClient;
use bazza_service_test_support::{ORPHAN_MEETING_ID, READY_MEETING_ID};
use std::{path::Path, time::Duration};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

fn job_state(root: &Path, job_id: &str) -> (String, Option<String>) {
    // Observe only the disposable fixture's durable queue in this test.
    let connection = rusqlite::Connection::open_with_flags(
        root.join("bazza.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    connection
        .query_row(
            "SELECT state,error_code FROM service_jobs WHERE id=?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn desktop_delete_fences_recording_and_replays_across_purge_and_restart() {
    timeout(Duration::from_secs(20), async {
        let mut h = Harness::new();
        h.start().await;
        let repository = RepositoryClient::connect(&h.root).await.unwrap();
        let recording = repository
            .meeting(ORPHAN_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(recording.status, "recording");
        let denied = DeleteDesktopMeetingRequest {
            meeting_id: recording.id,
            expected_lifecycle_revision: recording.lifecycle_revision,
            idempotency_key: Uuid::new_v4().to_string(),
        };
        remote(
            repository.delete_meeting(denied).await.unwrap_err(),
            ErrorCode::ResourceBusy,
        );
        assert!(repository
            .meeting(ORPHAN_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .is_some());

        let ready = repository
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        let request = DeleteDesktopMeetingRequest {
            meeting_id: ready.id,
            expected_lifecycle_revision: ready.lifecycle_revision,
            idempotency_key: Uuid::new_v4().to_string(),
        };
        let stale = DeleteDesktopMeetingRequest {
            expected_lifecycle_revision: Uuid::new_v4().to_string(),
            ..request.clone()
        };
        remote(
            repository.delete_meeting(stale.clone()).await.unwrap_err(),
            ErrorCode::Conflict,
        );

        let media = h.root.join("recordings").join(READY_MEETING_ID);
        let mix = media.join("mix.wav");
        assert!(mix.is_file());
        #[cfg(windows)]
        let pinned = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&mix)
                .unwrap()
        };

        let receipt = repository.delete_meeting(request.clone()).await.unwrap();
        assert!(request.accepts(&receipt.data));
        assert!(!serde_json::to_string(&receipt)
            .unwrap()
            .contains("recordings"));
        let job = receipt.data.job_id.as_deref().unwrap();
        assert!(repository
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .is_none());
        assert_eq!(
            repository
                .delete_meeting(request.clone())
                .await
                .unwrap()
                .data,
            receipt.data
        );
        remote(
            repository.delete_meeting(stale).await.unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );

        #[cfg(windows)]
        {
            timeout(Duration::from_secs(5), async {
                while job_state(&h.root, job).1.as_deref() != Some("PURGE_RETRY") {
                    sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .expect("service observed locked fixture file");
            assert!(mix.is_file());
        }
        drop(repository);
        h.crash();
        #[cfg(windows)]
        drop(pinned);
        h.start().await;
        let repository = RepositoryClient::connect(&h.root).await.unwrap();
        let replay = repository.delete_meeting(request).await.unwrap();
        assert_ne!(
            replay.identity.library_epoch,
            receipt.identity.library_epoch
        );
        assert_eq!(replay.data, receipt.data);
        timeout(Duration::from_secs(6), async {
            while job_state(&h.root, job).0 != "succeeded" {
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("service resumed its pending purge");
        assert!(!media.exists());
        drop(repository);
        h.stop().await;
    })
    .await
    .expect("bounded desktop delete IPC test");
}

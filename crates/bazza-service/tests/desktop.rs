#![cfg(any(windows, target_os = "macos"))]
//! Real owned service processes. The repository facade never opens SQLite.
#[path = "desktop_delete/mod.rs"]
mod delete_tests;
use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::{
    content::*,
    desktop::*,
    rpc::{ErrorCode, STORAGE_SCHEMA_MAX},
    worker::*,
};
use bazza_service_client::repository::{RepositorySummaryRequest, RepositoryTranscriptRequest};
use bazza_service_client::{ClientError, RepositoryClient, ServiceClient};
use bazza_service_test_support::{create_v8, DAMAGED_MEETING_ID, READY_MEETING_ID};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{sleep, timeout, Instant};

struct Harness {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    child: Option<Child>,
}
impl Harness {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("desktop fixture Орион");
        create_v8(&root).unwrap();
        drop(Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap());
        Self {
            _temporary: temporary,
            root,
            child: None,
        }
    }
    async fn start(&mut self) {
        assert!(self.child.is_none());
        let mut command = Command::new(env!("CARGO_BIN_EXE_bazza-service"));
        command
            .args(["serve-fixture", "--library"])
            .arg(&self.root)
            .args(["--idle-timeout-ms", "10000"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        self.child = Some(command.spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if ServiceClient::connect(&self.root).await.is_ok() {
                break;
            }
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "owned service exited before readiness"
            );
            assert!(Instant::now() < until, "owned service readiness timeout");
            sleep(Duration::from_millis(15)).await;
        }
    }
    fn crash(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    async fn stop(&mut self) {
        let mut control = ServiceClient::connect(&self.root).await.unwrap();
        control.shutdown().await.unwrap();
        drop(control);
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.child.take();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn remote(error: ClientError, code: ErrorCode) {
    assert!(
        matches!(error,ClientError::Remote(ref e) if e.code==code),
        "expected {code:?}: {error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_desktop_pages_and_narrow_preferences_share_one_service() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        h.start().await;
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let other = repo.clone();
        let boot = repo.bootstrap().await.unwrap().data;
        assert_eq!(boot.preferences.ui_locale, DesktopLocale::Ru);
        let mut cursor = None;
        let mut ids = std::collections::HashSet::new();
        loop {
            let page = repo
                .meetings(DesktopMeetingFilter::default(), cursor, 1, 4096)
                .await
                .unwrap();
            assert!(page.items.len() <= 1);
            for meeting in page.items {
                assert!(ids.insert(meeting.id));
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids.len(), 4);
        let m = repo
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        assert!(m.media.audio);
        assert_eq!(m.title, "Орион / Budget review");
        let missing = repo
            .meeting(DAMAGED_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        assert!(!missing.media.audio);
        assert_eq!(missing.content_kind, "recorded");
        assert!(!h.root.join("recordings").join(DAMAGED_MEETING_ID).exists());
        assert!(!h.root.join("models").exists());
        assert!(repo
            .meeting("opaque_legacy-id".into())
            .await
            .unwrap()
            .data
            .is_none());
        let update = UpdateDesktopPreferencesRequest {
            expected_revision: boot.preferences.revision,
            idempotency_key: "locale".into(),
            patch: DesktopPreferencesPatch {
                ui_locale: Some(DesktopLocale::En),
                ..Default::default()
            },
        };
        let ack = repo.update_preferences(update.clone()).await.unwrap().data;
        assert_eq!(ack, other.update_preferences(update).await.unwrap().data);
        assert_eq!(
            other.preferences().await.unwrap().data.ui_locale,
            DesktopLocale::En
        );
        let feed = repo.changes(None, 1, 4096).await.unwrap();
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.items[0].entity_kind, "desktop_preferences");
        drop(other);
        drop(repo);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn competing_gui_windows_conflict_and_invalidate_a_list_cursor() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        h.start().await;
        let a = RepositoryClient::connect(&h.root).await.unwrap();
        let b = RepositoryClient::connect(&h.root).await.unwrap();
        let old = a
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        let stale_list = b
            .meetings(Default::default(), None, 1, 4096)
            .await
            .unwrap()
            .next
            .unwrap();
        let request = RenameDesktopMeetingRequest {
            meeting_id: READY_MEETING_ID.into(),
            expected_revision: old.metadata_revision.clone(),
            idempotency_key: "window-a".into(),
            title: "Review committed".into(),
        };
        let ack = a.rename_meeting(request.clone()).await.unwrap().data;
        assert_eq!(ack, b.rename_meeting(request).await.unwrap().data);
        remote(
            b.rename_meeting(RenameDesktopMeetingRequest {
                meeting_id: READY_MEETING_ID.into(),
                expected_revision: old.metadata_revision,
                idempotency_key: "window-b".into(),
                title: "Stale edit".into(),
            })
            .await
            .unwrap_err(),
            ErrorCode::Conflict,
        );
        remote(
            b.meetings(Default::default(), Some(stale_list), 1, 4096)
                .await
                .unwrap_err(),
            ErrorCode::ResyncRequired,
        );
        let current = b
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(current.title, "Review committed");
        assert_eq!(current.participants, old.participants);
        assert_eq!(current.transcript_revision, old.transcript_revision);
        drop(a);
        drop(b);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_reconnect_pins_library_and_rejects_old_epoch_continuations() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        h.start().await;
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let before = repo.identity().await;
        let old_page = repo
            .meetings(Default::default(), None, 1, 4096)
            .await
            .unwrap()
            .next
            .unwrap();
        let p = repo.preferences().await.unwrap().data;
        let request = UpdateDesktopPreferencesRequest {
            expected_revision: p.revision,
            idempotency_key: "reconnect-retry".into(),
            patch: DesktopPreferencesPatch {
                close_to_tray: Some(false),
                ..Default::default()
            },
        };
        let receipt = repo.update_preferences(request.clone()).await.unwrap().data;
        h.crash();
        assert!(repo.preferences().await.is_err());
        assert!(repo.reconnect().await.is_err());
        assert!(
            repo.preferences().await.is_err(),
            "no stale connection fallback"
        );
        h.start().await;
        let after = repo.reconnect().await.unwrap();
        assert_eq!(after.library_id, before.library_id);
        assert_ne!(after.library_epoch, before.library_epoch);
        assert_eq!(
            receipt,
            repo.update_preferences(request).await.unwrap().data
        );
        remote(
            repo.meetings(Default::default(), Some(old_page), 1, 4096)
                .await
                .unwrap_err(),
            ErrorCode::ResyncRequired,
        );
        drop(repo);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn restart_with_unpolled_old_clients_uses_a_fresh_session_without_replaying_mutations() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        h.start().await;
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let mut old_client = ServiceClient::connect(&h.root).await.unwrap();
        let original = repo
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        let before = bazza_service_client::read_endpoint(&h.root).unwrap();
        h.crash();
        // Neither old connection is polled, shut down or dropped before the
        // replacement binds. Windows must not reuse their named pipe instance.
        h.start().await;
        let after = bazza_service_client::read_endpoint(&h.root).unwrap();
        assert_eq!(after.library_id, before.library_id);
        assert_ne!(after.library_epoch, before.library_epoch);
        #[cfg(windows)]
        assert_ne!(after.endpoint, before.endpoint);
        let mut fresh = ServiceClient::connect(&h.root).await.unwrap();
        assert_eq!(
            fresh.status().await.unwrap().library_epoch,
            after.library_epoch
        );
        let stale = RenameDesktopMeetingRequest {
            meeting_id: original.id.clone(),
            expected_revision: original.metadata_revision.clone(),
            idempotency_key: "stale-pipe-must-not-replay".into(),
            title: "Uncommitted stale rename".into(),
        };
        assert!(old_client
            .rename_desktop_meeting(stale.clone())
            .await
            .is_err());
        assert!(repo.rename_meeting(stale).await.is_err());
        assert_eq!(
            repo.reconnect().await.unwrap().library_epoch,
            after.library_epoch
        );
        let current = repo.meeting(original.id).await.unwrap().data.unwrap();
        assert_eq!(current.title, original.title);
        assert_eq!(current.metadata_revision, original.metadata_revision);
        drop(old_client);
        drop(fresh);
        drop(repo);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn reconnect_cannot_silently_select_a_different_library_identity() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        h.start().await;
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let original = repo.identity().await;
        h.crash();
        std::fs::write(
            h.root.join(bazza_core::IDENTITY_FILE),
            serde_json::to_vec(&serde_json::json!({"version":1,"library_id":uuid::Uuid::new_v4()}))
                .unwrap(),
        )
        .unwrap();
        h.start().await;
        remote(
            repo.reconnect().await.unwrap_err(),
            ErrorCode::ResyncRequired,
        );
        assert!(repo.preferences().await.is_err());
        let explicitly_new = RepositoryClient::connect(&h.root).await.unwrap();
        assert_ne!(
            explicitly_new.identity().await.library_id,
            original.library_id
        );
        drop(explicitly_new);
        drop(repo);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn capture_metadata_rename_preserves_recording_state_and_generation() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        h.start().await;
        let mut worker = ServiceClient::connect(&h.root).await.unwrap();
        let status = worker.status().await.unwrap();
        assert_eq!(status.schema_version, STORAGE_SCHEMA_MAX);
        let registration = WorkerRegistration::new(vec![WorkerCapability::Capture]);
        worker
            .register_worker(RegisterWorkerRequest {
                registration: registration.clone(),
                ttl_seconds: 60,
            })
            .await
            .unwrap();
        let proof = WorkerProof {
            session_id: registration.session_id,
            secret: registration.secret,
            library_epoch: status.library_epoch,
        };
        let start = CaptureStart {
            capture_id: uuid::Uuid::new_v4(),
            meeting_id: uuid::Uuid::new_v4(),
            fence: WorkerSecret::random(),
            title: "During capture".into(),
            app: None,
            started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
            participants: vec!["Sam".into()],
            provider: CaptureSelection {
                provider: CaptureProvider::Local,
                model: None,
                language: None,
            },
        };
        let lease = worker
            .prepare_capture(PrepareCaptureRequest { proof, start })
            .await
            .unwrap();
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let m = repo
            .meeting(lease.meeting_id.to_string())
            .await
            .unwrap()
            .data
            .unwrap();
        repo.rename_meeting(RenameDesktopMeetingRequest {
            meeting_id: m.id.clone(),
            expected_revision: m.metadata_revision,
            idempotency_key: "live-rename".into(),
            title: "Renamed during capture".into(),
        })
        .await
        .unwrap();
        let m = repo.meeting(m.id).await.unwrap().data.unwrap();
        assert_eq!(m.status, "recording");
        assert_eq!(m.lifecycle_revision, lease.generation.to_string());
        assert_eq!(m.participants, ["Sam"]);
        assert!(!m.title_auto);
        drop(repo);
        drop(worker);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn missing_facade_path_never_creates_a_library_or_default_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("does-not-exist");
    assert!(RepositoryClient::connect(&missing).await.is_err());
    assert!(!missing.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn text_meeting_header_and_filter_require_no_media_or_model_directory() {
    timeout(Duration::from_secs(12), async {
        let mut h = Harness::new();
        let domain = Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        let created = domain
            .data()
            .create_meeting(
                &bazza_core::data::MutationContext {
                    principal: "fixture".into(),
                    idempotency_key: "text-card".into(),
                },
                &bazza_core::data::NewTextMeeting {
                    title: "Text card".into(),
                    started_at: "2026-11-02T00:00:00Z".parse().unwrap(),
                    participants: vec![],
                    folder_id: None,
                    tag_ids: vec![],
                    calendar_uid: None,
                    transcript: None,
                },
            )
            .unwrap();
        drop(domain);
        h.start().await;
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let snapshot = repo.meeting(created.entity_id.clone()).await.unwrap();
        assert_eq!(snapshot.identity, repo.identity().await);
        let header = snapshot.data.unwrap();
        assert_eq!(header.content_kind, "text");
        assert_eq!(header.status, "ready");
        assert!(!header.media.audio);
        assert!(!header.media.video);
        assert_eq!(header.transcript_state, "absent");
        let page = repo
            .meetings(
                DesktopMeetingFilter {
                    content_kind: Some("text".into()),
                    ..Default::default()
                },
                None,
                20,
                4096,
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, created.entity_id);
        assert_eq!(page.identity, repo.identity().await);
        assert!(!h.root.join("recordings").join(created.entity_id).exists());
        assert!(!h.root.join("models").exists());
        drop(repo);
        h.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn content_facade_pages_raw_effective_and_structured_summary_without_losing_bytes() {
    timeout(Duration::from_secs(20), async {
        use bazza_core::data::{
            DocumentState, MutationContext, SummaryAuthor, SummaryWrite, SummaryWriteMode,
            TranscriptCorrection, TranscriptCorrections, TranscriptImportOrigin, TranscriptRead,
            TranscriptWrite,
        };
        let mut h = Harness::new();
        let domain = Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        let data = domain.data();
        let original_source = data
            .transcript_state(READY_MEETING_ID)
            .unwrap()
            .current_source_id
            .unwrap();
        let raw = "Я\"😀".repeat(8_000);
        let context = |key: &str| MutationContext {
            principal: "fixture".into(),
            idempotency_key: key.into(),
        };
        let state = data.transcript_state(READY_MEETING_ID).unwrap();
        data.replace_transcript(
            &context("content-source"),
            READY_MEETING_ID,
            &TranscriptWrite {
                expected_revision: state.revision,
                segments: serde_json::from_value(serde_json::json!([
                    {"start_ms":0,"end_ms":1000,"speaker":"Sam","text":raw},
                    {"start_ms":1000,"end_ms":2000,"speaker":"Sam","text":"Manual raw"}
                ]))
                .unwrap(),
                source_label: Some("Synthetic content".into()),
                language: Some("ru".into()),
                origin: TranscriptImportOrigin::Imported,
            },
        )
        .unwrap();
        let current = data
            .transcript(READY_MEETING_ID, &TranscriptRead::default())
            .unwrap();
        data.correct_transcript(
            &context("manual-empty"),
            READY_MEETING_ID,
            &TranscriptCorrections {
                expected_revision: current.state.revision,
                corrections: vec![TranscriptCorrection {
                    segment_id: current.segments[1].id,
                    text: Some(String::new()),
                }],
            },
        )
        .unwrap();
        let markdown = "Итог \"😺\n".repeat(3_000);
        let structured = serde_json::json!({"long": "Цель 😀\"".repeat(3_000)});
        let summary_state = data.summary(READY_MEETING_ID).unwrap().state;
        data.write_summary(
            &context("content-summary"),
            READY_MEETING_ID,
            &SummaryWrite {
                expected_revision: summary_state.revision,
                mode: if summary_state.document_state == DocumentState::Present {
                    SummaryWriteMode::Replace
                } else {
                    SummaryWriteMode::Create
                },
                markdown: markdown.clone(),
                structured_json: structured.clone(),
                source_label: None,
                author: SummaryAuthor::Manual,
            },
        )
        .unwrap();
        drop(domain);
        h.start().await;
        let repo = RepositoryClient::connect(&h.root).await.unwrap();
        let header = repo
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        let identity = repo.identity().await;
        for representation in [
            DesktopTranscriptRepresentation::Raw,
            DesktopTranscriptRepresentation::Effective,
        ] {
            let mut request = RepositoryTranscriptRequest::new(
                READY_MEETING_ID.into(),
                header.transcript_revision.clone(),
                representation,
            );
            request.limit = 2;
            request.max_bytes = 2048;
            let mut values = std::collections::BTreeMap::<u64, String>::new();
            let mut calls = 0;
            loop {
                calls += 1;
                assert!(
                    calls < 300,
                    "content must make bounded continuation progress"
                );
                let page = repo.transcript(request.clone()).await.unwrap();
                assert_eq!(page.identity, identity);
                assert_eq!(page.state.revision, header.transcript_revision);
                assert!(page.items.len() <= 2);
                for fragment in page.items {
                    let text = values.entry(fragment.segment_index).or_default();
                    assert_eq!(fragment.byte_offset, text.len() as u64);
                    text.push_str(&fragment.text);
                    if fragment.segment_index == 1 {
                        assert!(fragment.clean_text_manual);
                        assert_eq!(fragment.effective_origin, DesktopEffectiveOrigin::Manual);
                    }
                }
                request.cursor = page.next;
                if request.cursor.is_none() {
                    break;
                }
            }
            assert!(calls > 1);
            assert_eq!(values[&0], raw);
            assert_eq!(
                values[&1],
                if representation == DesktopTranscriptRepresentation::Raw {
                    "Manual raw"
                } else {
                    ""
                }
            );
        }
        let mut archive = RepositoryTranscriptRequest::new(
            READY_MEETING_ID.into(),
            header.transcript_revision.clone(),
            DesktopTranscriptRepresentation::Raw,
        );
        archive.source_id = Some(original_source.clone());
        let old = repo.transcript(archive).await.unwrap();
        assert_eq!(old.source_id, Some(original_source));
        assert!(!old.items.is_empty());
        assert_ne!(old.items[0].text, raw);
        for (field, expected) in [
            (DesktopSummaryField::Markdown, markdown),
            (
                DesktopSummaryField::StructuredJson,
                serde_json::to_string(&structured).unwrap(),
            ),
        ] {
            let mut request = RepositorySummaryRequest::new(
                READY_MEETING_ID.into(),
                header.summary_revision.clone(),
                field,
            );
            request.max_bytes = 2048;
            let mut text = String::new();
            let mut calls = 0;
            loop {
                calls += 1;
                assert!(calls < 300);
                let page = repo.summary(request.clone()).await.unwrap();
                assert_eq!(page.identity, identity);
                assert_eq!(page.byte_offset, text.len() as u64);
                assert_eq!(page.total_bytes, expected.len() as u64);
                text.push_str(&page.text);
                request.cursor = page.next;
                if request.cursor.is_none() {
                    break;
                }
            }
            assert!(calls > 1);
            assert_eq!(text, expected);
        }
        drop(repo);
        h.stop().await;
    })
    .await
    .expect("bounded owned content process test");
}

#[tokio::test(flavor = "current_thread")]
async fn organization_facades_share_revisions_and_preserve_meeting_content() {
    use bazza_service_api::library::{
        DesktopLibraryItem, DesktopLibraryOperation, DesktopLibraryRequest as L,
    };
    timeout(Duration::from_secs(15), async {
        let mut h = Harness::new();
        h.start().await;
        let a = RepositoryClient::connect(&h.root).await.unwrap();
        let b = RepositoryClient::connect(&h.root).await.unwrap();
        let original = a.meeting(READY_MEETING_ID.into()).await.unwrap().data.unwrap();
        let first_page = a.library_page(None, 1, 2048).await.unwrap();
        let stale_cursor = first_page.next.unwrap();
        let create = L::FolderCreate {
            name: " SDK project ".into(), parent_id: None, idempotency_key: "library-folder".into(),
        };
        let folder = a.library_mutate(create.clone()).await.unwrap();
        assert_eq!(folder.identity, a.identity().await);
        assert_eq!(folder.data.operation, DesktopLibraryOperation::FolderCreate);
        assert_eq!(folder.data.outcome, b.library_mutate(create).await.unwrap().data.outcome);
        let folder_id = folder.data.outcome.entity_id.clone();
        let renamed = a.library_mutate(L::FolderRename {
            folder_id: folder_id.clone(), name: "Project renamed".into(),
            expected_revision: folder.data.outcome.revision.clone(), idempotency_key: "library-rename".into(),
        }).await.unwrap().data.outcome;
        remote(b.library_mutate(L::FolderRename {
            folder_id: folder_id.clone(), name: "Stale overwrite".into(),
            expected_revision: folder.data.outcome.revision, idempotency_key: "library-stale".into(),
        }).await.unwrap_err(), ErrorCode::Conflict);
        remote(b.library_page(Some(stale_cursor), 1, 2048).await.unwrap_err(), ErrorCode::ResyncRequired);
        let folder_ready = a.library_mutate(L::FolderSetTemplate {
            folder_id: folder_id.clone(), template_id: Some("fixture-template".into()),
            expected_revision: renamed.revision, idempotency_key: "library-template".into(),
        }).await.unwrap().data.outcome;
        let moved = a.library_mutate(L::MeetingMove {
            meeting_id: original.id.clone(), folder_id: Some(folder_id.clone()),
            expected_revision: original.metadata_revision.clone(), idempotency_key: "library-meeting-move".into(),
        }).await.unwrap().data.outcome;
        let tagged = a.library_mutate(L::MeetingTagNamed {
            meeting_id: original.id.clone(), name: " ClientReview ".into(),
            expected_revision: moved.revision, idempotency_key: "library-named-tag".into(),
        }).await.unwrap().data.outcome;
        let page = b.library_page(None, 100, 65024).await.unwrap();
        assert_eq!(page.identity, b.identity().await);
        assert!(page.items.iter().any(|item| matches!(item,DesktopLibraryItem::Folder{id,meeting_count:1,default_template_id:Some(template),..} if id==&folder_id && template=="fixture-template")));
        let (tag_id, tag_revision) = page.items.iter().find_map(|item| match item {
            DesktopLibraryItem::Tag { id, name, meeting_count:1, revision } if name=="ClientReview" => Some((id.clone(),revision.clone())),
            _ => None,
        }).expect("named-tag association and count are visible");
        b.library_mutate(L::TagDelete {tag_id:tag_id.clone(),expected_revision:tag_revision,idempotency_key:"library-tag-delete".into()}).await.unwrap();
        let after_tag = a.meeting(original.id.clone()).await.unwrap().data.unwrap();
        assert_ne!(after_tag.metadata_revision, tagged.revision);
        b.library_mutate(L::FolderDelete {folder_id:folder_id.clone(),expected_revision:folder_ready.revision,idempotency_key:"library-folder-delete".into()}).await.unwrap();
        let final_header = a.meeting(original.id.clone()).await.unwrap().data.unwrap();
        assert!(final_header.folder_id.is_none());
        assert_eq!(final_header.title, original.title);
        assert_eq!(final_header.status, original.status);
        assert_eq!(final_header.participants, original.participants);
        assert_eq!(final_header.lifecycle_revision, original.lifecycle_revision);
        assert_eq!(final_header.transcript_revision, original.transcript_revision);
        assert_eq!(final_header.summary_revision, original.summary_revision);
        let end = a.library_page(None, 100, 65024).await.unwrap();
        assert!(!end.items.iter().any(|item| match item {DesktopLibraryItem::Folder{id,..}=>id==&folder_id,DesktopLibraryItem::Tag{id,..}=>id==&tag_id}));
        drop(a); drop(b); h.stop().await;
        let domain = Repository::open_fixture(&h.root, MigrationPolicy::RequireCurrent).unwrap();
        assert_eq!(domain.data().transcript_state(READY_MEETING_ID).unwrap().revision, original.transcript_revision);
    }).await.expect("bounded owned organization service test");
}

#[tokio::test(flavor = "current_thread")]
async fn private_configuration_preserves_shared_revision_conflicts_and_exact_retry() {
    use bazza_service_api::configuration::UpdateNativeConfigurationRequest;
    timeout(Duration::from_secs(15), async {
        let mut h = Harness::new();
        let audio = h
            .root
            .join("recordings")
            .join(READY_MEETING_ID)
            .join("mix.wav");
        let audio_before = std::fs::read(&audio).unwrap();
        h.start().await;
        let a = RepositoryClient::connect(&h.root).await.unwrap();
        let b = RepositoryClient::connect(&h.root).await.unwrap();
        let before = a.native_configuration().await.unwrap();
        let preferences = b.preferences().await.unwrap();
        assert_eq!(before.identity, a.identity().await);
        assert_eq!(before.data.settings.video_fps, 15);
        assert_eq!(before.data.settings.video_max_height, 1080);
        let mut settings = before.data.settings.clone();
        settings.video_fps = 30;
        settings.llm_api_key = Some("synthetic-private-config-token".into());
        let request = UpdateNativeConfigurationRequest {
            expected_revision: before.data.revision.clone(),
            idempotency_key: "private-configuration".into(),
            settings,
        };
        let receipt = a
            .update_native_configuration(request.clone())
            .await
            .unwrap();
        assert_eq!(receipt.identity, before.identity);
        assert_eq!(receipt.data.entity_id, "native_configuration");
        let serialized = serde_json::to_string(&receipt).unwrap();
        assert!(!serialized.contains("synthetic-private-config-token"));
        remote(
            b.update_native_configuration(UpdateNativeConfigurationRequest {
                idempotency_key: "private-stale".into(),
                ..request.clone()
            })
            .await
            .unwrap_err(),
            ErrorCode::Conflict,
        );
        remote(
            b.update_preferences(UpdateDesktopPreferencesRequest {
                expected_revision: preferences.data.revision,
                idempotency_key: "private-stale-preferences".into(),
                patch: DesktopPreferencesPatch {
                    close_to_tray: Some(false),
                    ..Default::default()
                },
            })
            .await
            .unwrap_err(),
            ErrorCode::Conflict,
        );
        let current = b.native_configuration().await.unwrap();
        assert_eq!(current.data.settings, request.settings);
        assert_eq!(current.data.revision, receipt.data.revision);
        let fresh_preferences = b.preferences().await.unwrap();
        b.update_preferences(UpdateDesktopPreferencesRequest {
            expected_revision: fresh_preferences.data.revision,
            idempotency_key: "private-fresh-preferences".into(),
            patch: DesktopPreferencesPatch {
                close_to_tray: Some(false),
                ..Default::default()
            },
        })
        .await
        .unwrap();
        let after = a.native_configuration().await.unwrap();
        assert_ne!(after.data.revision, current.data.revision);
        assert!(!after.data.settings.close_to_tray);
        assert_eq!(
            after.data.settings.llm_api_key,
            request.settings.llm_api_key
        );
        assert_eq!(
            b.update_native_configuration(request.clone())
                .await
                .unwrap()
                .data,
            receipt.data
        );
        let mut changed_request = request.clone();
        changed_request.settings.user_name = "different payload".into();
        remote(
            a.update_native_configuration(changed_request)
                .await
                .unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );
        let mut cloud_change = after.data.settings.clone();
        cloud_change.soniox_consent = !cloud_change.soniox_consent;
        let cloud_receipt = a
            .update_native_configuration(UpdateNativeConfigurationRequest {
                expected_revision: after.data.revision.clone(),
                idempotency_key: "private-cloud-consent".into(),
                settings: cloud_change.clone(),
            })
            .await
            .unwrap();
        let after = a.native_configuration().await.unwrap();
        assert_eq!(after.data.settings, cloud_change);
        assert_eq!(after.data.revision, cloud_receipt.data.revision);
        assert_eq!(std::fs::read(&audio).unwrap(), audio_before);
        drop(a);
        drop(b);
        h.stop().await;
        h.start().await;
        let reopened = RepositoryClient::connect(&h.root).await.unwrap();
        let snapshot = reopened.native_configuration().await.unwrap();
        assert_eq!(snapshot.data, after.data);
        assert_ne!(
            snapshot.identity.library_epoch,
            after.identity.library_epoch
        );
        assert_eq!(
            reopened
                .update_native_configuration(request)
                .await
                .unwrap()
                .data,
            receipt.data
        );
        drop(reopened);
        h.stop().await;
    })
    .await
    .expect("bounded owned configuration service test");
}

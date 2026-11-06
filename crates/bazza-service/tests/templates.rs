#![cfg(any(windows, target_os = "macos"))]
//! Real private template transport against owned service processes only.
use bazza_core::{MigrationPolicy, Repository};
use bazza_service_api::{
    library::{DesktopLibraryItem, DesktopLibraryRequest},
    rpc::ErrorCode,
    templates::*,
};
use bazza_service_client::{
    repository::{RepositoryTemplateReadRequest, TemplateFieldContinuation},
    ClientError, RepositoryClient, ServiceClient,
};
use bazza_service_test_support::{create_v8, READY_MEETING_ID};
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread::JoinHandle,
    time::Duration,
};
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

struct Harness {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    child: Option<Child>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}
impl Harness {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("templates fixture Орион");
        create_v8(&root).unwrap();
        drop(Repository::open_fixture(&root, MigrationPolicy::Upgrade).unwrap());
        Self {
            _temporary: temporary,
            root,
            child: None,
            stderr: None,
        }
    }
    fn stderr(&mut self) -> String {
        let bytes = self
            .stderr
            .take()
            .map(|reader| reader.join().unwrap())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
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
        let mut child = command.spawn().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        self.stderr = Some(std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let retain = count.min(8192_usize.saturating_sub(kept.len()));
                        kept.extend_from_slice(&buffer[..retain]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            kept
        }));
        self.child = Some(child);
        let until = Instant::now() + Duration::from_secs(7);
        loop {
            if ServiceClient::connect(&self.root).await.is_ok() {
                break;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!(
                    "owned service exited before readiness ({status}): {}",
                    self.stderr()
                );
            }
            if Instant::now() >= until {
                let stderr = self.crash();
                panic!("owned service readiness timeout: {stderr}");
            }
            sleep(Duration::from_millis(15)).await;
        }
    }
    fn crash(&mut self) -> String {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        // Drain the owned reader before the next process replaces its handle.
        self.stderr()
    }
    async fn stop(&mut self) {
        let mut control = ServiceClient::connect(&self.root).await.unwrap();
        control.shutdown().await.unwrap();
        drop(control);
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    assert!(status.success(), "owned service failed: {}", self.stderr());
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned service shutdown timeout");
        self.child.take();
        let _ = self.stderr();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}
fn remote(error: ClientError, code: ErrorCode) {
    assert!(
        matches!(&error, ClientError::Remote(error) if error.code == code),
        "expected {code:?}: {error:?}"
    );
}
async fn describe(repository: &RepositoryClient, id: &str) -> TemplateHeader {
    let reply = repository.template_describe(id.into()).await.unwrap();
    assert_eq!(reply.identity, repository.identity().await);
    let DesktopTemplatesReply::Describe(header) = reply.data else {
        panic!("template header");
    };
    header
}
async fn read_field(
    repository: &RepositoryClient,
    header: &TemplateHeader,
    field: TemplateField,
) -> (String, usize) {
    let mut text = String::new();
    let mut cursor = None;
    for calls in 1..=128 {
        let page = repository
            .template_field(RepositoryTemplateReadRequest {
                template_id: header.id.clone(),
                expected_revision: header.revision.clone(),
                field,
                cursor,
                max_bytes: 2048,
            })
            .await
            .unwrap();
        assert_eq!(page.identity, repository.identity().await);
        assert_eq!(&page.header, header);
        assert_eq!(page.byte_offset, text.len() as u64);
        let wire = DesktopTemplatesReply::ReadField(TemplateFieldPage {
            library_scope: page.identity.cursor_scope,
            header: page.header,
            field: page.field,
            text: page.text.clone(),
            byte_offset: page.byte_offset,
            total_bytes: page.total_bytes,
            next: page.next.as_ref().map(|next| next.cursor.clone()),
        });
        assert!(template_body_fits(&wire, 2048));
        text.push_str(&page.text);
        cursor = page.next;
        if cursor.is_none() {
            return (text, calls);
        }
    }
    panic!("fixture field exceeded bounded page count");
}
async fn folder_template(repository: &RepositoryClient, folder_id: &str) -> Option<String> {
    let mut cursor = None;
    for _ in 0..16 {
        let page = repository.library_page(cursor, 100, 65_024).await.unwrap();
        for item in page.items {
            if let DesktopLibraryItem::Folder {
                id,
                default_template_id,
                ..
            } = item
            {
                if id == folder_id {
                    return default_template_id;
                }
            }
        }
        cursor = page.next;
        if cursor.is_none() {
            panic!("fixture folder not found");
        }
    }
    panic!("fixture library exceeded bounded page count");
}

#[tokio::test(flavor = "current_thread")]
async fn exact_template_upload_survives_process_restart_and_interoperates_with_library_and_changes()
{
    timeout(Duration::from_secs(35), async {
        let mut h = Harness::new();
        let audio = h
            .root
            .join("recordings")
            .join(READY_MEETING_ID)
            .join("mix.wav");
        let audio_before = std::fs::read(&audio).unwrap();
        h.start().await;
        let repository = RepositoryClient::connect(&h.root).await.unwrap();
        let second = RepositoryClient::connect(&h.root).await.unwrap();
        let original_meeting = repository
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        let id = format!(" Templates / шаблон 日本語 🚀 {} ", "界".repeat(50));
        assert!(id.len() > 128 && id.len() <= MAX_TEMPLATE_ID_BYTES);
        let chunks = [
            "PRIVATE_BODY\0{{trans".to_owned(),
            "cript}}\n原文 \"quoted\"\n".to_owned(),
            "я🙂\0".repeat(1200),
        ];
        // SHA-256 of the exact synthetic chunks above, including NUL bytes.
        let hashes = [
            "32ceffb23aa2fe9f519a0a0c35096e13788ba8ba8b35367fa0b7ade259bce8e1",
            "13c2421f606392c37271ba9b7ea4af9432c99b83e8f2ab2e84363af7e71d59ca",
            "d903bf58cca4db39efcdf500b6383e52eeabe857f6e022a19915715555411481",
        ];
        let body = chunks.concat();
        let upload_id = Uuid::new_v4();
        let begin = TemplateBeginWrite {
            upload_id,
            target: TemplateWriteTarget::New {
                requested_id: id.clone(),
            },
            name: "  Орион\0日本語  ".into(),
            description: "Описание\0🦉 exact".into(),
            total_bytes: body.len() as u64,
            sha256: "08b5b01dcbf9219c51d16075ece647b66b0bff7cba3abc3e00183e72b311f599".into(),
            idempotency_key: format!("begin-{upload_id}"),
        };
        let DesktopTemplatesReply::Upload(open) = repository
            .template_write_begin(begin.clone())
            .await
            .unwrap()
            .data
        else {
            panic!("upload opened");
        };
        assert_eq!(open.template_id, id);
        assert_eq!(open.status, TemplateUploadStatus::Open);
        let mut offset = 0;
        let mut first_append = None;
        for (index, (text, digest)) in chunks.iter().zip(hashes).enumerate() {
            let append = TemplateAppendWrite {
                upload_id,
                sequence: index as u32 + 1,
                byte_offset: offset,
                text: text.clone(),
                sha256: digest.into(),
                idempotency_key: format!("append-{upload_id}-{index}"),
            };
            if index == 0 {
                first_append = Some(append.clone());
            }
            let first = repository
                .template_write_append(append.clone())
                .await
                .unwrap();
            let replay = second.template_write_append(append).await.unwrap();
            assert_eq!(
                serde_json::to_value(&first.data).unwrap(),
                serde_json::to_value(replay.data).unwrap()
            );
            offset += text.len() as u64;
            let DesktopTemplatesReply::Upload(progress) = first.data else {
                panic!("upload progress");
            };
            assert_eq!(progress.received_bytes, offset);
            assert_eq!(progress.next_sequence, index as u32 + 2);
        }
        let commit = TemplateUploadRequest {
            upload_id,
            idempotency_key: format!("commit-{upload_id}"),
        };
        let DesktopTemplatesReply::Mutation(receipt) = repository
            .template_write_commit(commit.clone())
            .await
            .unwrap()
            .data
        else {
            panic!("committed receipt");
        };
        assert_eq!(receipt.effect, TemplateEffect::Saved);
        assert_eq!(receipt.template_id.as_deref(), Some(id.as_str()));
        let mut changed = first_append.as_ref().unwrap().clone();
        changed.text = "changed payload".into();
        changed.sha256 = "34f8bf4083ec15e056df717fdd8f42354e2f8d8091233f7a858811139058e548".into();
        remote(
            second
                .template_write_append(changed.clone())
                .await
                .unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );
        let header = describe(&repository, &id).await;
        assert_eq!(header.revision, receipt.revision);
        assert_eq!(header.name_bytes, begin.name.len() as u64);
        assert_eq!(header.description_bytes, begin.description.len() as u64);
        assert_eq!(header.body_bytes, body.len() as u64);
        assert_eq!(
            read_field(&repository, &header, TemplateField::Name)
                .await
                .0,
            begin.name
        );
        assert_eq!(
            read_field(&repository, &header, TemplateField::Description)
                .await
                .0,
            begin.description
        );
        let (actual_body, pages) = read_field(&repository, &header, TemplateField::Body).await;
        assert_eq!(actual_body, body);
        assert!(pages > 1);
        let stale_cursor: TemplateFieldContinuation = repository
            .template_field(RepositoryTemplateReadRequest {
                template_id: id.clone(),
                expected_revision: header.revision.clone(),
                field: TemplateField::Body,
                cursor: None,
                max_bytes: 2048,
            })
            .await
            .unwrap()
            .next
            .expect("body spans several fragments");
        let folder = repository
            .library_mutate(DesktopLibraryRequest::FolderCreate {
                name: "Template integration".into(),
                parent_id: None,
                idempotency_key: "template-folder-create".into(),
            })
            .await
            .unwrap()
            .data
            .outcome;
        repository
            .library_mutate(DesktopLibraryRequest::FolderSetTemplate {
                folder_id: folder.entity_id.clone(),
                template_id: Some(id.clone()),
                expected_revision: folder.revision,
                idempotency_key: "template-folder-default".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            folder_template(&second, &folder.entity_id).await.as_deref(),
            Some(id.as_str())
        );
        let feed = second.changes(None, 100, 65_024).await.unwrap();
        assert!(feed
            .items
            .iter()
            .any(|change| change.entity_kind == "template"
                && change.entity_id == id
                && change.revision == receipt.revision));
        let old_identity = repository.identity().await;
        drop(second);
        let _ = h.crash();
        h.start().await;
        let new_identity = repository.reconnect().await.unwrap();
        assert_eq!(new_identity.library_id, old_identity.library_id);
        assert_eq!(new_identity.cursor_scope, old_identity.cursor_scope);
        assert_ne!(new_identity.library_epoch, old_identity.library_epoch);
        remote(
            repository
                .template_field(RepositoryTemplateReadRequest {
                    template_id: id.clone(),
                    expected_revision: header.revision.clone(),
                    field: TemplateField::Body,
                    cursor: Some(stale_cursor),
                    max_bytes: 2048,
                })
                .await
                .unwrap_err(),
            ErrorCode::ResyncRequired,
        );
        assert_eq!(describe(&repository, &id).await, header);
        assert_eq!(
            read_field(&repository, &header, TemplateField::Body)
                .await
                .0,
            body
        );
        let DesktopTemplatesReply::Mutation(replayed) =
            repository.template_write_commit(commit).await.unwrap().data
        else {
            panic!("durable commit replay");
        };
        assert_eq!(replayed, receipt);
        let DesktopTemplatesReply::Upload(retired) = repository
            .template_write_append(first_append.unwrap())
            .await
            .unwrap()
            .data
        else {
            panic!("current retired upload state");
        };
        assert_eq!(retired.status, TemplateUploadStatus::Committed);
        assert_eq!(retired.receipt, Some(receipt.clone()));
        remote(
            repository.template_write_append(changed).await.unwrap_err(),
            ErrorCode::IdempotencyConflict,
        );
        let DesktopTemplatesReply::Upload(begun_again) =
            repository.template_write_begin(begin).await.unwrap().data
        else {
            panic!("durable upload intent");
        };
        assert_eq!(begun_again.template_id, id);
        assert_eq!(begun_again.status, TemplateUploadStatus::Committed);
        assert_eq!(
            folder_template(&repository, &folder.entity_id)
                .await
                .as_deref(),
            Some(id.as_str())
        );
        let after = repository
            .meeting(READY_MEETING_ID.into())
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(
            serde_json::to_value(after).unwrap(),
            serde_json::to_value(original_meeting).unwrap()
        );
        assert_eq!(std::fs::read(&audio).unwrap(), audio_before);
        assert!(!h.root.join("models").exists());
        drop(repository);
        h.stop().await;
    })
    .await
    .expect("bounded real templates process integration");
}

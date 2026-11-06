//! Own the Store for a single explicitly admitted library.

use bazza_service_api::rpc::{
    build_id, ApiVersion, MeetingMetadata, ServiceStatus, FIXTURE_CAPTURE_TERMINAL_V2_CAPABILITY,
    FIXTURE_METADATA_CAPABILITY, FIXTURE_STATUS_CAPABILITY, FIXTURE_WORKER_CAPABILITY,
};
pub use bazza_store::MigrationPolicy;
use bazza_store::{LibraryOwnerGuard, Store, SUPPORTED_SCHEMA};
mod adoption_rehearsal;
pub use adoption_rehearsal::{rehearse_adoption, AdoptionRehearsal};
mod chats;
mod configuration;
pub mod data;
pub mod desktop;
mod external_read_grants;
mod fixture_notes;
mod fixture_read;
mod fixture_structure;
mod fixture_summary;
mod fixture_transcript;
mod managed_library;
mod postprocess;
mod postprocess_cleanup;
mod postprocess_finish;
mod postprocess_input;
mod postprocess_lifecycle;
mod postprocess_media;
mod postprocess_result;
mod postprocess_source;
mod postprocess_video;
mod templates;
pub mod worker;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;

pub const IDENTITY_FILE: &str = bazza_store::SERVICE_IDENTITY_FILE;
const FIXTURE_MARKER: &str = ".bazza-synthetic-fixture";
const FIXTURE_VALUE: &str = "bazza-mcp-v8-fixture-v1\n";
const FIXTURE_IDENTITY_VERSION: u16 = 1;
const MANAGED_IDENTITY_VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryMode {
    SyntheticFixture,
    Managed,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("capture handoff: {0}")]
    Handoff(#[from] bazza_capture_handoff::HandoffError),
    #[error("only an explicit synthetic library is supported before GUI cutover")]
    SyntheticFixtureRequired,
    #[error("invalid library identity")]
    InvalidIdentity,
    #[error("managed library requires a new absolute destination outside an existing library")]
    InvalidManagedPath,
    #[error("the selected library is not a managed service library")]
    ManagedLibraryRequired,
    #[error("adoption rehearsal requires distinct absolute paths and a new destination outside the library")]
    InvalidRehearsalPath,
    #[error("library storage: {0}")]
    Store(#[from] bazza_store::StoreError),
    #[error("library data: {0}")]
    Data(#[from] bazza_store::service_data::DataError),
    #[error("library I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("library identity encoding: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u16,
    library_id: Uuid,
}

/// The Store holds the canonical owner guard until SQLite has closed.
pub struct Repository {
    store: Store,
    library_id: Uuid,
    epoch: Uuid,
    mode: RepositoryMode,
}

fn regular_file(path: &Path) -> std::io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "regular library file required",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "regular library file required",
            ));
        }
    }
    Ok(())
}

fn bounded_read(path: &Path, maximum: u64) -> std::io::Result<Vec<u8>> {
    regular_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "library metadata exceeds limit",
        ));
    }
    Ok(bytes)
}

pub fn require_synthetic_fixture(root: &Path) -> Result<(), CoreError> {
    if bounded_read(&root.join(FIXTURE_MARKER), 128)
        .ok()
        .as_deref()
        != Some(FIXTURE_VALUE.as_bytes())
    {
        return Err(CoreError::SyntheticFixtureRequired);
    }
    if read_identity_if_present(root)?
        .is_some_and(|identity| identity.version != FIXTURE_IDENTITY_VERSION)
    {
        return Err(CoreError::SyntheticFixtureRequired);
    }
    Ok(())
}

impl Repository {
    pub fn open_fixture(root: &Path, migration: MigrationPolicy) -> Result<Self, CoreError> {
        require_synthetic_fixture(root)?;
        let guard = LibraryOwnerGuard::acquire_existing(root)?;
        let root = guard.root().to_path_buf();
        require_synthetic_fixture(&root)?;
        let store = Store::open_existing_owned(guard, migration)?;
        initialize_service_projections(&store)?;
        let identity = load_or_create_identity(&root)?;
        Ok(Self::from_owned_store(
            store,
            identity,
            RepositoryMode::SyntheticFixture,
        ))
    }

    fn from_owned_store(store: Store, identity: Identity, mode: RepositoryMode) -> Self {
        Self {
            store,
            library_id: identity.library_id,
            epoch: Uuid::new_v4(),
            mode,
        }
    }

    pub fn mode(&self) -> RepositoryMode {
        self.mode
    }

    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            api: ApiVersion::CURRENT,
            schema_version: SUPPORTED_SCHEMA,
            build_id: build_id().into(),
            library_id: self.library_id,
            library_epoch: self.epoch,
            process_id: std::process::id(),
            authentication: "local_os_user".into(),
            capabilities: vec![
                FIXTURE_STATUS_CAPABILITY.into(),
                FIXTURE_METADATA_CAPABILITY.into(),
                FIXTURE_WORKER_CAPABILITY.into(),
                FIXTURE_CAPTURE_TERMINAL_V2_CAPABILITY.into(),
                bazza_service_api::rpc::FIXTURE_CAPTURE_LOCAL_LIVE_CAPABILITY.into(),
                bazza_service_api::transcript_versions::DESKTOP_TRANSCRIPT_VERSIONS_CAPABILITY.into(),
                bazza_service_api::soniox_permission::FIXTURE_SONIOX_PERMISSION_CAPABILITY.into(),
                bazza_service_api::fixture_read::FIXTURE_READ_CAPABILITY.into(),
                bazza_service_api::chats::FIXTURE_CHATS_CAPABILITY.into(),
                bazza_service_api::desktop::FIXTURE_DESKTOP_CAPABILITY.into(),
                bazza_service_api::content::FIXTURE_CONTENT_CAPABILITY.into(),
                bazza_service_api::library::FIXTURE_LIBRARY_CAPABILITY.into(),
                "experimental_fixture_native_configuration".into(),
                bazza_service_api::postprocess::FIXTURE_POST_PROCESS_CAPABILITY.into(),
                bazza_service_api::postprocess_input::FIXTURE_POST_PROCESS_INPUT_CAPABILITY.into(),
                bazza_service_api::postprocess_media::FIXTURE_POST_PROCESS_MEDIA_CAPABILITY.into(),
                bazza_service_api::postprocess_video::FIXTURE_POST_PROCESS_VIDEO_CAPABILITY.into(),
                bazza_service_api::postprocess_cleanup::FIXTURE_POST_PROCESS_CLEANUP_CAPABILITY
                    .into(),
                bazza_service_api::postprocess_result::FIXTURE_POST_PROCESS_RESULT_CAPABILITY
                    .into(),
                bazza_service_api::postprocess_source::FIXTURE_POST_PROCESS_SOURCE_CAPABILITY
                    .into(),
                bazza_service_api::postprocess_speakers::FIXTURE_POST_PROCESS_SPEAKERS_CAPABILITY
                    .into(),
                bazza_service_api::postprocess_ready::FIXTURE_POST_PROCESS_READY_CAPABILITY.into(),
                bazza_service_api::postprocess_summary::FIXTURE_POST_PROCESS_SUMMARY_CAPABILITY
                    .into(),
                bazza_service_api::postprocess_complete::FIXTURE_POST_PROCESS_COMPLETE_CAPABILITY
                    .into(),
                bazza_service_api::templates::FIXTURE_TEMPLATES_CAPABILITY.into(),
            ],
            development_fixture_only: self.mode == RepositoryMode::SyntheticFixture,
        }
    }

    pub fn meeting(&self, id: Uuid) -> Result<Option<MeetingMetadata>, CoreError> {
        Ok(self
            .store
            .service_meeting(&id.to_string())?
            .map(|projection| {
                let meeting = projection.meeting;
                MeetingMetadata {
                    id: meeting.id,
                    title: meeting.title,
                    started_at: meeting.started_at.to_rfc3339(),
                    ended_at: meeting.ended_at.map(|time| time.to_rfc3339()),
                    duration_ms: meeting.duration_ms,
                    status: match meeting.status {
                        bazza_store::MeetingStatus::Recording => "recording",
                        bazza_store::MeetingStatus::Processing => "processing",
                        bazza_store::MeetingStatus::Ready => "ready",
                        bazza_store::MeetingStatus::Failed => "failed",
                    }
                    .into(),
                    participants: meeting.participants,
                }
            }))
    }
}

fn initialize_service_projections(store: &Store) -> Result<(), CoreError> {
    store.service_initialize_library()?;
    // Backfill is atomic and readiness is never inferred from schema presence.
    if store.service_search_state()?.0 != "ready" {
        store.service_rebuild_search()?;
    }
    Ok(())
}

fn load_or_create_identity(root: &Path) -> Result<Identity, CoreError> {
    let identity = match read_identity_if_present(root)? {
        Some(identity) => identity,
        None => persist_identity(root, FIXTURE_IDENTITY_VERSION)?,
    };
    if identity.version != FIXTURE_IDENTITY_VERSION {
        return Err(CoreError::InvalidIdentity);
    }
    Ok(identity)
}

fn read_identity_if_present(root: &Path) -> Result<Option<Identity>, CoreError> {
    let path = root.join(IDENTITY_FILE);
    match path.symlink_metadata() {
        Ok(_) => {
            let identity: Identity = serde_json::from_slice(&bounded_read(&path, 4096)?)?;
            if identity.library_id.is_nil() {
                return Err(CoreError::InvalidIdentity);
            }
            Ok(Some(identity))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn persist_identity(root: &Path, version: u16) -> Result<Identity, CoreError> {
    let identity = Identity {
        version,
        library_id: Uuid::new_v4(),
    };
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut file, &identity)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist_noclobber(root.join(IDENTITY_FILE))
        .map_err(|error| error.error)?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_service_test_support::{create_v8, READY_MEETING_ID};

    #[test]
    fn explicit_upgrade_preserves_data_and_identity_but_refreshes_epoch() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temporary.path().join("fixture")).unwrap();
        let before = std::fs::read(&fixture.database_path).unwrap();
        assert!(Repository::open_fixture(&fixture.root, MigrationPolicy::RequireCurrent).is_err());
        assert_eq!(std::fs::read(&fixture.database_path).unwrap(), before);
        assert!(!fixture.root.join(IDENTITY_FILE).exists());
        let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let first = repository.status();
        assert_eq!(first.schema_version, SUPPORTED_SCHEMA);
        let meeting = repository
            .meeting(READY_MEETING_ID.parse().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(meeting.title, "Орион / Budget review");
        assert_eq!(meeting.participants, ["Анна", "Alex"]);
        assert!(Repository::open_fixture(&fixture.root, MigrationPolicy::RequireCurrent).is_err());
        assert!(matches!(
            Store::open(&fixture.database_path),
            Err(bazza_store::StoreError::ServiceRequired)
        ));
        drop(repository);
        assert!(matches!(
            Store::open(&fixture.database_path),
            Err(bazza_store::StoreError::ServiceRequired)
        ));
        let reopened =
            Repository::open_fixture(&fixture.root, MigrationPolicy::RequireCurrent).unwrap();
        assert_eq!(reopened.status().library_id, first.library_id);
        assert_ne!(reopened.status().library_epoch, first.library_epoch);
    }

    #[test]
    fn no_default_library_or_unmarked_path_can_be_opened() {
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing");
        assert!(Repository::open_fixture(&missing, MigrationPolicy::Upgrade).is_err());
        assert!(!missing.exists());
        let fixture = create_v8(&temporary.path().join("fixture")).unwrap();
        std::fs::remove_file(fixture.root.join(FIXTURE_MARKER)).unwrap();
        assert!(Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).is_err());
        assert!(!fixture.root.join(IDENTITY_FILE).exists());
    }

    #[test]
    fn common_data_core_reopens_documents_dedup_and_frozen_baseline() {
        use crate::data::*;
        let temporary = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temporary.path().join("library")).unwrap();
        let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        assert_eq!(SUPPORTED_SCHEMA, bazza_service_api::rpc::STORAGE_SCHEMA_MAX);
        let data = repository.data();
        let raw = data
            .transcript(READY_MEETING_ID, &TranscriptRead::default())
            .unwrap();
        assert_eq!(raw.segments[0].text, "ээ проект Орион");
        assert_eq!(raw.segments[2].clean_text.as_deref(), Some(""));
        let original_source = raw.source_id.unwrap();
        let ctx = MutationContext {
            principal: "fixture-agent".into(),
            idempotency_key: "durable-note".into(),
        };
        let input = NewNote {
            title: "Saved decision".into(),
            markdown: "quasarDecision".into(),
            meeting_id: Some(READY_MEETING_ID.into()),
        };
        let outcome = data.create_note(&ctx, &input).unwrap();
        assert_eq!(
            data.search(&SearchRequest {
                query: "quasarDecision".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .hits[0]
                .source_kind,
            "note"
        );
        let summary = data.summary(READY_MEETING_ID).unwrap();
        assert_eq!(summary.state.origin, "legacy");
        assert!(!summary.state.auto_replace);
        let old_library = repository.status().library_id;
        drop(repository);
        let reopened =
            Repository::open_fixture(&fixture.root, MigrationPolicy::RequireCurrent).unwrap();
        assert_eq!(reopened.status().library_id, old_library);
        assert_eq!(reopened.data().create_note(&ctx, &input).unwrap(), outcome);
        assert_eq!(
            reopened
                .data()
                .transcript(READY_MEETING_ID, &TranscriptRead::default())
                .unwrap()
                .source_id
                .as_deref(),
            Some(original_source.as_str())
        );
        assert_eq!(
            reopened
                .data()
                .notes(Some(READY_MEETING_ID), false, None, 20)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn private_metadata_hides_tombstone_before_physical_purge() {
        use crate::data::*;
        let temporary = tempfile::tempdir().unwrap();
        let fixture = create_v8(&temporary.path().join("library")).unwrap();
        let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let m = repository
            .data()
            .meeting(READY_MEETING_ID)
            .unwrap()
            .unwrap();
        let ctx = MutationContext {
            principal: "fixture-agent".into(),
            idempotency_key: "delete".into(),
        };
        repository
            .data()
            .create_note(
                &MutationContext {
                    principal: "fixture-agent".into(),
                    idempotency_key: "linked".into(),
                },
                &NewNote {
                    title: "linked".into(),
                    markdown: "linked body".into(),
                    meeting_id: Some(READY_MEETING_ID.into()),
                },
            )
            .unwrap();
        let standalone = repository
            .data()
            .create_note(
                &MutationContext {
                    principal: "fixture-agent".into(),
                    idempotency_key: "standalone".into(),
                },
                &NewNote {
                    title: "standalone".into(),
                    markdown: "standalone body".into(),
                    meeting_id: None,
                },
            )
            .unwrap();
        repository
            .data()
            .delete_meeting(&ctx, READY_MEETING_ID, &m.lifecycle_revision)
            .unwrap();
        assert!(repository
            .meeting(READY_MEETING_ID.parse().unwrap())
            .unwrap()
            .is_none());
        assert!(repository
            .data()
            .notes(Some(READY_MEETING_ID), false, None, 20)
            .unwrap()
            .is_empty());
        assert!(repository
            .data()
            .note(&standalone.entity_id)
            .unwrap()
            .is_some());
        assert!(fixture
            .root
            .join("recordings")
            .join(READY_MEETING_ID)
            .join("mix.wav")
            .exists());
    }
}
mod transcript_versions;
pub use transcript_versions::TranscriptVersionTransfer;

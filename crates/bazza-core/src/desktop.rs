//! Typed desktop projection boundary, still restricted by Repository's fixture
//! opener. Capture/STT credentials and arbitrary settings are not part of it.
use crate::{CoreError, Repository};
use bazza_service_api::agent_summary::CommitDesktopAgentSummaryRequest;
use bazza_service_api::content as content_wire;
use bazza_service_api::desktop as wire;
use bazza_service_api::library as library_wire;
use bazza_store::service_data as data;
use serde::{de::DeserializeOwned, Serialize};

fn convert<T: Serialize, D: DeserializeOwned>(value: &T) -> Result<D, CoreError> {
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}
pub(super) fn authenticated_peer(peer_pid: u32) -> Result<(), CoreError> {
    if peer_pid == 0 {
        return Err(data::DataError::Forbidden.into());
    }
    Ok(())
}
pub(super) fn context(peer_pid: u32, key: String) -> Result<data::MutationContext, CoreError> {
    authenticated_peer(peer_pid)?;
    // One trusted local application principal across windows and GUI restarts.
    // A process PID is actor context, not a stable deduplication identity.
    Ok(data::MutationContext {
        principal: "desktop".into(),
        idempotency_key: key,
    })
}
impl Repository {
    pub(super) fn desktop_envelope<T: Serialize>(
        &self,
        value: T,
    ) -> Result<wire::DesktopEnvelope<T>, CoreError> {
        let envelope = wire::DesktopEnvelope {
            library_id: self.library_id,
            library_epoch: self.epoch,
            data: value,
        };
        struct Budget(usize);
        impl std::io::Write for Budget {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .filter(|n| *n <= wire::MAX_DESKTOP_REPLY_BYTES)
                    .ok_or_else(|| std::io::Error::other("desktop response budget"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Budget(0), &envelope)
            .map_err(|_| data::DataError::Invalid("desktop response budget"))?;
        Ok(envelope)
    }
    pub fn desktop_bootstrap(
        &self,
        peer_pid: u32,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopBootstrap>, CoreError> {
        authenticated_peer(peer_pid)?;
        self.desktop_envelope(convert(&self.store.service_desktop_bootstrap()?)?)
    }
    pub fn desktop_preferences(
        &self,
        peer_pid: u32,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopPreferences>, CoreError> {
        authenticated_peer(peer_pid)?;
        self.desktop_envelope(convert(&self.store.service_desktop_preferences()?)?)
    }
    pub fn update_desktop_preferences(
        &self,
        peer_pid: u32,
        request: wire::UpdateDesktopPreferencesRequest,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopMutationOutcome>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop preferences request").into());
        }
        let ctx = context(peer_pid, request.idempotency_key)?;
        let result = self.store.service_update_desktop_preferences(
            &ctx,
            &request.expected_revision,
            &convert(&request.patch)?,
        )?;
        self.desktop_envelope(convert(&result)?)
    }
    pub fn desktop_meetings(
        &self,
        peer_pid: u32,
        mut request: wire::DesktopMeetingRequest,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopMeetingPage>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop meeting page").into());
        }
        request.max_bytes = request.max_bytes.min(wire::MAX_DESKTOP_DATA_BYTES);
        self.desktop_envelope(convert(
            &self.store.service_desktop_meetings(&convert(&request)?)?,
        )?)
    }
    pub fn desktop_meeting(
        &self,
        peer_pid: u32,
        request: wire::DesktopMeetingLookup,
    ) -> Result<wire::DesktopEnvelope<Option<wire::DesktopMeeting>>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !wire::desktop_id(&request.meeting_id) {
            return Err(data::DataError::Invalid("meeting identifier").into());
        }
        self.desktop_envelope(convert(
            &self.store.service_desktop_meeting(&request.meeting_id)?,
        )?)
    }
    pub fn desktop_meeting_decorations(
        &self,
        peer_pid: u32,
        request: wire::DesktopMeetingLookup,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopMeetingDecorations>, CoreError> {
        authenticated_peer(peer_pid)?;
        let result: wire::DesktopMeetingDecorations = convert(
            &self
                .store
                .service_desktop_meeting_decorations(&request.meeting_id)?,
        )?;
        if !result.valid(&request.meeting_id) {
            return Err(data::DataError::Invalid("desktop meeting decorations").into());
        }
        self.desktop_envelope(result)
    }
    pub fn desktop_changes(
        &self,
        peer_pid: u32,
        mut request: wire::DesktopChangesRequest,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopChangePage>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop changes request").into());
        }
        request.max_bytes = request.max_bytes.min(wire::MAX_DESKTOP_DATA_BYTES);
        self.desktop_envelope(convert(
            &self.store.service_desktop_changes(&convert(&request)?)?,
        )?)
    }
    pub fn rename_desktop_meeting(
        &self,
        peer_pid: u32,
        request: wire::RenameDesktopMeetingRequest,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopMutationOutcome>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop rename request").into());
        }
        let ctx = context(peer_pid, request.idempotency_key)?;
        let title = request.title.trim();
        let outcome = self.store.service_update_meeting(
            &ctx,
            &request.meeting_id,
            &request.expected_revision,
            &data::MeetingUpdate {
                title: Some(title.into()),
                ..Default::default()
            },
        )?;
        self.desktop_envelope(convert(&outcome)?)
    }

    pub fn delete_desktop_meeting(
        &self,
        peer_pid: u32,
        request: wire::DeleteDesktopMeetingRequest,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopMutationOutcome>, CoreError> {
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop delete request").into());
        }
        let ctx = context(peer_pid, request.idempotency_key.clone())?;
        let outcome: wire::DesktopMutationOutcome = convert(&self.store.service_delete_meeting(
            &ctx,
            &request.meeting_id,
            &request.expected_lifecycle_revision,
        )?)?;
        if !request.accepts(&outcome) {
            return Err(data::DataError::Invalid("desktop delete outcome").into());
        }
        self.desktop_envelope(outcome)
    }

    /// Service monitor only. The cursor carries an internal job identity.
    pub fn next_desktop_meeting_purge(
        &self,
        after: Option<&str>,
    ) -> Result<Option<String>, CoreError> {
        Ok(self.store.service_next_pending_meeting_purge(after)?)
    }

    pub fn desktop_transcript(
        &self,
        peer_pid: u32,
        mut request: content_wire::DesktopTranscriptRequest,
    ) -> Result<wire::DesktopEnvelope<content_wire::DesktopTranscriptPage>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop transcript page").into());
        }
        request.max_bytes = request.max_bytes.min(wire::MAX_DESKTOP_DATA_BYTES);
        self.desktop_envelope(convert(
            &self.store.service_desktop_transcript(&convert(&request)?)?,
        )?)
    }

    pub fn desktop_summary(
        &self,
        peer_pid: u32,
        mut request: content_wire::DesktopSummaryRequest,
    ) -> Result<wire::DesktopEnvelope<content_wire::DesktopSummaryPage>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop summary page").into());
        }
        request.max_bytes = request.max_bytes.min(wire::MAX_DESKTOP_DATA_BYTES);
        self.desktop_envelope(convert(
            &self.store.service_desktop_summary(&convert(&request)?)?,
        )?)
    }

    pub fn commit_desktop_agent_summary(
        &self,
        peer_pid: u32,
        request: CommitDesktopAgentSummaryRequest,
    ) -> Result<wire::DesktopEnvelope<wire::DesktopMutationOutcome>, CoreError> {
        let ctx = context(peer_pid, request.idempotency_key.clone())?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop agent summary request").into());
        }
        if request.library_id != self.library_id || request.library_epoch != self.epoch {
            return Err(data::DataError::StaleCursor.into());
        }
        let outcome = convert(&self.store.service_commit_agent_summary(&ctx, &request.result)?)?;
        if !request.accepts(&outcome) {
            return Err(data::DataError::Invalid("desktop agent summary outcome").into());
        }
        self.desktop_envelope(outcome)
    }

    pub fn desktop_library(
        &self,
        peer_pid: u32,
        request: library_wire::DesktopLibraryRequest,
    ) -> Result<wire::DesktopEnvelope<library_wire::DesktopLibraryReply>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(data::DataError::Invalid("desktop library request").into());
        }
        let reply = match request {
            library_wire::DesktopLibraryRequest::List {
                cursor,
                limit,
                max_bytes,
            } => {
                let page = library_wire::DesktopLibraryPageRequest {
                    cursor,
                    limit,
                    max_bytes: max_bytes.min(wire::MAX_DESKTOP_DATA_BYTES),
                };
                library_wire::DesktopLibraryReply::Page(convert(
                    &self.store.service_desktop_library_page(&convert(&page)?)?,
                )?)
            }
            request => {
                let operation = request
                    .operation()
                    .ok_or(data::DataError::Invalid("library operation"))?;
                let key = request
                    .idempotency_key()
                    .ok_or(data::DataError::Invalid("library mutation key"))?
                    .to_owned();
                let ctx = context(peer_pid, key)?;
                let outcome = self
                    .store
                    .service_desktop_library_mutate(&ctx, &convert(&request)?)?;
                library_wire::DesktopLibraryReply::Mutation(
                    library_wire::DesktopLibraryMutationReply {
                        operation,
                        outcome: convert(&outcome)?,
                    },
                )
            }
        };
        self.desktop_envelope(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_test_support::{create_v8, READY_MEETING_ID};

    #[test]
    fn bootstrap_exposes_only_narrow_preferences_and_validated_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        repo.store.set_setting("settings",&serde_json::json!({"llm_api_key":"private-fixture-token","soniox_consent":false,"video_fps":15,"capture_system":true,"close_to_tray":false,"hotkey":"Ctrl+F8"})).unwrap();
        let bootstrap = repo.desktop_bootstrap(42).unwrap();
        assert_eq!(bootstrap.library_id, repo.library_id);
        assert_eq!(bootstrap.library_epoch, repo.epoch);
        assert!(!bootstrap.data.preferences.close_to_tray);
        assert_eq!(bootstrap.data.preferences.hotkey, "Ctrl+F8");
        let json = serde_json::to_string(&bootstrap).unwrap();
        assert!(!json.contains("private-fixture-token"));
        assert!(!json.contains("video_fps"));
        assert!(!json.contains("soniox"));
        assert!(matches!(
            repo.desktop_bootstrap(0),
            Err(CoreError::Data(data::DataError::Forbidden))
        ));
    }
    #[test]
    fn preference_retry_across_peer_processes_keeps_original_outcome_and_other_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let original = serde_json::json!({"video_fps":30,"llm_api_key":"fixture-private","unknown":{"version":7},"stt_provider":"local"});
        repo.store.set_setting("settings", &original).unwrap();
        let snapshot = repo.desktop_preferences(101).unwrap().data;
        let request = wire::UpdateDesktopPreferencesRequest {
            expected_revision: snapshot.revision.clone(),
            idempotency_key: "preferences-retry".into(),
            patch: wire::DesktopPreferencesPatch {
                ui_locale: Some(wire::DesktopLocale::En),
                close_to_tray: Some(false),
                hotkey: Some("Ctrl+F8".into()),
            },
        };
        let first = repo
            .update_desktop_preferences(101, request.clone())
            .unwrap()
            .data;
        let retry = repo.update_desktop_preferences(202, request).unwrap().data;
        assert_eq!(first, retry);
        let saved = repo
            .store
            .get_setting::<serde_json::Value>("settings")
            .unwrap()
            .unwrap();
        for field in ["video_fps", "llm_api_key", "unknown", "stt_provider"] {
            assert_eq!(saved[field], original[field]);
        }
        let conflict = wire::UpdateDesktopPreferencesRequest {
            expected_revision: snapshot.revision,
            idempotency_key: "other".into(),
            patch: wire::DesktopPreferencesPatch {
                close_to_tray: Some(true),
                ..Default::default()
            },
        };
        assert!(matches!(
            repo.update_desktop_preferences(101, conflict),
            Err(CoreError::Data(data::DataError::Conflict { .. }))
        ));
    }
    #[test]
    fn rename_uses_read_revision_and_cannot_overwrite_a_newer_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let initial = repo
            .desktop_meeting(
                42,
                wire::DesktopMeetingLookup {
                    meeting_id: READY_MEETING_ID.into(),
                },
            )
            .unwrap()
            .data
            .unwrap();
        let original_source = repo.data().transcript_state(READY_MEETING_ID).unwrap();
        let first = wire::RenameDesktopMeetingRequest {
            meeting_id: READY_MEETING_ID.into(),
            expected_revision: initial.metadata_revision.clone(),
            idempotency_key: "rename-1".into(),
            title: "  Deliberate title  ".into(),
        };
        let ack = repo.rename_desktop_meeting(42, first.clone()).unwrap().data;
        assert_eq!(ack, repo.rename_desktop_meeting(43, first).unwrap().data);
        let second = wire::RenameDesktopMeetingRequest {
            meeting_id: READY_MEETING_ID.into(),
            expected_revision: initial.metadata_revision,
            idempotency_key: "rename-2".into(),
            title: "Stale window title".into(),
        };
        assert!(matches!(
            repo.rename_desktop_meeting(42, second),
            Err(CoreError::Data(data::DataError::Conflict { .. }))
        ));
        let updated = repo
            .desktop_meeting(
                42,
                wire::DesktopMeetingLookup {
                    meeting_id: READY_MEETING_ID.into(),
                },
            )
            .unwrap()
            .data
            .unwrap();
        assert_eq!(updated.title, "Deliberate title");
        assert_eq!(updated.participants, initial.participants);
        assert_eq!(
            repo.data().transcript_state(READY_MEETING_ID).unwrap(),
            original_source
        );
    }
    #[test]
    fn agent_summary_requires_a_desktop_peer_and_the_observed_library_epoch() {
        use bazza_domain::agent_summary::{AgentSummaryCommit, SummaryProvider};
        use uuid::Uuid;

        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let request = CommitDesktopAgentSummaryRequest {
            library_id: repo.library_id,
            library_epoch: Uuid::new_v4(),
            idempotency_key: "agent-summary".into(),
            result: AgentSummaryCommit {
                meeting_id: READY_MEETING_ID.into(),
                expected_lifecycle_revision: "valid-lifecycle".into(),
                expected_transcript_revision: "valid-transcript".into(),
                expected_source_id: "valid-source".into(),
                expected_summary_revision: "valid-summary".into(),
                markdown: "Synthetic agent result".into(),
                structured_json: serde_json::json!({}),
                template_id: "general".into(),
                model: "test-model".into(),
                provider: SummaryProvider::Codex,
            },
        };
        assert!(matches!(
            repo.commit_desktop_agent_summary(0, request.clone()),
            Err(CoreError::Data(data::DataError::Forbidden))
        ));
        assert!(matches!(
            repo.commit_desktop_agent_summary(42, request),
            Err(CoreError::Data(data::DataError::StaleCursor))
        ));
    }

    #[test]
    fn oversized_desktop_request_is_rejected_instead_of_silently_clamped() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        assert!(matches!(
            repo.desktop_meetings(
                42,
                wire::DesktopMeetingRequest {
                    max_bytes: 65_537,
                    ..Default::default()
                }
            ),
            Err(CoreError::Data(data::DataError::Invalid(_)))
        ));
    }

    #[test]
    fn content_reads_authenticate_and_validate_before_access() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = create_v8(&tmp.path().join("library")).unwrap();
        let repo = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let revision = repo
            .data()
            .transcript_state(READY_MEETING_ID)
            .unwrap()
            .revision;
        let mut transcript = content_wire::DesktopTranscriptRequest {
            meeting_id: READY_MEETING_ID.into(),
            source_id: None,
            expected_revision: revision,
            representation: content_wire::DesktopTranscriptRepresentation::Raw,
            cursor: None,
            limit: 20,
            max_bytes: 2048,
        };
        assert!(matches!(
            repo.desktop_transcript(0, transcript.clone()),
            Err(CoreError::Data(data::DataError::Forbidden))
        ));
        transcript.max_bytes = 65_537;
        assert!(matches!(
            repo.desktop_transcript(42, transcript),
            Err(CoreError::Data(data::DataError::Invalid(_)))
        ));
        let mut summary = content_wire::DesktopSummaryRequest {
            meeting_id: READY_MEETING_ID.into(),
            expected_revision: repo
                .data()
                .summary(READY_MEETING_ID)
                .unwrap()
                .state
                .revision,
            field: content_wire::DesktopSummaryField::StructuredJson,
            cursor: None,
            max_bytes: 2048,
        };
        assert!(matches!(
            repo.desktop_summary(0, summary.clone()),
            Err(CoreError::Data(data::DataError::Forbidden))
        ));
        summary.max_bytes = 0;
        assert!(matches!(
            repo.desktop_summary(42, summary),
            Err(CoreError::Data(data::DataError::Invalid(_)))
        ));
    }
}

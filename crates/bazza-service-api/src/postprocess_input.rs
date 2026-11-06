//! Private reads of the original transcript owned by a live post-process claim.
//! No media path, executor, output publication or caller authority is selected.
use crate::{
    content::{DesktopDocumentState, DesktopTranscriptPage, DesktopTranscriptRequest},
    desktop::{desktop_body_fits, MAX_DESKTOP_DATA_BYTES},
    postprocess::PostProcessJobLease,
    worker::{WorkerProof, WorkerSecret},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

pub const FIXTURE_POST_PROCESS_INPUT_CAPABILITY: &str =
    "experimental_fixture_post_process_transcript";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostProcessTranscriptRequest {
    pub proof: WorkerProof,
    pub lease: PostProcessJobLease,
    pub fence: WorkerSecret,
    pub page: DesktopTranscriptRequest,
}

impl PostProcessTranscriptRequest {
    pub fn valid(&self) -> bool {
        self.page.valid()
            && self.lease.valid()
            && !self.proof.session_id.is_nil()
            && !self.proof.library_epoch.is_nil()
            && self.proof.session_id == self.lease.worker_session_id
            && self.proof.library_epoch == self.lease.library_epoch
            && self.lease.expires_at > Utc::now()
            && self.page.meeting_id == self.lease.meeting_id.to_string()
            && self.page.expected_revision == self.lease.input.transcript_revision
            && self
                .page
                .source_id
                .as_ref()
                .is_none_or(|source| self.lease.input.source_id.as_ref() == Some(source))
            && self.page.cursor.as_ref().is_none_or(|cursor| {
                cursor.current_source_id == self.lease.input.source_id
                    && cursor.source_id == self.lease.input.source_id
            })
            && desktop_body_fits(self, MAX_DESKTOP_DATA_BYTES as usize)
    }

    /// Immutable input pins only. Receiving a reply neither renews nor checks
    /// ownership; the server authenticates the live claim before every read.
    /// Fragment shape/progress/budgets are checked by the content boundary.
    pub fn reply_pins_valid(&self, page: &DesktopTranscriptPage) -> bool {
        page.state.meeting_id == self.lease.meeting_id.to_string()
            && page.state.revision == self.lease.input.transcript_revision
            && page.state.current_source_id == self.lease.input.source_id
            && page.source_id == self.lease.input.source_id
            && page.representation == self.page.representation
            && page.state.document_state
                == if self.lease.input.source_id.is_some() {
                    DesktopDocumentState::Present
                } else {
                    DesktopDocumentState::Absent
                }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        content::{
            DesktopTranscriptCursor, DesktopTranscriptRepresentation, DesktopTranscriptState,
        },
        postprocess::PostProcessSourceFence,
    };
    use chrono::Duration;
    use uuid::Uuid;

    fn request() -> PostProcessTranscriptRequest {
        let session = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let meeting = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        PostProcessTranscriptRequest {
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
                job_id: "processing-job".into(),
                meeting_id: meeting,
                job_revision: "claimed-revision".into(),
                input: PostProcessSourceFence {
                    capture_id: Uuid::new_v4(),
                    capture_generation: Uuid::new_v4(),
                    checkpoint_revision: "checkpoint-original".into(),
                    lifecycle_revision: "finalized-lifecycle".into(),
                    transcript_revision: "original-transcript".into(),
                    source_id: Some("captured-source".into()),
                },
                expires_at: Utc::now() + Duration::seconds(120),
                change_sequence: 5,
                dedup_until: Utc::now() + Duration::days(7),
            },
            fence: WorkerSecret::random(),
            page: DesktopTranscriptRequest {
                meeting_id: meeting.to_string(),
                source_id: None,
                expected_revision: "original-transcript".into(),
                representation: DesktopTranscriptRepresentation::Raw,
                cursor: None,
                limit: 1,
                max_bytes: 4096,
            },
        }
    }

    fn cursor(request: &PostProcessTranscriptRequest) -> DesktopTranscriptCursor {
        DesktopTranscriptCursor {
            library_scope: "a".repeat(32),
            meeting_id: request.page.meeting_id.clone(),
            revision: request.page.expected_revision.clone(),
            current_source_id: request.lease.input.source_id.clone(),
            source_id: request.lease.input.source_id.clone(),
            representation: request.page.representation,
            segment_index: 0,
            byte_offset: 3,
            query_hash: "b".repeat(64),
        }
    }

    fn page(request: &PostProcessTranscriptRequest) -> DesktopTranscriptPage {
        DesktopTranscriptPage {
            library_scope: "a".repeat(32),
            state: DesktopTranscriptState {
                meeting_id: request.page.meeting_id.clone(),
                revision: request.page.expected_revision.clone(),
                current_source_id: request.lease.input.source_id.clone(),
                document_state: if request.lease.input.source_id.is_some() {
                    DesktopDocumentState::Present
                } else {
                    DesktopDocumentState::Absent
                },
                deletion_epoch: 0,
            },
            source_id: request.lease.input.source_id.clone(),
            representation: request.page.representation,
            items: Vec::new(),
            next: None,
        }
    }

    #[test]
    fn transcript_request_requires_live_original_worker_meeting_revision_and_source() {
        let original = request();
        assert!(original.valid());
        let mut explicit = original.clone();
        explicit.page.source_id = explicit.lease.input.source_id.clone();
        assert!(explicit.valid());
        for fault in 0..11 {
            let mut bad = original.clone();
            match fault {
                0 => bad.proof.session_id = Uuid::new_v4(),
                1 => bad.proof.library_epoch = Uuid::new_v4(),
                2 => bad.proof.session_id = Uuid::nil(),
                3 => bad.lease.expires_at = Utc::now() - Duration::seconds(1),
                4 => bad.page.meeting_id = Uuid::new_v4().to_string(),
                5 => bad.page.meeting_id = bad.page.meeting_id.to_uppercase(),
                6 => bad.page.expected_revision = "freshly-refetched".into(),
                7 => bad.page.source_id = Some("archived-source".into()),
                8 => bad.lease.job_revision = "invalid/revision".into(),
                9 => bad.page.max_bytes = 65_537,
                _ => bad.page.limit = 0,
            }
            assert!(!bad.valid(), "fault {fault}");
        }
    }

    #[test]
    fn continuation_pins_both_current_and_selected_original_source() {
        let mut request = request();
        request.page.cursor = Some(cursor(&request));
        assert!(request.valid());
        request.page.cursor.as_mut().unwrap().current_source_id = Some("other-current".into());
        assert!(!request.valid());
        request.page.cursor = Some(cursor(&request));
        request.page.cursor.as_mut().unwrap().source_id = Some("archive".into());
        assert!(!request.valid());
        request.page.cursor = None;
        request.lease.input.source_id = None;
        assert!(request.valid());
        request.page.source_id = Some("captured-source".into());
        assert!(!request.valid());
    }

    #[test]
    fn reply_pins_cover_terminal_and_absent_source_states() {
        let mut request = request();
        let original = page(&request);
        assert!(request.reply_pins_valid(&original));
        for fault in 0..6 {
            let mut bad = original.clone();
            match fault {
                0 => bad.state.meeting_id = Uuid::new_v4().to_string(),
                1 => bad.state.revision = "different-revision".into(),
                2 => bad.state.current_source_id = Some("other-current".into()),
                3 => bad.source_id = Some("archive".into()),
                4 => bad.state.document_state = DesktopDocumentState::Deleted,
                _ => bad.representation = DesktopTranscriptRepresentation::Effective,
            }
            assert!(!request.reply_pins_valid(&bad), "fault {fault}");
        }
        request.lease.input.source_id = None;
        let mut absent = page(&request);
        assert!(request.reply_pins_valid(&absent));
        absent.state.document_state = DesktopDocumentState::Deleted;
        assert!(!request.reply_pins_valid(&absent));
    }

    #[test]
    fn request_is_closed_bounded_and_redacts_both_secrets() {
        let request = request();
        let debug = format!("{request:?}");
        assert!(!debug.contains(request.proof.secret.expose()));
        assert!(!debug.contains(request.fence.expose()));
        assert!(desktop_body_fits(&request, MAX_DESKTOP_DATA_BYTES as usize));
        for key in [
            "peer_pid",
            "actor",
            "path",
            "claim_token",
            "executor",
            "output",
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[key] = serde_json::json!("forbidden selector");
            assert!(serde_json::from_value::<PostProcessTranscriptRequest>(value).is_err());
        }
    }
}

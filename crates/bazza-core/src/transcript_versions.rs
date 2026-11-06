//! Authorized desktop version reads and connection-scoped staged writes.

use crate::{desktop::authenticated_peer, CoreError, Repository};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    transcript_versions::{
        version_chunk, DesktopTranscriptVersionReply as Reply,
        DesktopTranscriptVersionRequest as Request, NewTranscriptVersion, MAX_VERSION_READ_BYTES,
    },
};
use bazza_store::service_data::DataError;
use std::time::{Duration, Instant};
use uuid::Uuid;

const TRANSFER_TTL: Duration = Duration::from_secs(60);

struct MeetingFence {
    id: String,
    lifecycle: String,
    transcript: String,
}

enum Body {
    Download {
        text: String,
        offset: usize,
    },
    Upload {
        version_id: Uuid,
        total_bytes: usize,
        text: String,
    },
}

struct Transfer {
    id: Uuid,
    meeting: MeetingFence,
    deadline: Instant,
    body: Body,
}

/// One bounded transfer for the authenticated private socket. Dropping the
/// connection never installs a partial version or leaves a staging file.
#[derive(Default)]
pub struct TranscriptVersionTransfer(Option<Transfer>);

impl MeetingFence {
    fn check(&self, repository: &Repository) -> Result<(), CoreError> {
        let meeting = repository
            .store
            .service_desktop_meeting(&self.id)?
            .ok_or(DataError::NotFound)?;
        if meeting.lifecycle_revision != self.lifecycle {
            return Err(DataError::Conflict {
                current_revision: meeting.lifecycle_revision,
            }
            .into());
        }
        if meeting.transcript_revision != self.transcript {
            return Err(DataError::Conflict {
                current_revision: meeting.transcript_revision,
            }
            .into());
        }
        Ok(())
    }
}

impl TranscriptVersionTransfer {
    fn begin(&mut self, meeting: MeetingFence, body: Body) -> Result<Uuid, DataError> {
        if self.0.is_some() {
            return Err(DataError::Busy);
        }
        let id = Uuid::new_v4();
        self.0 = Some(Transfer {
            id,
            meeting,
            deadline: Instant::now() + TRANSFER_TTL,
            body,
        });
        Ok(id)
    }

    fn active(&mut self, id: Uuid) -> Result<&mut Transfer, DataError> {
        self.0
            .as_mut()
            .filter(|transfer| transfer.id == id)
            .ok_or(DataError::Forbidden)
    }

    fn read(
        &mut self,
        repository: &Repository,
        id: Uuid,
        requested: usize,
    ) -> Result<Reply, CoreError> {
        let result = self.read_current(repository, id, requested);
        if result.is_err()
            && self.0.as_ref().is_some_and(|transfer| {
                transfer.id == id && matches!(transfer.body, Body::Download { .. })
            })
        {
            self.0.take();
        }
        result
    }

    fn read_current(
        &mut self,
        repository: &Repository,
        id: Uuid,
        requested: usize,
    ) -> Result<Reply, CoreError> {
        let transfer = self.active(id)?;
        transfer.meeting.check(repository)?;
        let Body::Download { text, offset } = &mut transfer.body else {
            return Err(DataError::Forbidden.into());
        };
        if requested != *offset {
            return Err(DataError::Invalid("transcript version offset").into());
        }
        let chunk = version_chunk(text, *offset)
            .ok_or(DataError::Invalid("transcript version chunk"))?
            .to_owned();
        let total_bytes = text.len();
        *offset += chunk.len();
        if *offset == total_bytes {
            self.0.take();
        }
        Ok(Reply::Chunk {
            transfer_id: id,
            offset: requested,
            total_bytes,
            text: chunk,
        })
    }
}

impl Repository {
    pub fn desktop_transcript_version(
        &self,
        peer_pid: u32,
        request: Request,
        transfers: &mut TranscriptVersionTransfer,
    ) -> Result<DesktopEnvelope<Reply>, CoreError> {
        authenticated_peer(peer_pid)?;
        if !request.valid() {
            return Err(DataError::Invalid("transcript version request").into());
        }
        if transfers
            .0
            .as_ref()
            .is_some_and(|transfer| Instant::now() >= transfer.deadline)
        {
            transfers.0.take();
        }
        let reply = match request {
            Request::List { meeting_id } => Reply::Listed {
                versions: self.store.service_transcript_versions(&meeting_id)?,
            },
            Request::Read {
                meeting_id,
                version_id,
            } => {
                let meeting = self
                    .store
                    .service_desktop_meeting(&meeting_id)?
                    .ok_or(DataError::NotFound)?;
                let Some(segments) = self
                    .store
                    .service_transcript_version(&meeting_id, &version_id)?
                else {
                    return self.desktop_envelope(Reply::Missing);
                };
                let text = serde_json::to_string(&segments)?;
                if text.len() > MAX_VERSION_READ_BYTES {
                    return Err(DataError::Invalid("transcript version transfer budget").into());
                }
                let fence = MeetingFence {
                    id: meeting_id,
                    lifecycle: meeting.lifecycle_revision,
                    transcript: meeting.transcript_revision,
                };
                fence.check(self)?;
                let id = transfers.begin(fence, Body::Download { text, offset: 0 })?;
                transfers.read(self, id, 0)?
            }
            Request::ReadNext {
                transfer_id,
                offset,
            } => transfers.read(self, transfer_id, offset)?,
            Request::Begin {
                meeting_id,
                expected_lifecycle_revision,
                expected_transcript_revision,
                version_id,
                total_bytes,
            } => {
                let fence = MeetingFence {
                    id: meeting_id,
                    lifecycle: expected_lifecycle_revision,
                    transcript: expected_transcript_revision,
                };
                fence.check(self)?;
                let transfer_id = transfers.begin(
                    fence,
                    Body::Upload {
                        version_id,
                        total_bytes,
                        text: String::new(),
                    },
                )?;
                Reply::Staged {
                    transfer_id,
                    received_bytes: 0,
                }
            }
            Request::Append {
                transfer_id,
                offset,
                text: chunk,
            } => {
                let transfer = transfers.active(transfer_id)?;
                let Body::Upload {
                    total_bytes, text, ..
                } = &mut transfer.body
                else {
                    return Err(DataError::Forbidden.into());
                };
                if offset != text.len() || offset + chunk.len() > *total_bytes {
                    return Err(DataError::Invalid("transcript version offset").into());
                }
                text.push_str(&chunk);
                Reply::Staged {
                    transfer_id,
                    received_bytes: text.len(),
                }
            }
            Request::Commit { transfer_id } => {
                let transfer = transfers.active(transfer_id)?;
                if !matches!(&transfer.body, Body::Upload { total_bytes, text, .. } if *total_bytes == text.len())
                {
                    return Err(DataError::Invalid("incomplete transcript version").into());
                }
                let Transfer { meeting, body, .. } =
                    transfers.0.take().ok_or(DataError::Forbidden)?;
                let Body::Upload {
                    version_id, text, ..
                } = body
                else {
                    return Err(DataError::Forbidden.into());
                };
                let input = NewTranscriptVersion::parse(text.as_bytes())
                    .map_err(|_| DataError::Invalid("transcript version body"))?;
                if input.id != version_id.to_string() {
                    return Err(DataError::Invalid("transcript version identity").into());
                }
                let version = self.store.service_save_transcript_version(
                    &meeting.id,
                    &meeting.lifecycle,
                    &meeting.transcript,
                    &input,
                )?;
                Reply::Saved { version }
            }
        };
        self.desktop_envelope(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MigrationPolicy;
    use bazza_service_api::desktop::DesktopMeetingLookup;
    use bazza_service_test_support::{create_v8, READY_MEETING_ID};

    #[test]
    fn desktop_version_upload_is_connection_owned_and_preserves_original() {
        let root = tempfile::tempdir().unwrap();
        let fixture = create_v8(&root.path().join("library")).unwrap();
        let repository = Repository::open_fixture(&fixture.root, MigrationPolicy::Upgrade).unwrap();
        let mut transfer = TranscriptVersionTransfer::default();
        let list = || Request::List {
            meeting_id: READY_MEETING_ID.into(),
        };
        assert!(matches!(
            repository.desktop_transcript_version(0, list(), &mut transfer),
            Err(CoreError::Data(DataError::Forbidden))
        ));
        let original = repository
            .store
            .service_transcript_version(READY_MEETING_ID, "original")
            .unwrap();
        let row = repository
            .desktop_meeting(
                42,
                DesktopMeetingLookup {
                    meeting_id: READY_MEETING_ID.into(),
                },
            )
            .unwrap()
            .data
            .unwrap();
        let version_id = Uuid::new_v4();
        let text = " 🎙️ Версия ".repeat(1_000);
        let body = serde_json::json!({
            "id": version_id, "source": "meet", "model": "whisper-small", "language": "ru",
            "segments": [{"start_ms":10,"end_ms":20,"speaker":"Марина","text":text,
                          "playback":{"start_ms":70,"end_ms":80}}],
        })
        .to_string();
        let begin = Request::Begin {
            meeting_id: READY_MEETING_ID.into(),
            expected_lifecycle_revision: row.lifecycle_revision,
            expected_transcript_revision: row.transcript_revision,
            version_id,
            total_bytes: body.len(),
        };
        let Reply::Staged {
            transfer_id,
            received_bytes: 0,
        } = repository
            .desktop_transcript_version(42, begin, &mut transfer)
            .unwrap()
            .data
        else {
            panic!("missing upload")
        };
        let commit = || Request::Commit { transfer_id };
        assert!(matches!(
            repository.desktop_transcript_version(
                42,
                commit(),
                &mut TranscriptVersionTransfer::default()
            ),
            Err(CoreError::Data(DataError::Forbidden))
        ));
        assert!(matches!(
            repository.desktop_transcript_version(42, commit(), &mut transfer),
            Err(CoreError::Data(DataError::Invalid(_)))
        ));
        let mut offset = 0;
        while let Some(chunk) = version_chunk(&body, offset) {
            repository
                .desktop_transcript_version(
                    42,
                    Request::Append {
                        transfer_id,
                        offset,
                        text: chunk.into(),
                    },
                    &mut transfer,
                )
                .unwrap();
            offset += chunk.len();
        }
        assert!(
            matches!(repository.desktop_transcript_version(42, commit(), &mut transfer).unwrap().data, Reply::Saved { version } if version.id == version_id.to_string())
        );
        assert_eq!(
            repository
                .store
                .service_transcript_version(READY_MEETING_ID, "original")
                .unwrap(),
            original
        );
        let mut reply = repository
            .desktop_transcript_version(
                42,
                Request::Read {
                    meeting_id: READY_MEETING_ID.into(),
                    version_id: version_id.to_string(),
                },
                &mut transfer,
            )
            .unwrap()
            .data;
        let mut downloaded = String::new();
        loop {
            let Reply::Chunk {
                transfer_id,
                offset,
                total_bytes,
                text,
            } = reply
            else {
                panic!("missing version chunk")
            };
            assert_eq!(offset, downloaded.len());
            downloaded.push_str(&text);
            if downloaded.len() == total_bytes {
                break;
            }
            reply = repository
                .desktop_transcript_version(
                    42,
                    Request::ReadNext {
                        transfer_id,
                        offset: downloaded.len(),
                    },
                    &mut transfer,
                )
                .unwrap()
                .data;
        }
        let rows: Vec<bazza_service_api::transcript_versions::TranscriptVersionSegment> =
            serde_json::from_str(&downloaded).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].segment.id < 0);
        assert_eq!(rows[0].segment.speaker.as_deref(), Some("Марина"));
        assert_eq!(rows[0].segment.text, text);
        assert_eq!(rows[0].playback.unwrap().start_ms, 70);
    }
}

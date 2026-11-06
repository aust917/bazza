//! Bridge the normal meeting IPC shape to bounded service reads.
use super::{MeetingDetail, ServicePlayback};
use crate::commands::CmdResult;
use crate::ui_message::{app, UiMessage};
use bazza_domain::{Meeting, MeetingStatus, Segment};
use bazza_service_client::{
    api::{
        content::{DesktopTranscriptFragment, DesktopTranscriptRepresentation},
        desktop::{
            DeleteDesktopMeetingRequest, DesktopMeeting, DesktopMeetingFilter,
            RenameDesktopMeetingRequest, MAX_DESKTOP_DATA_BYTES, MAX_DESKTOP_ITEMS,
        },
        rpc::ErrorCode,
    },
    repository::{RepositoryIdentity, RepositoryTranscriptRequest},
    ClientError, RepositoryClient,
};
use serde::Serialize;
use std::collections::HashSet;

// The legacy UI asks for one array. This keeps each service request bounded
// and prevents this compatibility bridge from accumulating an unlimited list.
const MAX_MEETING_PAGES: usize = 128;
const MAX_TRANSCRIPT_PAGES: usize = 128;

#[derive(Debug, Serialize)]
pub struct MeetingCommandRow {
    #[serde(flatten)]
    meeting: Meeting,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lifecycle_revision: Option<String>,
}

impl MeetingCommandRow {
    pub(in crate::commands) fn legacy(meeting: Meeting) -> Self {
        Self {
            meeting,
            metadata_revision: None,
            lifecycle_revision: None,
        }
    }

    fn from_service(row: DesktopMeeting) -> CmdResult<Self> {
        // A truncated diagnostic cannot be returned as the exact legacy field.
        let status = match (row.error_truncated, row.status.as_str()) {
            (false, "recording") => MeetingStatus::Recording,
            (false, "processing") => MeetingStatus::Processing,
            (false, "ready") => MeetingStatus::Ready,
            (false, "failed") => MeetingStatus::Failed,
            _ => {
                return Err(app(
                    "native.serviceMeetingProjectionUnavailable",
                    "The service could not provide a complete meeting row.",
                ));
            }
        };
        Ok(Self {
            meeting: Meeting {
                id: row.id,
                title: row.title,
                app: row.app,
                started_at: row.started_at,
                ended_at: row.ended_at,
                duration_ms: row.duration_ms,
                has_video: row.has_video,
                status,
                folder_id: row.folder_id,
                created_at: row.created_at,
                error: row.error,
                title_auto: row.title_auto,
                participants: row.participants,
                calendar_uid: row.calendar_uid,
            },
            metadata_revision: Some(row.metadata_revision),
            lifecycle_revision: Some(row.lifecycle_revision),
        })
    }
}

fn refresh_required() -> UiMessage {
    app(
        "native.serviceMeetingRefreshRequired",
        "Refresh the meeting list before continuing.",
    )
}

fn service_error(error: ClientError) -> UiMessage {
    if let ClientError::Remote(error) = error {
        match error.code {
            ErrorCode::Conflict | ErrorCode::ResyncRequired => return refresh_required(),
            ErrorCode::NotFound => return app("native.meetingNotFound", "meeting not found"),
            ErrorCode::ResourceBusy => {
                return app(
                    "native.serviceMeetingBusy",
                    "The meeting is still recording or processing.",
                );
            }
            ErrorCode::InvalidArgument | ErrorCode::IdempotencyConflict => {
                return app(
                    "native.serviceMeetingRequestInvalid",
                    "The service rejected this meeting request.",
                );
            }
            _ => {}
        }
    }
    // IPC errors can contain local paths. Keep them out of the presentation DTO.
    app(
        "native.serviceMeetingUnavailable",
        "The meeting service is unavailable.",
    )
}

pub(super) async fn detail(
    repository: &RepositoryClient,
    playback: &ServicePlayback,
    meeting_id: String,
) -> CmdResult<MeetingDetail> {
    let header = repository
        .meeting(meeting_id.clone())
        .await
        .map_err(service_error)?;
    let row = header
        .data
        .ok_or_else(|| app("native.meetingNotFound", "meeting not found"))?;
    let decorations = repository
        .meeting_decorations(meeting_id.clone())
        .await
        .map_err(service_error)?;
    if decorations.identity != header.identity {
        return Err(refresh_required());
    }
    let segments = if row.transcript_state == "present" {
        let raw = transcript(
            repository,
            &header.identity,
            &row,
            DesktopTranscriptRepresentation::Raw,
        )
        .await?;
        let effective = if raw.iter().any(|fragment| fragment.has_clean_text) {
            Some(
                transcript(
                    repository,
                    &header.identity,
                    &row,
                    DesktopTranscriptRepresentation::Effective,
                )
                .await?,
            )
        } else {
            None
        };
        merge_transcript(&meeting_id, raw, effective)?
    } else {
        Vec::new()
    };

    let files = if row.status != "recording" && (row.media.audio || row.media.video) {
        let root = repository.root().to_path_buf();
        let id = meeting_id.clone();
        let available = row.media.clone();
        crate::commands::blocking(move || {
            Ok(super::meeting_playback::PlaybackFiles::open(
                &root,
                &id,
                available.audio,
                available.video,
            ))
        })
        .await?
    } else {
        None
    };
    // The physical gate blocks purge after opening. Recheck the visible
    // meeting and revisions to close the race with an earlier tombstone or
    // transcript publication before returning either a snapshot or token.
    let latest = repository
        .meeting(meeting_id)
        .await
        .map_err(service_error)?;
    let latest_row = latest
        .data
        .ok_or_else(|| app("native.meetingNotFound", "meeting not found"))?;
    if latest.identity != header.identity
        || latest_row.metadata_revision != row.metadata_revision
        || latest_row.lifecycle_revision != row.lifecycle_revision
        || latest_row.transcript_revision != row.transcript_revision
        || latest_row.media.audio != row.media.audio
        || latest_row.media.video != row.media.video
    {
        return Err(refresh_required());
    }
    let meeting = MeetingCommandRow::from_service(latest_row)?;
    let media = files
        .and_then(|files| playback.register(files))
        .unwrap_or_default();
    let decorations = decorations.data;
    Ok(MeetingDetail {
        meeting,
        segments,
        media,
        highlights: decorations.highlights,
        speakers: decorations.speakers,
        speaker_stats: decorations.speaker_stats,
    })
}

async fn transcript(
    repository: &RepositoryClient,
    identity: &RepositoryIdentity,
    meeting: &DesktopMeeting,
    representation: DesktopTranscriptRepresentation,
) -> CmdResult<Vec<DesktopTranscriptFragment>> {
    let mut request = RepositoryTranscriptRequest::new(
        meeting.id.clone(),
        meeting.transcript_revision.clone(),
        representation,
    );
    request.limit = MAX_DESKTOP_ITEMS;
    let mut rows = Vec::new();
    let mut pending = None;
    let mut seen = HashSet::new();
    for _ in 0..MAX_TRANSCRIPT_PAGES {
        let page = repository
            .transcript(request.clone())
            .await
            .map_err(service_error)?;
        if page.identity != *identity {
            return Err(refresh_required());
        }
        for fragment in page.items {
            append_fragment(&mut rows, &mut pending, &mut seen, fragment)?;
        }
        request.cursor = page.next;
        if request.cursor.is_none() {
            return if pending.is_none() {
                Ok(rows)
            } else {
                Err(refresh_required())
            };
        }
    }
    Err(app(
        "native.serviceMeetingProjectionUnavailable",
        "The transcript exceeds this window's supported size.",
    ))
}

fn same_row(left: &DesktopTranscriptFragment, right: &DesktopTranscriptFragment) -> bool {
    left.segment_index == right.segment_index
        && left.segment_id == right.segment_id
        && left.start_ms == right.start_ms
        && left.end_ms == right.end_ms
        && left.speaker == right.speaker
        && left.has_clean_text == right.has_clean_text
        && left.clean_text_manual == right.clean_text_manual
        && left.effective_origin == right.effective_origin
}

fn append_fragment(
    rows: &mut Vec<DesktopTranscriptFragment>,
    pending: &mut Option<DesktopTranscriptFragment>,
    seen: &mut HashSet<i64>,
    fragment: DesktopTranscriptFragment,
) -> CmdResult<()> {
    if fragment.segment_index != rows.len() as u64 {
        return Err(refresh_required());
    }
    if let Some(current) = pending {
        if !same_row(current, &fragment)
            || current.total_bytes != fragment.total_bytes
            || current.text.len() as u64 != fragment.byte_offset
        {
            return Err(refresh_required());
        }
        current.text.push_str(&fragment.text);
    } else if fragment.byte_offset == 0 && seen.insert(fragment.segment_id) {
        *pending = Some(fragment);
    } else {
        return Err(refresh_required());
    }
    let current = pending.as_ref().expect("accepted fragment owns a row");
    if current.text.len() as u64 == current.total_bytes {
        rows.push(pending.take().expect("complete transcript row"));
    }
    Ok(())
}

fn merge_transcript(
    id: &str,
    raw: Vec<DesktopTranscriptFragment>,
    effective: Option<Vec<DesktopTranscriptFragment>>,
) -> CmdResult<Vec<Segment>> {
    let mut effective = effective.map(Vec::into_iter);
    let mut segments = Vec::with_capacity(raw.len());
    for row in raw {
        let clean_text = if let Some(values) = &mut effective {
            let value = values
                .next()
                .filter(|value| same_row(&row, value))
                .ok_or_else(refresh_required)?;
            if !row.has_clean_text && value.text != row.text {
                return Err(refresh_required());
            }
            row.has_clean_text.then_some(value.text)
        } else if row.has_clean_text {
            return Err(refresh_required());
        } else {
            None
        };
        segments.push(Segment {
            id: row.segment_id,
            meeting_id: id.to_owned(),
            start_ms: row.start_ms,
            end_ms: row.end_ms,
            speaker: row.speaker,
            text: row.text,
            clean_text,
            clean_text_manual: row.clean_text_manual,
        });
    }
    if effective.is_some_and(|mut values| values.next().is_some()) {
        return Err(refresh_required());
    }
    // Independent speaker decoders can confirm an earlier line after a later one.
    segments.sort_by_key(|segment| (segment.start_ms, segment.id));
    Ok(segments)
}

pub(super) async fn list(repository: &RepositoryClient) -> CmdResult<Vec<MeetingCommandRow>> {
    list_filtered(repository, DesktopMeetingFilter::default()).await
}

pub(super) async fn list_filtered(
    repository: &RepositoryClient,
    filter: DesktopMeetingFilter,
) -> CmdResult<Vec<MeetingCommandRow>> {
    let identity = repository.identity().await;
    let mut sequence = None;
    let mut continuation = None;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for _ in 0..MAX_MEETING_PAGES {
        let page = repository
            .meetings(
                filter.clone(),
                continuation.take(),
                MAX_DESKTOP_ITEMS,
                MAX_DESKTOP_DATA_BYTES,
            )
            .await
            .map_err(service_error)?;
        if page.identity != identity
            || sequence.is_some_and(|value| value != page.snapshot_sequence)
        {
            return Err(refresh_required());
        }
        sequence = Some(page.snapshot_sequence);
        for row in page.items {
            if !seen.insert(row.id.clone()) {
                return Err(refresh_required());
            }
            rows.push(MeetingCommandRow::from_service(row)?);
        }
        continuation = page.next;
        if continuation.is_none() {
            return Ok(rows);
        }
    }
    Err(app(
        "native.serviceMeetingListTooLarge",
        "The meeting list exceeds this window's supported size.",
    ))
}

pub(super) async fn rename(
    repository: &RepositoryClient,
    meeting_id: String,
    title: String,
    expected_revision: Option<String>,
    idempotency_key: Option<String>,
) -> CmdResult<MeetingCommandRow> {
    let (Some(expected_revision), Some(idempotency_key)) = (expected_revision, idempotency_key)
    else {
        return Err(refresh_required());
    };
    let title = title.trim().to_owned();
    if title.is_empty() {
        return Err(app("native.titleEmpty", "title cannot be empty"));
    }
    let receipt = repository
        .rename_meeting(RenameDesktopMeetingRequest {
            meeting_id: meeting_id.clone(),
            expected_revision,
            idempotency_key,
            title,
        })
        .await
        .map_err(service_error)?;
    let current = repository
        .meeting(meeting_id)
        .await
        .map_err(service_error)?;
    if current.identity != receipt.identity {
        return Err(refresh_required());
    }
    let row = current
        .data
        .filter(|row| row.metadata_revision == receipt.data.revision)
        .ok_or_else(refresh_required)?;
    MeetingCommandRow::from_service(row)
}

pub(super) async fn delete(
    repository: &RepositoryClient,
    meeting_id: String,
    expected_lifecycle_revision: Option<String>,
    idempotency_key: Option<String>,
) -> CmdResult<()> {
    let (Some(expected_lifecycle_revision), Some(idempotency_key)) =
        (expected_lifecycle_revision, idempotency_key)
    else {
        return Err(refresh_required());
    };
    repository
        .delete_meeting(DeleteDesktopMeetingRequest {
            meeting_id,
            expected_lifecycle_revision,
            idempotency_key,
        })
        .await
        .map_err(service_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header() -> DesktopMeeting {
        serde_json::from_value(json!({
            "id": "opaque-legacy-id",
            "title": "Встреча / Review",
            "app": "Zoom",
            "started_at": "2026-11-03T10:20:30Z",
            "ended_at": "2026-11-03T10:21:30Z",
            "duration_ms": 60_000,
            "has_video": true,
            "status": "ready",
            "folder_id": "folder-1",
            "created_at": "2026-11-03T10:20:29Z",
            "error": "Exact optional diagnostic",
            "error_truncated": false,
            "title_auto": false,
            "participants": ["Аня", "Bill"],
            "calendar_uid": "external-calendar-id",
            "content_kind": "recorded",
            "metadata_revision": "metadata-current",
            "lifecycle_revision": "lifecycle-current",
            "transcript_revision": "transcript-current",
            "transcript_state": "present",
            "summary_revision": "summary-current",
            "summary_state": "absent",
            "media": {"audio": true, "video": false}
        }))
        .unwrap()
    }

    #[test]
    fn list_row_preserves_normal_meeting_fields_and_attaches_observed_revisions() {
        let original = header();
        let row = MeetingCommandRow::from_service(original.clone()).unwrap();
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(value["metadata_revision"], original.metadata_revision);
        assert_eq!(value["lifecycle_revision"], original.lifecycle_revision);
        assert_eq!(value["title"], original.title);
        assert_eq!(value["error"], original.error.unwrap());
        assert_eq!(value["participants"], json!(original.participants));
        assert_eq!(value["calendar_uid"], original.calendar_uid.unwrap());
        assert_eq!(value["status"], "ready");
        assert_eq!(value["has_video"], true);
        assert_eq!(value["duration_ms"], 60_000);
        assert!(value.get("media").is_none());

        let legacy = serde_json::to_value(MeetingCommandRow::legacy(row.meeting)).unwrap();
        let mut service = value.as_object().unwrap().clone();
        service.remove("metadata_revision");
        service.remove("lifecycle_revision");
        assert_eq!(legacy, serde_json::Value::Object(service));
    }

    #[test]
    fn list_row_rejects_incomplete_diagnostics_and_unknown_status() {
        let mut original = header();
        original.error_truncated = true;
        assert!(MeetingCommandRow::from_service(original).is_err());
        let mut original = header();
        original.status = "unsupported".into();
        assert!(MeetingCommandRow::from_service(original).is_err());
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires an already running service on a fresh BAZZA_TEST_SERVICE_FIXTURE"]
    async fn lists_renames_and_deletes_through_running_fixture_service() {
        use bazza_service_client::validate_fixture_root;
        use std::path::PathBuf;
        use uuid::Uuid;

        const DAMAGED_MEETING_ID: &str = "00000000-0000-4000-8000-000000000002";

        let root = PathBuf::from(
            std::env::var_os("BAZZA_TEST_SERVICE_FIXTURE")
                .expect("set BAZZA_TEST_SERVICE_FIXTURE to an absolute marked disposable root"),
        );
        let root = validate_fixture_root(&root).expect("invalid disposable service fixture");
        let repository = RepositoryClient::connect(&root).await.unwrap();
        let before = list(&repository).await.unwrap();
        let original = before
            .iter()
            .find(|row| row.meeting.id == DAMAGED_MEETING_ID)
            .expect("use a fresh generated fixture");
        assert_eq!(
            original.meeting.title,
            "Повреждённая запись / Missing media"
        );
        assert_eq!(original.meeting.status, MeetingStatus::Failed);
        assert_eq!(
            original.meeting.error.as_deref(),
            Some("synthetic missing mix.wav")
        );
        let metadata = original.metadata_revision.clone();
        assert!(metadata.is_some());
        assert!(original.lifecycle_revision.is_some());

        let renamed = rename(
            &repository,
            DAMAGED_MEETING_ID.into(),
            "  Переименовано через службу  ".into(),
            metadata.clone(),
            Some(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap();
        let mut expected = original.meeting.clone();
        expected.title = "Переименовано через службу".into();
        expected.title_auto = false;
        assert_eq!(renamed.meeting, expected);
        assert_ne!(renamed.metadata_revision, metadata);
        assert_eq!(renamed.lifecycle_revision, original.lifecycle_revision);

        let stale = rename(
            &repository,
            DAMAGED_MEETING_ID.into(),
            "Stale rename".into(),
            metadata,
            Some(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.code, "native.serviceMeetingRefreshRequired");

        let after_rename = list(&repository).await.unwrap();
        let current = after_rename
            .iter()
            .find(|row| row.meeting.id == DAMAGED_MEETING_ID)
            .unwrap();
        assert_eq!(current.meeting, expected);
        assert_eq!(current.metadata_revision, renamed.metadata_revision);
        let key = Some(Uuid::new_v4().to_string());
        for _ in 0..2 {
            delete(
                &repository,
                DAMAGED_MEETING_ID.into(),
                current.lifecycle_revision.clone(),
                key.clone(),
            )
            .await
            .unwrap();
        }
        let after_delete = list(&repository).await.unwrap();
        assert_eq!(after_delete.len(), before.len() - 1);
        assert!(after_delete
            .iter()
            .all(|row| row.meeting.id != DAMAGED_MEETING_ID));
    }

    #[test]
    fn transcript_joins_utf8_fragments_and_preserves_an_empty_manual_overlay() {
        use bazza_service_client::api::content::DesktopEffectiveOrigin;
        let original = DesktopTranscriptFragment {
            segment_index: 0,
            segment_id: 7,
            start_ms: 1,
            end_ms: 3,
            speaker: Some("Аня".into()),
            text: "🔔".into(),
            byte_offset: 0,
            total_bytes: "🔔 да".len() as u64,
            has_clean_text: true,
            clean_text_manual: true,
            effective_origin: DesktopEffectiveOrigin::Manual,
        };
        let mut rows = Vec::new();
        let mut pending = None;
        let mut seen = HashSet::new();
        append_fragment(&mut rows, &mut pending, &mut seen, original.clone()).unwrap();
        assert!(rows.is_empty());
        append_fragment(
            &mut rows,
            &mut pending,
            &mut seen,
            DesktopTranscriptFragment {
                text: " да".into(),
                byte_offset: "🔔".len() as u64,
                ..original.clone()
            },
        )
        .unwrap();
        let effective = vec![DesktopTranscriptFragment {
            text: String::new(),
            total_bytes: 0,
            ..original
        }];
        let merged = merge_transcript("meeting", rows, Some(effective)).unwrap();
        assert_eq!(merged[0].text, "🔔 да");
        assert_eq!(merged[0].clean_text.as_deref(), Some(""));
        assert_eq!(merged[0].speaker.as_deref(), Some("Аня"));
        assert!(merged[0].clean_text_manual);
    }
}

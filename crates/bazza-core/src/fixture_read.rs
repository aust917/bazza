//! Public DTO projection for the synthetic-only service. Fixture authority is
//! inherited from the only Repository constructor, never from tool arguments.
use crate::Repository;
use bazza_service_api::{
    fixture_read::{FixtureReadReply, FixtureReadRequest, FixtureReadTool},
    inputs::{GetMeetingInput, ListMeetingsInput, SearchInput},
    outputs::*,
    types::*,
};
use bazza_store::service_data::{
    DataError, FixtureMeeting, FixtureMeetingFilter, FixtureSearchHit, SearchCursor,
    SearchRepresentation, SearchRequest,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[cfg(test)]
#[path = "fixture_read_tests.rs"]
mod tests;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FixtureReadError {
    #[error("invalid bounded public input")]
    Invalid,
    #[error("cursor belongs to another request or snapshot")]
    Stale,
    #[error("data cannot be represented in a bounded public page")]
    Unavailable,
    #[error("fixture data error")]
    Data(#[from] DataError),
    #[error("fixture encoding error")]
    Serde(#[from] serde_json::Error),
}

impl FixtureReadError {
    fn code(&self) -> ErrorCode {
        match self {
            Self::Invalid
            | Self::Data(DataError::Invalid(
                "search token count" | "search limits" | "search fields",
            )) => ErrorCode::InvalidArgument,
            Self::Stale | Self::Data(DataError::StaleCursor | DataError::Conflict { .. }) => {
                ErrorCode::CursorStale
            }
            Self::Data(DataError::NotFound) => ErrorCode::NotFound,
            Self::Data(DataError::IndexUnavailable) => ErrorCode::SearchUnavailable,
            Self::Data(DataError::Busy) => ErrorCode::ResourceBusy,
            _ => ErrorCode::BackendUnavailable,
        }
    }
}

type Result<T> = std::result::Result<T, FixtureReadError>;

fn as_enum<T: DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_value(Value::String(value)).map_err(|_| FixtureReadError::Unavailable)
}

fn millis(value: i64) -> Result<Millis> {
    u64::try_from(value)
        .map(Millis)
        .map_err(|_| FixtureReadError::Unavailable)
}

fn filter(input: Option<MeetingFilter>) -> Result<FixtureMeetingFilter> {
    let f = input.unwrap_or_default();
    Ok(FixtureMeetingFilter {
        from: f
            .from
            .map(|value| value.0.parse())
            .transpose()
            .map_err(|_| FixtureReadError::Invalid)?,
        to: f
            .to
            .map(|value| value.0.parse())
            .transpose()
            .map_err(|_| FixtureReadError::Invalid)?,
        meeting_ids: f
            .meeting_ids
            .map(|values| values.into_iter().map(|value| value.0).collect()),
        folder_ids: f
            .folder_ids
            .map(|values| values.into_iter().map(|value| value.0).collect()),
        tag_ids: f
            .tag_ids
            .map(|values| values.into_iter().map(|value| value.0).collect()),
        participants: f
            .participants
            .map(|values| values.into_iter().map(|value| value.0).collect()),
        content_kind: f.content_kind.map(|kind| match kind {
            ContentKind::Recorded => "recorded",
            ContentKind::Text => "text",
        }),
    })
}

fn meeting(snapshot: FixtureMeeting) -> Result<MeetingView> {
    let FixtureMeeting {
        item,
        tag_ids,
        current_source_id,
    } = snapshot;
    let content_kind = as_enum(item.content_kind)?;
    let media_state = |expected, available| match (expected, available) {
        (false, _) => MediaState::None,
        (true, true) => MediaState::Available,
        (true, false) => MediaState::Missing,
    };
    Ok(MeetingView {
        meeting_id: EntityId(item.id),
        title: Text(item.title),
        started_at: Timestamp(item.started_at.to_rfc3339()),
        ended_at: item.ended_at.map(|time| Timestamp(time.to_rfc3339())),
        duration_ms: millis(item.duration_ms)?,
        content_kind,
        processing_state: as_enum(item.status)?,
        participants: item.participants.into_iter().map(Text).collect(),
        folder_id: item.folder_id.map(EntityId),
        tag_ids: tag_ids.into_iter().map(EntityId).collect(),
        calendar_uid: item.calendar_uid.map(Text),
        metadata_revision: Revision(item.metadata_revision),
        lifecycle_revision: Revision(item.lifecycle_revision),
        transcript: ContentState {
            state: as_enum(item.transcript_state)?,
            revision: Revision(item.transcript_revision),
            source_id: current_source_id.map(EntityId),
        },
        summary: ContentState {
            state: as_enum(item.summary_state)?,
            revision: Revision(item.summary_revision),
            source_id: None,
        },
        media: MediaAvailability {
            audio: media_state(content_kind == ContentKind::Recorded, item.media.audio),
            video: media_state(
                content_kind == ContentKind::Recorded && item.has_video,
                item.media.video,
            ),
        },
    })
}

pub(super) fn cursor_in(
    epoch: Uuid,
    kind: &str,
    token: Option<Cursor>,
) -> Result<Option<SearchCursor>> {
    let Some(Cursor(token)) = token else {
        return Ok(None);
    };
    let mut parts = token.split('.');
    let (
        Some("v1"),
        Some(tool),
        Some(service_epoch),
        Some(scope),
        Some(generation),
        Some(offset),
        Some(query_hash),
        None,
    ) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    )
    else {
        return Err(FixtureReadError::Stale);
    };
    if tool != kind
        || service_epoch != epoch.to_string()
        || scope.len() != 32
        || query_hash.len() != 64
        || !scope
            .bytes()
            .chain(query_hash.bytes())
            .all(|c| c.is_ascii_hexdigit())
    {
        return Err(FixtureReadError::Stale);
    }
    let generation = generation
        .parse::<i64>()
        .map_err(|_| FixtureReadError::Stale)?;
    let offset = offset
        .parse::<usize>()
        .map_err(|_| FixtureReadError::Stale)?;
    if generation < 0 || !(1..=1_000_000).contains(&offset) {
        return Err(FixtureReadError::Stale);
    }
    Ok(Some(SearchCursor {
        library_scope: scope.to_owned(),
        generation,
        query_hash: query_hash.to_owned(),
        offset,
    }))
}

pub(super) fn page(epoch: Uuid, kind: &str, cursor: Option<SearchCursor>) -> Page {
    Page {
        truncated: cursor.is_some(),
        next_cursor: cursor.map(|c| {
            Cursor(format!(
                "v1.{kind}.{epoch}.{}.{}.{}.{}",
                c.library_scope, c.generation, c.offset, c.query_hash
            ))
        }),
    }
}

// Reserve room for the envelope, diagnostics and one complete continuation.
fn bounded_items<T: Serialize>(items: &mut Vec<T>) -> Result<()> {
    let mut used = 2048;
    let mut count = 0;
    for item in items.iter() {
        used += serde_json::to_vec(item)?.len() + 1;
        if used > FixtureReadReply::MAX_ENCODED_BYTES {
            break;
        }
        count += 1;
    }
    if count == 0 && !items.is_empty() {
        return Err(FixtureReadError::Unavailable);
    }
    items.truncate(count);
    Ok(())
}

impl Repository {
    pub fn fixture_read(&self, request: FixtureReadRequest) -> FixtureReadReply {
        if !request.valid() {
            return FixtureReadReply::failure(self.library_id, ErrorCode::InvalidArgument);
        }
        match self.fixture_read_data(&request) {
            Ok(data) => {
                let reply = FixtureReadReply {
                    output: serde_json::to_value(Success {
                        schema_version: 1,
                        library_id: LibraryId(self.library_id.to_string()),
                        data,
                        warnings: Vec::new(),
                    })
                    .expect("typed success envelope serializes"),
                };
                if reply.valid_for(&request, self.library_id) {
                    reply
                } else {
                    FixtureReadReply::failure(self.library_id, ErrorCode::BackendUnavailable)
                }
            }
            Err(error) => FixtureReadReply::failure(self.library_id, error.code()),
        }
    }

    fn fixture_read_data(&self, request: &FixtureReadRequest) -> Result<Value> {
        let input = || request.arguments.clone();
        Ok(match request.tool {
            FixtureReadTool::Status => serde_json::to_value(self.fixture_status()?)?,
            FixtureReadTool::GetMeeting => {
                let input: GetMeetingInput = serde_json::from_value(input())?;
                serde_json::to_value(meeting(
                    self.store.fixture_read_meeting(&input.meeting_id.0)?,
                )?)?
            }
            FixtureReadTool::ListMeetings => {
                serde_json::to_value(self.fixture_meetings(serde_json::from_value(input())?)?)?
            }
            FixtureReadTool::Transcript => {
                serde_json::to_value(self.fixture_transcript(serde_json::from_value(input())?)?)?
            }
            FixtureReadTool::Summary => {
                serde_json::to_value(self.fixture_summary(serde_json::from_value(input())?)?)?
            }
            FixtureReadTool::ListNotes => {
                serde_json::to_value(self.fixture_notes(serde_json::from_value(input())?)?)?
            }
            FixtureReadTool::GetNote => {
                serde_json::to_value(self.fixture_note(serde_json::from_value(input())?)?)?
            }
            FixtureReadTool::Structure => {
                serde_json::to_value(self.fixture_structure(serde_json::from_value(input())?)?)?
            }
            FixtureReadTool::Search => {
                serde_json::to_value(self.fixture_search(serde_json::from_value(input())?)?)?
            }
        })
    }

    fn fixture_status(&self) -> Result<ServiceStatus> {
        let health = self.store.fixture_read_health()?;
        let status = self.status();
        Ok(ServiceStatus {
            service_version: Version(env!("CARGO_PKG_VERSION").into()),
            api_major: status.api.major,
            api_minor: status.api.minor,
            schema_version: u32::try_from(status.schema_version)
                .map_err(|_| FixtureReadError::Unavailable)?,
            library_epoch: Revision(self.epoch.to_string()),
            index: IndexHealth {
                lexical: as_enum(health.index_state)?,
                search_generation: Revision(health.search_generation.to_string()),
                semantic_coverage: SemanticCoverage::None,
            },
            jobs: JobHealth {
                queued: health.queued,
                running: health.running,
                waiting_for_worker: health.waiting_for_worker,
            },
            shares: ShareHealth {
                active_count: 0,
                listener_available: false,
            },
            available_tools: FixtureReadTool::ALL
                .map(|tool| Text(tool.name().into()))
                .into(),
        })
    }

    fn fixture_meetings(&self, input: ListMeetingsInput) -> Result<MeetingsPage> {
        let snapshot = self.store.fixture_read_meetings(
            &filter(input.filter)?,
            input.limit.0,
            cursor_in(self.epoch, "meetings", input.cursor)?,
        )?;
        let mut position = snapshot.position;
        let mut items = snapshot
            .items
            .into_iter()
            .map(meeting)
            .collect::<Result<Vec<_>>>()?;
        let original_count = items.len();
        bounded_items(&mut items)?;
        position.offset += items.len();
        let next = (snapshot.more || items.len() < original_count).then_some(position);
        Ok(MeetingsPage {
            items,
            page: page(self.epoch, "meetings", next),
        })
    }

    fn fixture_search(&self, input: SearchInput) -> Result<SearchPage> {
        let cursor = cursor_in(self.epoch, "search", input.cursor)?;
        let offset = cursor.as_ref().map_or(0, |c| c.offset);
        let request = SearchRequest {
            query: input.query.0,
            fields: input
                .fields
                .unwrap_or_default()
                .into_iter()
                .map(|field| {
                    match field {
                        SearchField::Title => "title",
                        SearchField::Transcript => "transcript",
                        SearchField::Summary => "summary",
                        SearchField::Notes => "note",
                    }
                    .into()
                })
                .collect(),
            representation: match input.representation {
                Representation::Raw => SearchRepresentation::Raw,
                Representation::Effective => SearchRepresentation::Effective,
                Representation::Both => SearchRepresentation::Both,
            },
            meeting_ids: Vec::new(),
            limit: usize::from(input.limit.0),
            cursor,
        };
        let result = self
            .store
            .fixture_read_search(&request, Some(&filter(input.filter)?))?;
        let mut hits = result
            .hits
            .into_iter()
            .map(|hit| self.fixture_search_hit(hit))
            .collect::<Result<Vec<_>>>()?;
        let original_count = hits.len();
        bounded_items(&mut hits)?;
        let next = if hits.len() < original_count {
            let mut next = result.position;
            next.offset = offset + hits.len();
            Some(next)
        } else {
            result.next
        };
        Ok(SearchPage {
            diagnostics: SearchDiagnostics {
                lexical: IndexState::Ready,
                semantic_coverage: SemanticCoverage::None,
                mode: SearchMode::Lexical,
                no_matches: hits.is_empty(),
                search_generation: Revision(result.generation.to_string()),
            },
            hits,
            page: page(self.epoch, "search", next),
        })
    }

    fn fixture_search_hit(&self, entry: FixtureSearchHit) -> Result<SearchHit> {
        let hit = entry.hit;
        let kind: SourceKind = as_enum(hit.source_kind)?;
        let source_id = hit.source_id.unwrap_or_else(|| hit.entity_id.clone());
        let authority = self.library_id;
        let uri = match kind {
            SourceKind::Note => format!("bazza://{authority}/notes/{source_id}"),
            SourceKind::Title => format!("bazza://{authority}/meetings/{source_id}"),
            SourceKind::Summary => format!("bazza://{authority}/meetings/{source_id}/summary"),
            SourceKind::Transcript => format!(
                "bazza://{authority}/meetings/{}/transcripts/{source_id}/segments/{}",
                hit.meeting_id
                    .as_deref()
                    .ok_or(FixtureReadError::Unavailable)?,
                hit.segment_id.ok_or(FixtureReadError::Unavailable)?
            ),
        };
        Ok(SearchHit {
            kind,
            meeting_id: hit.meeting_id.map(EntityId),
            note_id: (kind == SourceKind::Note).then_some(EntityId(hit.entity_id)),
            source_id: EntityId(source_id),
            source_revision: Revision(hit.revision),
            representation: as_enum(hit.representation)?,
            title: Text(entry.title),
            date: entry.date.map(|date| Timestamp(date.to_rfc3339())),
            excerpt: Text(hit.snippet),
            source_uri: SourceUri(uri),
            start_ms: hit.start_ms.map(millis).transpose()?,
            end_ms: hit.end_ms.map(millis).transpose()?,
        })
    }
}

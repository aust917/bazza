use super::{emit, upload::UploadKind, Claim, TranscriptRow, WorkError, WorkResult, Worker};
use crate::{settings::Settings, state::RuntimeState};
use bazza_service_client::{
    api::{
        content::DesktopTranscriptRepresentation,
        desktop::{DesktopMeeting, DesktopMeetingLookup},
        postprocess_cleanup::PostProcessCleanupUpdate,
        postprocess_result::CommitPostProcessAutoTitleRequest,
        postprocess_summary::WorkerGeneratedSummary,
        rpc::ErrorCode,
        templates::{
            DesktopTemplatesReply, DesktopTemplatesRequest, TemplateDocumentState, TemplateField,
            TemplateHeader, TemplateReadRequest, MAX_TEMPLATE_DATA_BYTES,
        },
    },
    ClientError,
};
use std::sync::atomic::AtomicBool;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

const MAX_PROMPT_TEMPLATE_BYTES: u64 = 256 * 1024;

impl Worker {
    pub(super) async fn run_optional_stages(
        &mut self,
        app: &AppHandle,
        claim: &mut Claim,
        settings: &Settings,
        cleanup: bool,
        has_summary: bool,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        let mut rows = self
            .read_transcript(claim, DesktopTranscriptRepresentation::Effective, stopping)
            .await?;
        if !rows.is_empty() && !cleanup {
            let updates = if settings.cleanup_enabled {
                emit(app, claim.lease.meeting_id, "cleanup", 0.75, None);
                let desktop = app.clone();
                let selected = settings.clone();
                let input = std::mem::take(&mut rows);
                self.compute(claim, stopping, move || {
                    cleanup_transcript(&desktop, &selected, input)
                })
                .await?
            } else {
                // An explicit empty update advances the cleanup checkpoint without
                // loading a model or writing any cleaned text.
                Vec::new()
            };
            self.upload(UploadKind::Cleanup, claim, &updates, stopping)
                .await?;
            if settings.cleanup_enabled {
                rows = self
                    .read_transcript(claim, DesktopTranscriptRepresentation::Effective, stopping)
                    .await?;
            }
        }
        if settings.summary_enabled && !has_summary && !rows.is_empty() {
            self.generate_summary(app, claim, settings, rows, stopping)
                .await?;
        }
        if settings.webhook_enabled {
            return Err(WorkError::Deferred(
                "webhook delivery is outside this worker's scope",
            ));
        }
        let runtime = app.state::<RuntimeState>();
        let has_embedding_model = runtime
            .catalog
            .get(crate::state::EMBED_MODEL_ID)
            .is_some_and(|model| bazza_models::is_present(&runtime.models_dir(), model));
        if has_embedding_model {
            return Err(WorkError::Deferred(
                "embedding publication is outside this worker's scope",
            ));
        }
        self.finish(app, claim, stopping).await
    }

    async fn generate_summary(
        &mut self,
        app: &AppHandle,
        claim: &mut Claim,
        settings: &Settings,
        rows: Vec<TranscriptRow>,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        let meeting = self
            .client
            .desktop_meeting(DesktopMeetingLookup {
                meeting_id: claim.lease.meeting_id.to_string(),
            })
            .await?
            .data
            .ok_or(WorkError::Deferred("the meeting is no longer visible"))?;
        if meeting.transcript_revision != claim.lease.input.transcript_revision
            || meeting.lifecycle_revision != claim.lease.input.lifecycle_revision
        {
            return Err(WorkError::Deferred("the summary input changed"));
        }
        // A deleted or manually saved summary must not be replaced by an
        // automatic job. The service also checks the observed revision.
        if meeting.summary_state != "absent" {
            return Ok(());
        }
        let (template, template_body) = self.read_template(&settings.default_template_id).await?;
        let metadata_revision = meeting.metadata_revision.clone();
        emit(app, claim.lease.meeting_id, "summary", 0.9, None);
        let desktop = app.clone();
        let selected = settings.clone();
        let (title, body) = self
            .compute(claim, stopping, move || {
                summarize(&desktop, &selected, meeting, rows, template, template_body)
            })
            .await?;
        if let Some(title) = title {
            let result = self
                .client
                .commit_post_process_auto_title(
                    &self.registered,
                    CommitPostProcessAutoTitleRequest {
                        proof: self.proof.clone(),
                        lease: claim.lease.clone(),
                        fence: claim.fence.clone(),
                        operation_id: Uuid::new_v4(),
                        expected_metadata_revision: metadata_revision,
                        title,
                    },
                )
                .await;
            match result {
                Ok(_) => {}
                Err(ClientError::Remote(error))
                    if matches!(error.code, ErrorCode::Conflict | ErrorCode::PermissionDenied) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.upload(UploadKind::Summary, claim, &body, stopping)
            .await
    }

    async fn read_template(&mut self, configured_id: &str) -> WorkResult<(TemplateHeader, String)> {
        let mut header = self.template_header(configured_id).await?;
        if header.state != TemplateDocumentState::Present && configured_id != "general" {
            header = self.template_header("general").await?;
        }
        if header.state != TemplateDocumentState::Present
            || header.body_bytes > MAX_PROMPT_TEMPLATE_BYTES
        {
            return Err(WorkError::Deferred(
                "the selected summary template is unavailable or too large",
            ));
        }
        let mut request = TemplateReadRequest {
            template_id: header.id.clone(),
            expected_revision: header.revision.clone(),
            field: TemplateField::Body,
            cursor: None,
            max_bytes: MAX_TEMPLATE_DATA_BYTES as u32,
        };
        let mut body = String::new();
        loop {
            let reply = self
                .client
                .desktop_templates(DesktopTemplatesRequest::ReadField(request.clone()))
                .await?
                .data;
            let DesktopTemplatesReply::ReadField(page) = reply else {
                return Err(ClientError::Protocol.into());
            };
            if page.header != header
                || page.byte_offset != body.len() as u64
                || page.total_bytes != header.body_bytes
            {
                return Err(ClientError::Protocol.into());
            }
            body.push_str(&page.text);
            let Some(next) = page.next else {
                break;
            };
            request.cursor = Some(next);
        }
        if body.len() as u64 != header.body_bytes {
            return Err(ClientError::Protocol.into());
        }
        Ok((header, body))
    }

    async fn template_header(&mut self, id: &str) -> WorkResult<TemplateHeader> {
        match self
            .client
            .desktop_templates(DesktopTemplatesRequest::Describe {
                template_id: id.to_owned(),
            })
            .await?
            .data
        {
            DesktopTemplatesReply::Describe(header) => Ok(header),
            _ => Err(ClientError::Protocol.into()),
        }
    }
}

fn cleanup_transcript(
    app: &AppHandle,
    settings: &Settings,
    rows: Vec<TranscriptRow>,
) -> WorkResult<Vec<PostProcessCleanupUpdate>> {
    let runtime = app.state::<RuntimeState>();
    let _busy = runtime.busy_guard();
    let backend = runtime.bundled_llm(settings).ok_or(WorkError::Deferred(
        "a bundled language model is unavailable",
    ))?;
    let raw: Vec<_> = rows
        .into_iter()
        .map(|row| (row.segment_id, row.speaker, row.text))
        .collect();
    let updates = bazza_llm::cleanup::run(backend.as_ref(), &raw, |_| {}, || false)
        .map_err(|_| WorkError::Deferred("native transcript cleanup failed"))?;
    // The engine can return zero updates when every chunk failed or its output
    // was unusable. Leave this work retryable instead of recording success.
    if updates.is_empty() {
        return Err(WorkError::Deferred(
            "native cleanup returned no usable result",
        ));
    }
    Ok(updates
        .into_iter()
        .map(|(segment_id, text)| PostProcessCleanupUpdate { segment_id, text })
        .collect())
}

fn summarize(
    app: &AppHandle,
    settings: &Settings,
    meeting: DesktopMeeting,
    rows: Vec<TranscriptRow>,
    template: TemplateHeader,
    template_body: String,
) -> WorkResult<(Option<String>, WorkerGeneratedSummary)> {
    let runtime = app.state::<RuntimeState>();
    let _busy = runtime.busy_guard();
    let backend = runtime.bundled_llm(settings).ok_or(WorkError::Deferred(
        "a bundled language model is unavailable",
    ))?;
    let mut participants = meeting.participants;
    for row in &rows {
        if let Some(name) = &row.speaker {
            if !participants.contains(name) {
                participants.push(name.clone());
            }
        }
    }
    let vars = bazza_llm::summary::TemplateVars {
        title: meeting.title,
        date: meeting
            .started_at
            .with_timezone(&chrono::Local)
            .format("%b %-d, %Y")
            .to_string(),
        duration: bazza_llm::summary::fmt_stamp(meeting.duration_ms),
        participants: if participants.is_empty() {
            "unknown".into()
        } else {
            participants.join(", ")
        },
        transcript: rows
            .iter()
            .map(|row| {
                bazza_llm::summary::transcript_line(row.start_ms, row.speaker.as_deref(), &row.text)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        highlights: String::new(),
    };
    let title = if meeting.title_auto {
        bazza_llm::title::generate(backend.as_ref(), &vars.transcript, meeting.app.as_deref())
            .ok()
            .flatten()
    } else {
        None
    };
    let result = bazza_llm::summary::generate(backend.as_ref(), &template_body, &vars, 24_000)
        .map_err(|_| WorkError::Deferred("native summary generation failed"))?;
    let body = WorkerGeneratedSummary {
        expected_summary_revision: meeting.summary_revision,
        template_id: template.id,
        expected_template_revision: template.revision,
        expected_template_generation: template.generation,
        model: backend.name(),
        markdown: result.markdown,
        structured_json: serde_json::to_value(result.structured)
            .map_err(|_| WorkError::Deferred("native summary result could not be encoded"))?,
    };
    if body.markdown.trim().is_empty() || !body.valid() {
        return Err(WorkError::Deferred(
            "native summary returned no usable result",
        ));
    }
    Ok((title, body))
}

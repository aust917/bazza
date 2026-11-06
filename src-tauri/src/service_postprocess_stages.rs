use super::{emit, local_source, upload::UploadKind, Claim, WorkError, WorkResult, Worker};
use crate::{settings::Settings, state::RuntimeState};
use bazza_service_client::api::{
    content::DesktopTranscriptRepresentation,
    postprocess::{PostProcessJob, PostProcessStage},
    postprocess_complete::CompleteFixturePostProcessRequest,
    postprocess_ready::PostProcessReadyRequest,
    postprocess_source::LocalAsrOutcome,
    postprocess_speakers::{SpeakerAssignment, SpeakerResultBody, SpeakerSkipReason},
    worker::{CaptureProvider, CaptureSelection},
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

impl Worker {
    pub(super) async fn execute(
        &mut self,
        app: &AppHandle,
        root: &Path,
        job: PostProcessJob,
        claim: &mut Claim,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        let settings = app.state::<RuntimeState>().settings.read().clone();
        let soniox = job.provider.provider == CaptureProvider::Soniox;
        let retained_live = job.stage == PostProcessStage::CapturedLive;
        let was_captured = job.stage == PostProcessStage::Captured || retained_live;
        let mut stage = job.stage;
        if was_captured {
            let media = Arc::new(self.media(root, claim, &job.provider).await?);
            self.settle_video(app, &media, claim, stopping).await?;
            if !soniox && !retained_live {
                let meeting_id = job.meeting_id;
                emit(app, meeting_id, "transcribe", 0.1, None);
                let desktop = app.clone();
                let provider = job.provider.clone();
                let body = self
                    .compute(claim, stopping, move || {
                        local_source(&desktop, meeting_id, &media, &provider)
                    })
                    .await?;
                if !body.valid() {
                    return Err(WorkError::Deferred(
                        "local transcription exceeds the supported source shape",
                    ));
                }
                if body.outcome == LocalAsrOutcome::NoModel {
                    crate::recorder::notice_message(app, "info", crate::ui_message::app(
                        "native.transcriptionNoModel",
                        "Transcription skipped: no speech model downloaded. Download one in Settings, then Re-transcribe.",
                    ));
                }
                self.upload(UploadKind::Source, claim, &body, stopping)
                    .await?;
                stage = PostProcessStage::LocalAsr {
                    outcome: body.outcome,
                };
            }
        }
        let local_asr = match stage {
            PostProcessStage::LocalAsr { outcome } => Some(outcome),
            PostProcessStage::Captured if soniox => None,
            PostProcessStage::CapturedLive => Some(LocalAsrOutcome::Complete),
            _ => {
                return self
                    .resume_after_speakers(app, root, &job.provider, stage, claim, stopping)
                    .await
            }
        };
        emit(app, job.meeting_id, "diarize", 0.45, None);
        let body = self.speaker_result(app, root, &job.provider, claim, &settings, retained_live, stopping)
            .await?;
        let outcome = body.outcome();
        self.upload(UploadKind::Speakers, claim, &body, stopping)
            .await?;
        stage = match local_asr {
            Some(local_asr) => PostProcessStage::Speakers { local_asr, outcome },
            None => PostProcessStage::SonioxSpeakers { outcome },
        };
        self.resume_after_speakers(app, root, &job.provider, stage, claim, stopping)
            .await
    }

    async fn resume_after_speakers(
        &mut self,
        app: &AppHandle,
        root: &Path,
        provider: &CaptureSelection,
        stage: PostProcessStage,
        claim: &mut Claim,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        let (cleanup, has_summary) = match stage {
            PostProcessStage::Speakers { .. } | PostProcessStage::SonioxSpeakers { .. } => {
                // Resume older/partially completed work without assuming the
                // prior native process reached the publication checkpoint.
                let media = Arc::new(self.media(root, claim, provider).await?);
                self.settle_video(app, &media, claim, stopping).await?;
                if stopping.load(Ordering::Acquire) {
                    return Err(WorkError::Stopping);
                }
                self.keep_claim(claim).await?;
                claim.lease = self
                    .client
                    .mark_post_process_ready(
                        &self.registered,
                        PostProcessReadyRequest {
                            proof: self.proof.clone(),
                            lease: claim.lease.clone(),
                            fence: claim.fence.clone(),
                            operation_id: Uuid::new_v4(),
                        },
                    )
                    .await?
                    .data
                    .lease;
                (false, false)
            }
            PostProcessStage::Ready {
                cleanup,
                generated_summary_revision,
                ..
            }
            | PostProcessStage::SonioxReady {
                cleanup,
                generated_summary_revision,
                ..
            } => (cleanup, generated_summary_revision.is_some()),
            _ => {
                return Err(WorkError::Deferred(
                    "the queued stage requires a different worker",
                ))
            }
        };
        let mut settings = app.state::<RuntimeState>().settings.read().clone();
        match self.supplement(app, root, claim, stopping).await? {
            super::supplement::Outcome::Published => return Ok(()),
            super::supplement::Outcome::Incomplete => {
                settings.cleanup_enabled = false;
                settings.summary_enabled = false;
            }
            super::supplement::Outcome::Unchanged => {}
        }
        let result = self
            .run_optional_stages(app, claim, &settings, cleanup, has_summary, stopping)
            .await;
        if result.is_err() && !stopping.load(Ordering::Acquire) {
            // The meeting is already Ready. Clear transient window progress
            // even if optional work remains queued for a later attempt.
            emit(app, claim.lease.meeting_id, "done", 1.0, None);
        }
        match result {
            Err(WorkError::Failed(_)) => Err(WorkError::Deferred(
                "an optional native stage stopped unexpectedly",
            )),
            other => other,
        }
    }

    async fn speaker_result(
        &mut self,
        app: &AppHandle,
        root: &Path,
        provider: &CaptureSelection,
        claim: &mut Claim,
        settings: &Settings,
        retained_live: bool,
        stopping: &AtomicBool,
    ) -> WorkResult<SpeakerResultBody> {
        let rows = self
            .read_transcript(claim, DesktopTranscriptRepresentation::Raw, stopping)
            .await?;
        if rows.is_empty() {
            return Ok(SpeakerResultBody::Skipped {
                reason: SpeakerSkipReason::EmptyTranscript,
            });
        }
        if !settings.diarize_enabled || (retained_live && rows.iter().any(|row| row.speaker.is_some())) {
            // Preserve identities already supplied by a participant track.
            return Ok(SpeakerResultBody::Skipped {
                reason: SpeakerSkipReason::Disabled,
            });
        }
        let media = self.media(root, claim, provider).await?;
        let app = app.clone();
        self.compute(claim, stopping, move || {
            let runtime = app.state::<RuntimeState>();
            let _busy = runtime.busy_guard();
            let spans: Vec<_> = rows.iter().map(|row| (row.start_ms, row.end_ms)).collect();
            let clusters = match crate::pipeline::service_speaker_clusters(&runtime, &media, &spans)
            {
                Ok(clusters) => clusters,
                Err(reason) => return Ok(SpeakerResultBody::Skipped { reason }),
            };
            if clusters.len() != rows.len() {
                return Err(WorkError::Deferred(
                    "the speaker engine returned an incomplete result",
                ));
            }
            let assignments = rows
                .into_iter()
                .zip(clusters)
                .map(|(row, cluster)| {
                    let cluster = cluster.map(u32::try_from).transpose().map_err(|_| {
                        WorkError::Deferred("the speaker engine returned an invalid cluster")
                    })?;
                    Ok(SpeakerAssignment {
                        segment_id: row.segment_id,
                        cluster,
                    })
                })
                .collect::<WorkResult<_>>()?;
            Ok(SpeakerResultBody::Applied { assignments })
        })
        .await
    }

    pub(super) async fn finish(
        &mut self,
        app: &AppHandle,
        claim: &mut Claim,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        if stopping.load(Ordering::Acquire) {
            return Err(WorkError::Stopping);
        }
        self.keep_claim(claim).await?;
        self.client
            .complete_fixture_post_process(
                &self.registered,
                CompleteFixturePostProcessRequest {
                    proof: self.proof.clone(),
                    lease: claim.lease.clone(),
                    fence: claim.fence.clone(),
                    operation_id: Uuid::new_v4(),
                },
            )
            .await?;
        emit(app, claim.lease.meeting_id, "done", 1.0, None);
        Ok(())
    }
}

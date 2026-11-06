//! Durable intent and visibility for one screen result per captured job.
use super::{postprocess_media::post_process_media_context_on, *};
use crate::Store;
use bazza_domain::capture_screen::{PostProcessVideoFailure, PostProcessVideoOutcome};
use chrono::{DateTime, Utc};
use rusqlite::{params, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::io::Read;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostProcessVideoContent {
    bytes: u64,
    digest: String,
}

impl PostProcessVideoContent {
    pub fn read(mut input: impl Read, bytes: u64, expires_at: DateTime<Utc>) -> DataResult<Self> {
        if !(1..=9_007_199_254_740_991).contains(&bytes) {
            return Err(DataError::Invalid("post-process video length"));
        }
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 256 * 1024];
        let mut remaining = bytes;
        while remaining > 0 {
            if expires_at <= Utc::now() {
                return Err(DataError::Forbidden);
            }
            let length = remaining.min(buffer.len() as u64) as usize;
            input.read_exact(&mut buffer[..length])?;
            hash.update(&buffer[..length]);
            remaining -= length as u64;
        }
        if input.read(&mut buffer[..1])? != 0 {
            return Err(DataError::Invalid("post-process video changed length"));
        }
        Ok(Self {
            bytes,
            digest: format!("{:x}", hash.finalize()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostProcessVideoRecord {
    Prepared {
        content: PostProcessVideoContent,
        claim_id: Uuid,
        owner_nonce: Uuid,
    },
    Published(PostProcessVideoContent, i64),
    Unavailable(PostProcessVideoFailure, i64),
}

fn reason_code(reason: PostProcessVideoFailure) -> &'static str {
    match reason {
        PostProcessVideoFailure::MissingRaw => "missing_raw",
        PostProcessVideoFailure::FfmpegUnavailable => "ffmpeg_unavailable",
        PostProcessVideoFailure::MuxFailed => "mux_failed",
    }
}

fn warning(reason: PostProcessVideoFailure) -> &'static str {
    match reason {
        PostProcessVideoFailure::MissingRaw => "video: screen capture produced no video file",
        PostProcessVideoFailure::FfmpegUnavailable => "video: FFmpeg is unavailable",
        PostProcessVideoFailure::MuxFailed => "video: screen recording could not be muxed",
    }
}

pub(super) fn record_on(
    conn: &Connection,
    lease: &PostProcessJobLease,
) -> DataResult<Option<PostProcessVideoRecord>> {
    let row = conn.query_row(
        "SELECT state,bytes,digest,reason,change_sequence,staging_claim,staging_owner FROM service_post_process_video
         WHERE job_id=?1 AND capture_id=?2",
        params![lease.job_id, lease.input.capture_id.to_string()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<i64>>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?)),
    ).optional()?;
    row.map(
        |(state, bytes, digest, reason, sequence, staging_claim, staging_owner)| match (
            state.as_str(),
            bytes,
            digest,
            reason.as_deref(),
            sequence,
        ) {
            ("prepared", Some(bytes), Some(digest), None, None) => {
                Ok(PostProcessVideoRecord::Prepared {
                    content: PostProcessVideoContent {
                        bytes: u64::try_from(bytes)
                            .map_err(|_| DataError::Invalid("post-process video length"))?,
                        digest,
                    },
                    claim_id: staging_uuid(staging_claim)?,
                    owner_nonce: staging_uuid(staging_owner)?,
                })
            }
            ("published", Some(bytes), Some(digest), None, Some(sequence)) => {
                Ok(PostProcessVideoRecord::Published(
                    PostProcessVideoContent {
                        bytes: u64::try_from(bytes)
                            .map_err(|_| DataError::Invalid("post-process video length"))?,
                        digest,
                    },
                    sequence,
                ))
            }
            ("unavailable", None, None, Some(reason), Some(sequence)) => {
                let reason = match reason {
                    "missing_raw" => PostProcessVideoFailure::MissingRaw,
                    "ffmpeg_unavailable" => PostProcessVideoFailure::FfmpegUnavailable,
                    "mux_failed" => PostProcessVideoFailure::MuxFailed,
                    _ => return Err(DataError::Invalid("post-process video reason")),
                };
                Ok(PostProcessVideoRecord::Unavailable(reason, sequence))
            }
            _ => Err(DataError::Invalid("post-process video state")),
        },
    )
    .transpose()
}

fn staging_uuid(value: Option<String>) -> DataResult<Uuid> {
    value
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
        .ok_or(DataError::Invalid("post-process video staging identity"))
}

fn admit(
    conn: &Connection,
    peer: u32,
    epoch: Uuid,
    request: &PostProcessMediaRequest,
) -> DataResult<Option<PostProcessVideoRecord>> {
    let context = post_process_media_context_on(conn, peer, epoch, request)?;
    if context.raw_screen.is_none() {
        return Err(DataError::Forbidden);
    }
    Ok(context.video_record)
}

fn project(
    tx: &Transaction<'_>,
    request: &PostProcessMediaRequest,
    outcome: PostProcessVideoOutcome,
) -> DataResult<i64> {
    let meeting = request.lease.meeting_id.to_string();
    let (has_video, diagnostic) = match outcome {
        PostProcessVideoOutcome::Published { .. } => (true, None),
        PostProcessVideoOutcome::Unavailable { reason } => (false, Some(warning(reason))),
    };
    if tx.execute(
        "UPDATE meetings SET has_video=?2,error=COALESCE(error,?3) WHERE id=?1 AND has_video=0",
        params![meeting, has_video, diagnostic],
    )? != 1
    {
        return Err(DataError::Forbidden);
    }
    consistency::change(
        tx,
        "meeting_media",
        &meeting,
        &revision(),
        &format!("worker:{}", request.proof.session_id),
    )
}

fn durable_video_write<T>(
    conn: &mut Connection,
    write: impl FnOnce(&Transaction<'_>) -> DataResult<T>,
) -> DataResult<T> {
    // Ordinary writes use WAL/NORMAL. Sync this small intent before installing
    // its file, and sync publication before replying, even after power loss.
    let previous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let result = (|| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = write(&tx)?;
        tx.commit()?;
        Ok(result)
    })();
    conn.pragma_update(None, "synchronous", previous)?;
    result
}

impl Store {
    pub fn service_post_process_video_record(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessMediaRequest,
    ) -> DataResult<Option<PostProcessVideoRecord>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        admit(&tx, peer, epoch, request)
    }

    /// Bind a verified staging fingerprint before any final name is created.
    /// Absence is checked with the media gate held and under this write lock.
    pub fn service_prepare_post_process_video(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessMediaRequest,
        content: &PostProcessVideoContent,
        require_no_final: impl FnOnce() -> DataResult<()>,
    ) -> DataResult<()> {
        durable_video_write(&mut self.conn.lock(), |tx| {
            match admit(tx, peer, epoch, request)? {
                None | Some(PostProcessVideoRecord::Prepared { .. }) => {}
                _ => return Err(DataError::Forbidden),
            }
            require_no_final()?;
            if tx.execute(
            "INSERT INTO service_post_process_video(job_id,capture_id,state,bytes,digest,staging_claim,staging_owner)
             VALUES(?1,?2,'prepared',?3,?4,?5,?6)
             ON CONFLICT(job_id) DO UPDATE SET bytes=excluded.bytes,digest=excluded.digest,
                 staging_claim=excluded.staging_claim,staging_owner=excluded.staging_owner
             WHERE capture_id=excluded.capture_id AND state='prepared'",
            params![request.lease.job_id, request.lease.input.capture_id.to_string(), content.bytes as i64,
                content.digest, request.lease.claim_id.to_string(), request.owner_nonce.to_string()],
        )? != 1 {
            return Err(DataError::Forbidden);
        }
            Ok(())
        })
    }

    /// Recheck the live claim and prepared content before the no-clobber install.
    /// An interruption after install leaves a durable intent for a later claim.
    pub fn service_commit_post_process_video(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessMediaRequest,
        content: &PostProcessVideoContent,
        install: impl FnOnce() -> DataResult<()>,
    ) -> DataResult<(PostProcessVideoOutcome, i64)> {
        durable_video_write(&mut self.conn.lock(), |tx| {
            let outcome = PostProcessVideoOutcome::Published {
                bytes: content.bytes,
            };
            match admit(tx, peer, epoch, request)? {
                Some(PostProcessVideoRecord::Published(saved, sequence)) if saved == *content => {
                    return Ok((outcome, sequence));
                }
                Some(PostProcessVideoRecord::Prepared { content: saved, .. })
                    if saved == *content => {}
                _ => return Err(DataError::Forbidden),
            }
            install()?;
            // A slow filesystem operation cannot grant an expired writer visibility.
            post_process_media_context_on(tx, peer, epoch, request)?;
            let sequence = project(tx, request, outcome)?;
            tx.execute(
                "UPDATE service_post_process_video SET state='published',change_sequence=?2,
                staging_claim=NULL,staging_owner=NULL WHERE job_id=?1",
                params![request.lease.job_id, sequence],
            )?;
            Ok((outcome, sequence))
        })
    }

    pub fn service_unavailable_post_process_video(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessMediaRequest,
        reason: PostProcessVideoFailure,
        require_no_final: impl FnOnce() -> DataResult<()>,
    ) -> DataResult<(PostProcessVideoOutcome, i64)> {
        durable_video_write(&mut self.conn.lock(), |tx| {
            let outcome = PostProcessVideoOutcome::Unavailable { reason };
            match admit(tx, peer, epoch, request)? {
                Some(PostProcessVideoRecord::Unavailable(saved, sequence)) if saved == reason => {
                    return Ok((outcome, sequence));
                }
                None => {}
                _ => return Err(DataError::Forbidden),
            }
            require_no_final()?;
            let sequence = project(tx, request, outcome)?;
            tx.execute(
            "INSERT INTO service_post_process_video(job_id,capture_id,state,reason,change_sequence)
             VALUES(?1,?2,'unavailable',?3,?4)",
            params![request.lease.job_id, request.lease.input.capture_id.to_string(), reason_code(reason), sequence],
        )?;
            Ok((outcome, sequence))
        })
    }
}

pub(super) fn screen_settled_on(
    conn: &Connection,
    lease: &PostProcessJobLease,
) -> DataResult<bool> {
    let sealed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM service_capture_terminal_screen WHERE capture_id=?1)",
        [lease.input.capture_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(!sealed
        || matches!(
            record_on(conn, lease)?,
            Some(PostProcessVideoRecord::Published(..) | PostProcessVideoRecord::Unavailable(..))
        ))
}

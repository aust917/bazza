use super::{Claim, WorkError, WorkResult, Worker};
use bazza_service_client::{
    api::{
        postprocess::PostProcessJobLease,
        postprocess_cleanup::{
            PostProcessCleanupOperation as Operation, PostProcessCleanupReply,
            PostProcessCleanupRequest, PostProcessCleanupStatus, PostProcessCleanupUploadState,
            MAX_POST_PROCESS_CLEANUP_BODY_BYTES,
        },
        postprocess_source::PostProcessSourceReply,
        postprocess_speakers::PostProcessSpeakersReply,
        postprocess_summary::{PostProcessSummaryReply, MAX_WORKER_SUMMARY_BODY_BYTES},
    },
    ClientError,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

// JSON escapes can expand each body byte up to sixfold. Leave space for the
// serialized lease and operation metadata inside the 32 KiB request budget.
const CHUNK_BYTES: usize = 4 * 1024;

pub(super) enum UploadKind {
    Source,
    Speakers,
    Cleanup,
    Summary,
}

enum Reply {
    Progress(PostProcessCleanupStatus),
    Committed(Option<PostProcessJobLease>),
}

impl Worker {
    pub(super) async fn upload(
        &mut self,
        kind: UploadKind,
        claim: &mut Claim,
        body: &impl serde::Serialize,
        stopping: &AtomicBool,
    ) -> WorkResult<()> {
        let body = serde_json::to_string(body)
            .map_err(|_| WorkError::Deferred("post-process result could not be encoded"))?;
        let budget = match kind {
            UploadKind::Summary => MAX_WORKER_SUMMARY_BODY_BYTES,
            _ => MAX_POST_PROCESS_CLEANUP_BODY_BYTES,
        };
        if body.len() as u64 > budget {
            return Err(WorkError::Deferred(
                "post-process result exceeds the upload limit",
            ));
        }
        let upload_id = Uuid::new_v4();
        let begin = Operation::Begin {
            upload_id,
            total_bytes: body.len() as u64,
            sha256: sha256(&body),
        };
        let mut reply = self.upload_operation(&kind, claim, begin, stopping).await?;
        let mut offset = 0;
        let mut sequence = 1;
        while offset < body.len() {
            verify_progress(reply, offset, sequence)?;
            let end = next_boundary(&body, offset);
            let text = &body[offset..end];
            reply = self
                .upload_operation(
                    &kind,
                    claim,
                    Operation::Append {
                        upload_id,
                        sequence,
                        byte_offset: offset as u64,
                        text: text.to_owned(),
                        sha256: sha256(text),
                    },
                    stopping,
                )
                .await?;
            offset = end;
            sequence += 1;
        }
        verify_progress(reply, offset, sequence)?;
        match self
            .upload_operation(&kind, claim, Operation::Commit { upload_id }, stopping)
            .await?
        {
            Reply::Committed(Some(lease)) => claim.lease = lease,
            Reply::Committed(None) if matches!(kind, UploadKind::Summary) => {}
            _ => return Err(ClientError::Protocol.into()),
        }
        Ok(())
    }

    async fn upload_operation(
        &mut self,
        kind: &UploadKind,
        claim: &mut Claim,
        operation: Operation,
        stopping: &AtomicBool,
    ) -> WorkResult<Reply> {
        if stopping.load(Ordering::Acquire) {
            return Err(WorkError::Stopping);
        }
        self.keep_claim(claim).await?;
        let request = PostProcessCleanupRequest {
            proof: self.proof.clone(),
            lease: claim.lease.clone(),
            fence: claim.fence.clone(),
            operation_id: Uuid::new_v4(),
            operation,
        };
        let reply = match kind {
            UploadKind::Source => match self
                .client
                .post_process_source(&self.registered, request)
                .await?
                .data
            {
                PostProcessSourceReply::Upload(status) => Reply::Progress(status),
                PostProcessSourceReply::Committed(receipt) => Reply::Committed(Some(receipt.lease)),
            },
            UploadKind::Speakers => match self
                .client
                .post_process_speakers(&self.registered, request)
                .await?
                .data
            {
                PostProcessSpeakersReply::Upload(status) => Reply::Progress(status),
                PostProcessSpeakersReply::Committed(receipt) => {
                    Reply::Committed(Some(receipt.lease))
                }
            },
            UploadKind::Cleanup => match self
                .client
                .post_process_cleanup(&self.registered, request)
                .await?
                .data
            {
                PostProcessCleanupReply::Upload(status) => Reply::Progress(status),
                PostProcessCleanupReply::Committed(receipt) => {
                    Reply::Committed(Some(receipt.lease))
                }
            },
            UploadKind::Summary => match self
                .client
                .post_process_summary(&self.registered, request)
                .await?
                .data
            {
                PostProcessSummaryReply::Upload(status) => Reply::Progress(status),
                PostProcessSummaryReply::Committed(_) => Reply::Committed(None),
            },
        };
        Ok(reply)
    }
}

fn verify_progress(reply: Reply, offset: usize, sequence: u64) -> WorkResult<()> {
    match reply {
        Reply::Progress(status)
            if status.state == PostProcessCleanupUploadState::Open
                && status.received_bytes == offset as u64
                && status.next_sequence == sequence =>
        {
            Ok(())
        }
        _ => Err(ClientError::Protocol.into()),
    }
}

fn next_boundary(body: &str, offset: usize) -> usize {
    let mut end = (offset + CHUNK_BYTES).min(body.len());
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn sha256(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_chunks_keep_the_exact_unicode_body() {
        let body = "🫱🏼‍🫲🏻 встреча\\\"\n".repeat(CHUNK_BYTES);
        let mut offset = 0;
        let mut reassembled = String::new();
        while offset < body.len() {
            let next = next_boundary(&body, offset);
            assert!(next > offset && next - offset <= CHUNK_BYTES);
            reassembled.push_str(&body[offset..next]);
            offset = next;
        }
        assert_eq!(reassembled, body);
    }
}

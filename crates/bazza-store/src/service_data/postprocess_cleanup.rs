//! Staging for closed worker results. Upload bytes are invisible until commit.
include!("postprocess_cleanup_types.rs");
use crate::Store;
use rusqlite::{params, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[path = "postprocess_source.rs"]
mod source;
pub use source::*;
#[path = "postprocess_speakers.rs"]
mod speakers;
pub use speakers::*;
#[path = "postprocess_summary.rs"]
mod summary;
pub use summary::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum UploadKind {
    Cleanup,
    LocalSource,
    Speakers,
    GeneratedSummary,
}

impl UploadKind {
    fn name(self) -> &'static str {
        match self {
            Self::Cleanup => "cleanup",
            Self::LocalSource => "local_source",
            Self::Speakers => "speakers",
            Self::GeneratedSummary => "generated_summary",
        }
    }

    fn max_body_bytes(self) -> u64 {
        match self {
            Self::GeneratedSummary => bazza_domain::worker_summary::MAX_WORKER_SUMMARY_BODY_BYTES,
            _ => MAX_POST_PROCESS_CLEANUP_BODY_BYTES,
        }
    }
}

trait StagedReply: Serialize + DeserializeOwned {
    const KIND: UploadKind;
    const TOOL: &'static str;

    fn upload(status: PostProcessCleanupStatus) -> Self;
    fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool;
    fn commit(
        tx: &Transaction<'_>,
        request: &PostProcessCleanupRequest,
        upload: &Upload,
        token: &str,
    ) -> DataResult<Self>;
}

struct Upload {
    status: PostProcessCleanupStatus,
    kind: UploadKind,
    input: PostProcessSourceFence,
    fence_hash: String,
    intent_hash: String,
    body_hash: String,
}
struct UploadRow {
    status: PostProcessCleanupStatus,
    kind: UploadKind,
    total: i64,
    received: i64,
    next: i64,
    input: String,
    fence_hash: String,
    intent_hash: String,
    body_hash: String,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn count(value: i64) -> DataResult<u64> {
    u64::try_from(value).map_err(|_| DataError::Invalid("negative cleanup count"))
}
fn number(value: u64) -> DataResult<i64> {
    i64::try_from(value).map_err(|_| DataError::Invalid("cleanup SQL count"))
}
fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)
}
fn upload_on(conn: &Connection, id: Uuid) -> DataResult<Upload> {
    conn.query_row(
        "SELECT CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END,CASE WHEN length(CAST(claim_id AS BLOB))<=128 THEN claim_id END,CASE WHEN length(CAST(worker_session_id AS BLOB))<=128 THEN worker_session_id END,CASE WHEN length(CAST(library_epoch AS BLOB))<=128 THEN library_epoch END,CASE WHEN length(CAST(job_id AS BLOB))<=128 THEN job_id END,CASE WHEN length(CAST(meeting_id AS BLOB))<=128 THEN meeting_id END,CASE WHEN length(CAST(checkpoint_revision AS BLOB))<=128 THEN checkpoint_revision END,state,total_bytes,received_bytes,next_sequence,CASE WHEN length(CAST(expires_at AS BLOB))<=128 THEN expires_at END,CASE WHEN length(CAST(input_json AS BLOB))<=4096 THEN input_json END,CASE WHEN length(CAST(fence_hash AS BLOB))<=64 THEN fence_hash END,CASE WHEN length(CAST(intent_hash AS BLOB))<=64 THEN intent_hash END,CASE WHEN length(CAST(sha256 AS BLOB))<=64 THEN sha256 END,upload_kind FROM service_post_process_cleanup_uploads WHERE id=?1",
        [id.to_string()],
        |row| {
            let state:String=row.get(7)?;
            let state = match state.as_str() {
                "open"=>PostProcessCleanupUploadState::Open,"committed"=>PostProcessCleanupUploadState::Committed,
                "aborted"=>PostProcessCleanupUploadState::Aborted,"expired"=>PostProcessCleanupUploadState::Expired,
                "invalidated"=>PostProcessCleanupUploadState::Invalidated,_=>return Err(rusqlite::Error::InvalidQuery),
            };
            let kind = match row.get::<_, String>(16)?.as_str() {
                "cleanup" => UploadKind::Cleanup,
                "local_source" => UploadKind::LocalSource,
                "speakers" => UploadKind::Speakers,
                "generated_summary" => UploadKind::GeneratedSummary,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(UploadRow {kind,status:PostProcessCleanupStatus {
                upload_id:parse_uuid(row.get(0)?)?,claim_id:parse_uuid(row.get(1)?)?,worker_session_id:parse_uuid(row.get(2)?)?,library_epoch:parse_uuid(row.get(3)?)?,job_id:row.get(4)?,meeting_id:parse_uuid(row.get(5)?)?,checkpoint_revision:row.get(6)?,state,total_bytes:0,received_bytes:0,next_sequence:0,expires_at:row.get(11)?,
            },total:row.get(8)?,received:row.get(9)?,next:row.get(10)?,input:row.get(12)?,fence_hash:row.get(13)?,intent_hash:row.get(14)?,body_hash:row.get(15)?})
        },
    ).optional()?.ok_or(DataError::NotFound).and_then(|row| {
        let mut status=row.status;
        status.total_bytes=count(row.total)?; status.received_bytes=count(row.received)?; status.next_sequence=count(row.next)?;
        let input:PostProcessSourceFence=serde_json::from_str(&row.input)?;
        if !(1..=row.kind.max_body_bytes()).contains(&status.total_bytes)
            || status.received_bytes>status.total_bytes
            || !(1..=MAX_POST_PROCESS_CLEANUP_CHUNKS+1).contains(&status.next_sequence)
            || status.next_sequence-1>status.received_bytes
            || (status.received_bytes==0)!=(status.next_sequence==1)
            || status.expires_at.timestamp_millis()<=0
            || status.expires_at>Utc::now()+Duration::seconds(POST_PROCESS_CLEANUP_TTL_SECONDS+5)
            || [status.upload_id,status.claim_id,status.worker_session_id,status.library_epoch,status.meeting_id,input.capture_id,input.capture_generation].iter().any(Uuid::is_nil)
            || safe_id(&status.job_id).is_err() || !desktop_revision(&status.checkpoint_revision)
            || status.checkpoint_revision!=input.checkpoint_revision
            || !desktop_revision(&input.lifecycle_revision) || !desktop_revision(&input.transcript_revision)
            || input.source_id.as_ref().is_some_and(|id|safe_id(id).is_err())
            || !sha256(&row.fence_hash) || !sha256(&row.intent_hash) || !sha256(&row.body_hash)
        {return Err(DataError::Invalid("cleanup stored upload"));}
        Ok(Upload {status,kind:row.kind,input,fence_hash:row.fence_hash,intent_hash:row.intent_hash,body_hash:row.body_hash})
    })
}
fn matches_owner(
    upload: &Upload,
    request: &PostProcessCleanupRequest,
    kind: UploadKind,
) -> DataResult<()> {
    if upload.kind != kind
        || upload.status.worker_session_id != request.proof.session_id
        || upload.status.library_epoch != request.proof.library_epoch
        || upload.status.claim_id != request.lease.claim_id
        || upload.status.job_id != request.lease.job_id
        || upload.status.meeting_id != request.lease.meeting_id
        || upload.status.checkpoint_revision != request.lease.input.checkpoint_revision
        || upload.input != request.lease.input
        || upload.fence_hash != request.fence.hash()?
    {
        return Err(DataError::Forbidden);
    }
    Ok(())
}
fn retire(tx: &Transaction<'_>, id: Uuid, state: &str) -> DataResult<()> {
    tx.execute(
        "UPDATE service_post_process_cleanup_chunks SET text=NULL WHERE upload_id=?1",
        [id.to_string()],
    )?;
    tx.execute(
        "UPDATE service_post_process_cleanup_uploads SET state=?2 WHERE id=?1",
        params![id.to_string(), state],
    )?;
    Ok(())
}
fn expire_on(tx: &Transaction<'_>, upload: Upload) -> DataResult<Upload> {
    if upload.status.state == PostProcessCleanupUploadState::Open
        && upload.status.expires_at <= Utc::now()
    {
        retire(tx, upload.status.upload_id, "expired")?;
        return upload_on(tx, upload.status.upload_id);
    }
    Ok(upload)
}
fn binding_on(tx: &Transaction<'_>, request: &PostProcessCleanupRequest) -> DataResult<String> {
    postprocess::lease_valid(&request.lease, &request.proof)?;
    let token = postprocess::check_binding(tx, &request.proof, &request.lease, &request.fence)?;
    let until: DateTime<Utc> = tx.query_row(
        "SELECT lease_until FROM service_jobs WHERE id=?1",
        [&request.lease.job_id],
        |row| row.get(0),
    )?;
    if until != request.lease.expires_at || until <= Utc::now() {
        return Err(DataError::Forbidden);
    }
    Ok(token)
}
fn check_phase(
    tx: &Transaction<'_>,
    request: &PostProcessCleanupRequest,
    kind: UploadKind,
) -> DataResult<()> {
    let phase:String=tx.query_row("SELECT phase FROM service_post_process_checkpoints WHERE job_id=?1 AND revision=?2 AND state='active'",params![request.lease.job_id,request.lease.input.checkpoint_revision],|row|row.get(0)).optional()?.ok_or(DataError::Forbidden)?;
    let retained_live = || {
        postprocess::retained_live_source_on(
            tx,
            request.lease.meeting_id,
            request.lease.input.source_id.as_deref(),
        )
    };
    let allowed = match kind {
        UploadKind::Cleanup => matches!(phase.as_str(), "captured" | "local_asr" | "ready"),
        UploadKind::Speakers => {
            request.lease.input.source_id.is_some()
                && (phase == "local_asr"
                    || (phase == "captured"
                        && (postprocess::capture_context_on(tx, &request.lease.job_id)?
                            .provider.provider == CaptureProvider::Soniox || retained_live()?)))
        }
        UploadKind::GeneratedSummary => {
            phase == "ready_cleanup"
                && request.lease.input.source_id.is_some()
                && !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM service_post_process_generated_summaries WHERE job_id=?1)",
                    [&request.lease.job_id],
                    |row| row.get::<_, bool>(0),
                )?
        }
        UploadKind::LocalSource => {
            phase == "captured"
                && !retained_live()?
                && postprocess::capture_context_on(tx, &request.lease.job_id)?
                    .provider
                    .provider
                    == CaptureProvider::Local
        }
    };
    if !allowed {
        return Err(DataError::Forbidden);
    }
    Ok(())
}
fn begin_on(
    tx: &Transaction<'_>,
    request: &PostProcessCleanupRequest,
    id: Uuid,
    total: u64,
    body_hash: &str,
    kind: UploadKind,
) -> DataResult<PostProcessCleanupStatus> {
    if total > kind.max_body_bytes() {
        return Err(DataError::Invalid("worker upload byte budget"));
    }
    let intent = postprocess::fingerprint(
        &(
            request.proof.session_id,
            request.proof.library_epoch,
            request.lease.claim_id,
            &request.lease.job_id,
            request.lease.meeting_id,
            &request.lease.input,
            request.fence.hash()?,
            total,
            body_hash,
        ),
        8192,
    )?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM service_post_process_cleanup_uploads WHERE id=?1)",
        [id.to_string()],
        |row| row.get(0),
    )?;
    if exists {
        let upload = expire_on(tx, upload_on(tx, id)?)?;
        matches_owner(&upload, request, kind)?;
        if upload.intent_hash != intent {
            return Err(DataError::IdempotencyConflict);
        }
        return Ok(upload.status);
    }
    check_phase(tx, request, kind)?;
    let (open,reserved):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(total_bytes),0) FROM service_post_process_cleanup_uploads WHERE state='open'",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
    if open >= i64::from(MAX_POST_PROCESS_CLEANUP_OPEN_UPLOADS)
        || count(reserved)?
            .checked_add(total)
            .is_none_or(|value| value > MAX_POST_PROCESS_CLEANUP_RESERVED_BYTES)
    {
        return Err(DataError::Busy);
    }
    tx.execute("INSERT INTO service_post_process_cleanup_uploads(id,worker_session_id,library_epoch,claim_id,job_id,meeting_id,checkpoint_revision,input_json,fence_hash,intent_hash,total_bytes,sha256,state,expires_at,upload_kind) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'open',?13,?14)",params![id.to_string(),request.proof.session_id.to_string(),request.proof.library_epoch.to_string(),request.lease.claim_id.to_string(),request.lease.job_id,request.lease.meeting_id.to_string(),request.lease.input.checkpoint_revision,serde_json::to_string(&request.lease.input)?,request.fence.hash()?,intent,number(total)?,body_hash,Utc::now()+Duration::seconds(POST_PROCESS_CLEANUP_TTL_SECONDS),kind.name()])?;
    Ok(upload_on(tx, id)?.status)
}
fn append_on(
    tx: &Transaction<'_>,
    request: &PostProcessCleanupRequest,
    sequence: u64,
    offset: u64,
    text: &str,
    hash: &str,
    kind: UploadKind,
) -> DataResult<PostProcessCleanupStatus> {
    let id = request.operation.upload_id();
    if sha(text.as_bytes()) != hash {
        return Err(DataError::Invalid("cleanup chunk digest"));
    }
    let upload = expire_on(tx, upload_on(tx, id)?)?;
    matches_owner(&upload, request, kind)?;
    let saved:Option<(i64,i64,String)>=tx.query_row("SELECT byte_offset,bytes,sha256 FROM service_post_process_cleanup_chunks WHERE upload_id=?1 AND sequence=?2",params![id.to_string(),number(sequence)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
    if let Some((saved_offset, bytes, saved_hash)) = saved {
        if count(saved_offset)? != offset
            || count(bytes)? != text.len() as u64
            || saved_hash != hash
        {
            return Err(DataError::IdempotencyConflict);
        }
        if upload.status.state == PostProcessCleanupUploadState::Open {
            check_phase(tx, request, kind)?;
        }
        return Ok(upload.status);
    }
    if upload.status.state != PostProcessCleanupUploadState::Open {
        return Ok(upload.status);
    }
    check_phase(tx, request, kind)?;
    if sequence != upload.status.next_sequence
        || offset != upload.status.received_bytes
        || offset
            .checked_add(text.len() as u64)
            .is_none_or(|value| value > upload.status.total_bytes)
    {
        return Err(DataError::Invalid("cleanup chunk order or length"));
    }
    tx.execute("INSERT INTO service_post_process_cleanup_chunks(upload_id,sequence,byte_offset,bytes,sha256,text) VALUES(?1,?2,?3,?4,?5,?6)",params![id.to_string(),number(sequence)?,number(offset)?,number(text.len() as u64)?,hash,text])?;
    tx.execute("UPDATE service_post_process_cleanup_uploads SET received_bytes=?2,next_sequence=?3 WHERE id=?1",params![id.to_string(),number(offset+text.len() as u64)?,number(sequence+1)?])?;
    Ok(upload_on(tx, id)?.status)
}

fn body_on(tx: &Transaction<'_>, upload: &Upload) -> DataResult<Vec<u8>> {
    if upload.status.received_bytes != upload.status.total_bytes {
        return Err(DataError::Invalid("cleanup upload incomplete"));
    }
    let mut body = Vec::with_capacity(upload.status.total_bytes as usize);
    let mut next = 1_u64;
    let mut statement=tx.prepare("SELECT sequence,byte_offset,bytes,sha256,CASE WHEN length(CAST(text AS BLOB))<=32768 THEN text END FROM service_post_process_cleanup_chunks WHERE upload_id=?1 ORDER BY sequence LIMIT 4097")?;
    let rows = statement.query_map([upload.status.upload_id.to_string()], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (sequence, offset, bytes, hash, text) = row?;
        let text = text.ok_or(DataError::Invalid("cleanup stored chunk"))?;
        if next > MAX_POST_PROCESS_CLEANUP_CHUNKS
            || count(sequence)? != next
            || count(offset)? != body.len() as u64
            || count(bytes)? != text.len() as u64
            || sha(text.as_bytes()) != hash
            || body.len() + text.len() > upload.status.total_bytes as usize
        {
            return Err(DataError::Invalid("cleanup stored chunk integrity"));
        }
        body.extend_from_slice(text.as_bytes());
        next += 1;
    }
    if body.len() as u64 != upload.status.total_bytes
        || next != upload.status.next_sequence
        || sha(&body) != upload.body_hash
    {
        return Err(DataError::Invalid("cleanup body digest"));
    }
    Ok(body)
}

fn decode_cleanup(body: &[u8]) -> DataResult<Vec<PostProcessCleanupUpdate>> {
    let mut decoder = serde_json::Deserializer::from_slice(body);
    let updates = serde::de::Deserializer::deserialize_seq(&mut decoder, UpdatesVisitor)
        .map_err(|_| DataError::Invalid("cleanup body"))?;
    decoder
        .end()
        .map_err(|_| DataError::Invalid("cleanup body"))?;
    if !post_process_cleanup_body_valid(&updates) {
        return Err(DataError::Invalid("cleanup body shape"));
    }
    Ok(updates)
}

struct UpdatesVisitor;
impl<'de> serde::de::Visitor<'de> for UpdatesVisitor {
    type Value = Vec<PostProcessCleanupUpdate>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded cleanup updates")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut sequence: A,
    ) -> Result<Self::Value, A::Error> {
        let mut updates = Vec::with_capacity(
            sequence
                .size_hint()
                .unwrap_or(0)
                .min(MAX_POST_PROCESS_CLEANUP_UPDATES),
        );
        loop {
            if updates.len() == MAX_POST_PROCESS_CLEANUP_UPDATES {
                if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom("cleanup update count"));
                }
                break;
            }
            match sequence.next_element()? {
                Some(update) => updates.push(update),
                None => break,
            }
        }
        Ok(updates)
    }
}

fn commit_on<R: StagedReply>(
    tx: &Transaction<'_>,
    request: &PostProcessCleanupRequest,
    id: Uuid,
    token: &str,
) -> DataResult<R> {
    let upload = expire_on(tx, upload_on(tx, id)?)?;
    matches_owner(&upload, request, R::KIND)?;
    if upload.status.state != PostProcessCleanupUploadState::Open {
        return Ok(R::upload(upload.status));
    }
    check_phase(tx, request, R::KIND)?;
    let reply = R::commit(tx, request, &upload, token)?;
    // Either result fences the entire old checkpoint, across upload kinds.
    tx.execute("UPDATE service_post_process_cleanup_chunks SET text=NULL WHERE upload_id IN (SELECT id FROM service_post_process_cleanup_uploads WHERE job_id=?1 AND checkpoint_revision=?2 AND state='open' AND id<>?3)",params![request.lease.job_id,request.lease.input.checkpoint_revision,id.to_string()])?;
    tx.execute("UPDATE service_post_process_cleanup_uploads SET state='invalidated' WHERE job_id=?1 AND checkpoint_revision=?2 AND state='open' AND id<>?3",params![request.lease.job_id,request.lease.input.checkpoint_revision,id.to_string()])?;
    retire(tx, id, "committed")?;
    Ok(reply)
}

fn apply_cleanup(
    tx: &Transaction<'_>,
    request: &PostProcessCleanupRequest,
    upload: &Upload,
    token: &str,
) -> DataResult<PostProcessCleanupReceipt> {
    let updates = decode_cleanup(&body_on(tx, upload)?)?;
    let final_values: BTreeMap<i64, String> = updates
        .into_iter()
        .map(|value| (value.segment_id, value.text))
        .collect();
    let mut changed = 0_u32;
    let source = request.lease.input.source_id.as_ref();
    for (segment, text) in final_values {
        let saved:Option<(bool,bool,bool)>=tx.query_row("SELECT (s.clean_text IS ?3) AND EXISTS(SELECT 1 FROM service_transcript_overlays o WHERE o.source_id=?4 AND o.segment_key=CAST(s.id AS TEXT) AND o.manual=0 AND o.clean_text IS ?3),s.clean_text_manual,EXISTS(SELECT 1 FROM service_transcript_overlays o WHERE o.source_id=?4 AND o.segment_key=CAST(s.id AS TEXT) AND o.manual=1) FROM segments s WHERE s.meeting_id=?1 AND s.id=?2",params![request.lease.meeting_id.to_string(),segment,text,source],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        let Some((unchanged, manual, canonical_manual)) = saved else {
            continue;
        };
        if manual || canonical_manual || unchanged {
            continue;
        }
        if tx.execute("UPDATE segments SET clean_text=?3 WHERE meeting_id=?1 AND id=?2 AND clean_text_manual=0",params![request.lease.meeting_id.to_string(),segment,text])?!=1 {return Err(DataError::Forbidden);}
        let source = source.ok_or(DataError::Forbidden)?;
        tx.execute("INSERT INTO service_transcript_overlays(source_id,segment_key,clean_text,manual) VALUES(?1,?2,?3,0) ON CONFLICT(source_id,segment_key) DO UPDATE SET clean_text=excluded.clean_text,manual=0 WHERE service_transcript_overlays.manual=0",params![source,segment.to_string(),text])?;
        changed += 1;
    }
    let transcript = if changed > 0 {
        let rev = revision();
        if tx.execute(
            "UPDATE service_transcript_state SET revision=?2 WHERE meeting_id=?1 AND revision=?3",
            params![
                request.lease.meeting_id.to_string(),
                rev,
                request.lease.input.transcript_revision
            ],
        )? != 1
        {
            return Err(DataError::Forbidden);
        }
        crate::ai::rebuild_chunks_on(tx, &request.lease.meeting_id.to_string())?;
        search::rebuild_meeting_on(tx, &request.lease.meeting_id.to_string())?;
        consistency::change(
            tx,
            "transcript",
            &request.lease.meeting_id.to_string(),
            &rev,
            &format!("worker:{}", request.proof.session_id),
        )?;
        rev
    } else {
        request.lease.input.transcript_revision.clone()
    };
    let lease = postprocess::advance_result_on(
        tx,
        &request.proof,
        &request.lease,
        token,
        postprocess::InstalledResult::Cleanup {
            transcript_revision: &transcript,
        },
        request.operation_id,
    )?;
    Ok(PostProcessCleanupReceipt {
        operation_id: request.operation_id,
        upload_id: upload.status.upload_id,
        lease,
        changed_segments: changed,
        outcome: if changed == 0 {
            PostProcessCleanupOutcome::Noop
        } else {
            PostProcessCleanupOutcome::Applied
        },
    })
}

impl StagedReply for PostProcessCleanupReply {
    const KIND: UploadKind = UploadKind::Cleanup;
    const TOOL: &'static str = "post_process_cleanup";

    fn upload(status: PostProcessCleanupStatus) -> Self {
        Self::Upload(status)
    }

    fn valid_for(&self, request: &PostProcessCleanupRequest) -> bool {
        self.valid_for(request)
    }

    fn commit(
        tx: &Transaction<'_>,
        request: &PostProcessCleanupRequest,
        upload: &Upload,
        token: &str,
    ) -> DataResult<Self> {
        let receipt = apply_cleanup(tx, request, upload, token)?;
        tx.execute(
            "UPDATE service_post_process_cleanup_uploads SET receipt_json=?2 WHERE id=?1",
            params![
                upload.status.upload_id.to_string(),
                serde_json::to_string(&receipt)?
            ],
        )?;
        Ok(Self::Committed(receipt))
    }
}

impl Store {
    pub fn service_post_process_cleanup(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessCleanupRequest,
    ) -> DataResult<PostProcessCleanupReply> {
        self.service_post_process_upload(peer, epoch, request)
    }

    fn service_post_process_upload<R: StagedReply>(
        &self,
        peer: u32,
        epoch: Uuid,
        request: &PostProcessCleanupRequest,
    ) -> DataResult<R> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        postprocess::worker_on(&tx, peer, epoch, &request.proof)?;
        if !request.valid() {
            return Err(DataError::Invalid("post-process cleanup request"));
        }
        request.fence.validate()?;
        let hash = postprocess::fingerprint(request, MAX_POST_PROCESS_CLEANUP_REQUEST_BYTES)?;
        let principal = format!("worker:{}", request.proof.session_id);
        let key = request.operation_id.to_string();
        let saved:Option<(String,Option<String>,DateTime<Utc>)>=tx.query_row("SELECT arguments_hash,CASE WHEN length(CAST(outcome_json AS BLOB))<=65024 THEN outcome_json END,expires_at FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",params![principal,R::TOOL,key],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        if let Some((saved_hash, body, until)) = saved {
            if until > Utc::now() {
                if saved_hash != hash {
                    return Err(DataError::IdempotencyConflict);
                }
                let reply: R = serde_json::from_str(
                    body.as_deref()
                        .ok_or(DataError::Invalid("cleanup receipt budget"))?,
                )?;
                if !reply.valid_for(request) {
                    return Err(DataError::Invalid("cleanup cached receipt"));
                }
                return Ok(reply);
            }
            tx.execute(
                "DELETE FROM service_idempotency WHERE principal=?1 AND tool=?2 AND key=?3",
                params![principal, R::TOOL, key],
            )?;
        }
        let token = binding_on(&tx, request)?;
        let reply = match &request.operation {
            PostProcessCleanupOperation::Begin {
                upload_id,
                total_bytes,
                sha256,
            } => R::upload(begin_on(
                &tx,
                request,
                *upload_id,
                *total_bytes,
                sha256,
                R::KIND,
            )?),
            PostProcessCleanupOperation::Append {
                sequence,
                byte_offset,
                text,
                sha256,
                ..
            } => R::upload(append_on(
                &tx,
                request,
                *sequence,
                *byte_offset,
                text,
                sha256,
                R::KIND,
            )?),
            PostProcessCleanupOperation::Commit { upload_id } => {
                let result = commit_on::<R>(&tx, request, *upload_id, &token)?;
                // A large commit can outlast worker registration. Do not
                // publish staged results under an expired registration.
                postprocess::worker_on(&tx, peer, epoch, &request.proof)?;
                result
            }
            PostProcessCleanupOperation::Status { upload_id }
            | PostProcessCleanupOperation::Abort { upload_id } => {
                let upload = expire_on(&tx, upload_on(&tx, *upload_id)?)?;
                matches_owner(&upload, request, R::KIND)?;
                if matches!(request.operation, PostProcessCleanupOperation::Abort { .. })
                    && upload.status.state == PostProcessCleanupUploadState::Open
                {
                    retire(&tx, *upload_id, "aborted")?;
                }
                R::upload(upload_on(&tx, *upload_id)?.status)
            }
        };
        if !reply.valid_for(request) {
            return Err(DataError::Invalid("cleanup reply"));
        }
        let until = Utc::now() + Duration::seconds(consistency::DEDUP_RETENTION_SECONDS);
        tx.execute("INSERT INTO service_idempotency(principal,tool,key,arguments_hash,outcome_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",params![principal,R::TOOL,key,hash,serde_json::to_string(&reply)?,until])?;
        tx.commit()?;
        Ok(reply)
    }

    pub fn service_expire_post_process_cleanup_uploads(&self, limit: usize) -> DataResult<usize> {
        if !(1..=100).contains(&limit) {
            return Err(DataError::Invalid("cleanup expiry limit"));
        }
        let mut conn = self.conn.lock();
        let now = Utc::now();
        let due:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM service_post_process_cleanup_uploads WHERE state='open' AND expires_at<=?1)",[now],|row|row.get(0))?;
        if !due {
            return Ok(0);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut st=tx.prepare("SELECT id FROM service_post_process_cleanup_uploads WHERE state='open' AND expires_at<=?1 ORDER BY expires_at,id LIMIT ?2")?;
            let ids = st
                .query_map(params![now, limit as i64], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        for id in &ids {
            retire(
                &tx,
                Uuid::parse_str(id).map_err(|_| DataError::Invalid("cleanup stored upload"))?,
                "expired",
            )?;
        }
        tx.commit()?;
        Ok(ids.len())
    }
}

pub(super) fn invalidate_on(tx: &Transaction<'_>, meeting_id: &str) -> DataResult<()> {
    tx.execute("UPDATE service_post_process_cleanup_chunks SET text=NULL WHERE upload_id IN (SELECT id FROM service_post_process_cleanup_uploads WHERE meeting_id=?1 AND state='open')",[meeting_id])?;
    tx.execute("UPDATE service_post_process_cleanup_uploads SET state='invalidated' WHERE meeting_id=?1 AND state='open'",[meeting_id])?;
    Ok(())
}

#[cfg(test)]
#[path = "postprocess_cleanup_tests.rs"]
mod tests;
